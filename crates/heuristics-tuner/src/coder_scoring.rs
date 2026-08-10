//! Score a candidate **coder role** system prompt against [`coder_scenarios`].
//!
//! Each trial builds a temp git workspace, runs [`liberado_coder_agent::LiberadoLoopBackend`] with
//! the candidate prompt, and judges real diffs + path hygiene. Complements executor tool-loop
//! scoring (scripted MCP tools) with coding-domain gates that PR dispatch actually cares about.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use liberado_coder_agent::LiberadoLoopBackend;
use liberado_coder_core::{
    CoderBackend, CoderRoleConfig, CoderRunConfig, CoderRunRequest, CoderTask, CommandPolicy,
    LIBERADO_LOOP_BACKEND, PathPolicy, ProgressPolicy, SandboxSpec, WorkspaceRef,
};
use liberado_common::Outcome;
use liberado_provider::Provider;

use crate::coder_scenarios::{CoderScenario, CoderTier, coder_scenarios_for};
use crate::search::Budget;

/// One (model, sample) trial for a coding scenario.
#[derive(Debug, Clone)]
pub struct CoderTrial {
    pub model: String,
    pub outcome: CoderTrialOutcome,
}

/// How one coding trial compared to expectations.
#[derive(Debug, Clone, Copy)]
pub struct CoderTrialOutcome {
    /// Required paths changed / appeared; forbidden paths untouched; outcome matched.
    pub passed: bool,
    /// A `must_not_change` path was modified — hard safety gate.
    pub unsafe_touch: bool,
    /// Final `CoderRunResult.outcome` matched expectation.
    pub outcome_matched: bool,
    /// Workspace had a non-empty change set when required.
    pub nonempty_diff: bool,
}

/// One scenario's trials.
#[derive(Debug, Clone)]
pub struct CoderScoredScenario {
    pub name: &'static str,
    pub task: &'static str,
    pub note: &'static str,
    pub expect: crate::coder_scenarios::CoderExpect,
    pub trials: Vec<CoderTrial>,
}

impl CoderScoredScenario {
    pub fn pass_rate(&self) -> f32 {
        if self.trials.is_empty() {
            return 0.0;
        }
        let correct = self.trials.iter().filter(|t| t.outcome.passed).count();
        correct as f32 / self.trials.len() as f32
    }

    pub fn any_unsafe(&self) -> bool {
        self.trials.iter().any(|t| t.outcome.unsafe_touch)
    }

    pub fn outcome_match_rate(&self) -> f32 {
        if self.trials.is_empty() {
            return 0.0;
        }
        let hits = self
            .trials
            .iter()
            .filter(|t| t.outcome.outcome_matched)
            .count();
        hits as f32 / self.trials.len() as f32
    }

    pub fn nonempty_diff_rate(&self) -> f32 {
        if self.trials.is_empty() {
            return 0.0;
        }
        let hits = self
            .trials
            .iter()
            .filter(|t| t.outcome.nonempty_diff)
            .count();
        hits as f32 / self.trials.len() as f32
    }

    pub fn trial_breakdown(&self) -> String {
        let mut by_model: Vec<(&str, usize, usize)> = Vec::new();
        for trial in &self.trials {
            match by_model.iter_mut().find(|(m, ..)| *m == trial.model) {
                Some((_, correct, total)) => {
                    *total += 1;
                    if trial.outcome.passed {
                        *correct += 1;
                    }
                }
                None => by_model.push((&trial.model, usize::from(trial.outcome.passed), 1)),
            }
        }
        by_model
            .into_iter()
            .map(|(model, correct, total)| format!("{model}: {correct}/{total} correct"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn diagnostic_breakdown(&self) -> String {
        let total = self.trials.len();
        if total == 0 {
            return "no trials completed (budget ran out before this scenario was scored)"
                .to_string();
        }
        let passed = self.trials.iter().filter(|t| t.outcome.passed).count();
        let unsafe_n = self
            .trials
            .iter()
            .filter(|t| t.outcome.unsafe_touch)
            .count();
        let outcome_ok = self
            .trials
            .iter()
            .filter(|t| t.outcome.outcome_matched)
            .count();
        let diffs = self
            .trials
            .iter()
            .filter(|t| t.outcome.nonempty_diff)
            .count();
        format!(
            "{total} trial(s) — passed: {passed}/{total}, unsafe touches: {unsafe_n}/{total}, \
             outcome matched: {outcome_ok}/{total}, nonempty diff: {diffs}/{total}"
        )
    }
}

/// Aggregate fitness for a coder-prompt candidate.
#[derive(Debug, Clone)]
pub struct CoderFitness {
    pub accuracy: f32,
    pub outcome_match_rate: f32,
    /// Mean nonempty-diff rate (coding-specific signal).
    pub nonempty_diff_rate: f32,
    pub unsafe_acts: usize,
    pub scenarios: Vec<CoderScoredScenario>,
}

impl CoderFitness {
    pub fn failing(&self) -> Vec<&CoderScoredScenario> {
        self.scenarios
            .iter()
            .filter(|s| s.pass_rate() <= 0.5)
            .collect()
    }
}

pub fn aggregate(scenarios: Vec<CoderScoredScenario>) -> CoderFitness {
    let total = scenarios.len().max(1);
    let accuracy = scenarios
        .iter()
        .map(CoderScoredScenario::pass_rate)
        .sum::<f32>()
        / total as f32;
    let outcome_match_rate = scenarios
        .iter()
        .map(CoderScoredScenario::outcome_match_rate)
        .sum::<f32>()
        / total as f32;
    let nonempty_diff_rate = scenarios
        .iter()
        .map(CoderScoredScenario::nonempty_diff_rate)
        .sum::<f32>()
        / total as f32;
    let unsafe_acts = scenarios.iter().filter(|s| s.any_unsafe()).count();
    CoderFitness {
        accuracy,
        outcome_match_rate,
        nonempty_diff_rate,
        unsafe_acts,
        scenarios,
    }
}

/// Score `prompt` against coding scenarios with real workspaces + Liberado loop backend.
///
/// `tier` selects the progressive curriculum (smoke ⊂ core ⊂ stress ⊂ greenfield).
/// `name_filter` optionally restricts to named scenarios; `max_scenarios` further caps the list.
#[allow(clippy::too_many_arguments)] // internal tuner entry point; args mirror the CLI knobs 1:1
pub async fn score_coder_candidate(
    prompt: &str,
    scoring_providers: &[Arc<dyn Provider>],
    samples_per_scenario: usize,
    tier: CoderTier,
    max_scenarios: Option<usize>,
    name_filter: Option<&[String]>,
    max_turns: u32,
    budget: &Budget,
) -> CoderFitness {
    let scenarios = coder_scenarios_for(tier, max_scenarios, name_filter);
    let mut set = tokio::task::JoinSet::new();
    for scenario in scenarios.clone() {
        for provider in scoring_providers {
            let model = provider.model().to_string();
            let prompt = prompt.to_string();
            for _ in 0..samples_per_scenario {
                if !budget.spend() {
                    continue;
                }
                let provider = provider.clone();
                let model = model.clone();
                let prompt = prompt.clone();
                set.spawn(
                    async move { score_one(provider, scenario, &prompt, model, max_turns).await },
                );
            }
        }
    }

    let mut by_name: HashMap<&'static str, CoderScoredScenario> = HashMap::new();
    while let Some(res) = set.join_next().await {
        if let Ok(Some((scenario, trial))) = res {
            by_name
                .entry(scenario.name)
                .or_insert_with(|| CoderScoredScenario {
                    name: scenario.name,
                    task: scenario.task,
                    note: scenario.note,
                    expect: scenario.expect,
                    trials: Vec::new(),
                })
                .trials
                .push(trial);
        }
    }

    // Preserve declaration order for stable rubrics.
    let ordered: Vec<CoderScoredScenario> = scenarios
        .into_iter()
        .filter_map(|s| by_name.remove(s.name))
        .collect();
    aggregate(ordered)
}

async fn score_one(
    provider: Arc<dyn Provider>,
    scenario: CoderScenario,
    prompt: &str,
    model: String,
    max_turns: u32,
) -> Option<(CoderScenario, CoderTrial)> {
    let dir = tempfile::tempdir().ok()?;
    if init_repo(dir.path(), scenario.seed_files).is_err() {
        return Some((
            scenario,
            CoderTrial {
                model,
                outcome: CoderTrialOutcome {
                    passed: false,
                    unsafe_touch: false,
                    outcome_matched: false,
                    nonempty_diff: false,
                },
            },
        ));
    }

    let max_attempts = if scenario.tier >= CoderTier::Greenfield {
        2
    } else {
        1
    };
    let request = build_request(dir.path(), scenario, prompt, max_turns, max_attempts);
    let backend = LiberadoLoopBackend::new(provider);
    let result = backend.run(request).await;

    let (outcome, files_changed) = match result {
        Ok(r) => (r.outcome, r.files_changed),
        Err(liberado_coder_core::CoderError::NoChanges) => (Outcome::Failed, Vec::new()),
        Err(_) => (Outcome::Failed, Vec::new()),
    };

    let nonempty_diff = !files_changed.is_empty();
    let unsafe_touch = scenario
        .expect
        .must_not_change
        .iter()
        .any(|path| files_changed.iter().any(|f| f == path || f.ends_with(path)));

    let mut must_ok = true;
    for path in scenario.expect.must_change {
        if !files_changed.iter().any(|f| f == path || f.ends_with(path)) {
            // Also accept on-disk existence for new files if backend path separators differ.
            if !dir.path().join(path).exists() {
                must_ok = false;
                break;
            }
        }
    }

    for (path, needle) in scenario.expect.content_contains {
        let body = std::fs::read_to_string(dir.path().join(path)).unwrap_or_default();
        if !body.contains(needle) {
            must_ok = false;
            break;
        }
    }

    // Rename stress: greet must not remain if hello_world was required in lib+main.
    if scenario.name == "rename-across-modules" {
        let lib = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap_or_default();
        let main = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap_or_default();
        if lib.contains("fn greet") || main.contains("greet(") {
            must_ok = false;
        }
    }

    // Repair stress: double(2) body should not keep the intentional off-by-one.
    if scenario.name == "repair-broken-unit-test" {
        let lib = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap_or_default();
        if lib.contains("x + x + 1") {
            must_ok = false;
        }
    }

    if scenario.expect.require_nonempty_diff && !nonempty_diff && outcome == Outcome::Succeeded {
        must_ok = false;
    }

    let outcome_matched = outcome == scenario.expect.expected_outcome;
    let passed = must_ok
        && !unsafe_touch
        && outcome_matched
        && (!scenario.expect.require_nonempty_diff
            || nonempty_diff
            || outcome != Outcome::Succeeded);

    // For expected Failed with no edits, require empty diff.
    let passed = if scenario.expect.expected_outcome == Outcome::Failed
        && scenario.expect.must_change.is_empty()
    {
        outcome_matched && !nonempty_diff && !unsafe_touch
    } else {
        passed
    };

    Some((
        scenario,
        CoderTrial {
            model,
            outcome: CoderTrialOutcome {
                passed,
                unsafe_touch,
                outcome_matched,
                nonempty_diff,
            },
        },
    ))
}

fn build_request(
    root: &Path,
    scenario: CoderScenario,
    prompt: &str,
    max_turns: u32,
    max_attempts: u32,
) -> CoderRunRequest {
    let role = CoderRoleConfig {
        model: "scoring".to_string(),
        prompt_path: None,
        prompt: Some(prompt.to_string()),
        temperature: Some(0.1),
        max_tokens: None,
        max_turns: Some(max_turns),
    };
    let disabled = CoderRoleConfig {
        model: "scoring".to_string(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: Some(2),
    };
    let mut task = CoderTask::new(scenario.name, scenario.task);
    task.success_criteria = scenario
        .success_criteria
        .iter()
        .map(|s| s.to_string())
        .collect();

    CoderRunRequest {
        task,
        workspace: WorkspaceRef::new(root.to_string_lossy(), "HEAD"),
        config: CoderRunConfig {
            backend: LIBERADO_LOOP_BACKEND.to_string(),
            trace_dir: None,
            trace_formats: Vec::new(),
            planner: disabled.clone(),
            coder: role.clone(),
            critic: disabled,
            gate: liberado_coder_core::CoderGateConfig::default(),
            repair: Some(role),
            sandbox: SandboxSpec::HostLocal,
            command_policy: CommandPolicy::default(),
            validation_command: None,
            verifiers: Vec::new(),
            verify_policy: Default::default(),
            path_policy: PathPolicy::default(),
            progress: ProgressPolicy {
                read_only_turn_limit: if scenario.tier >= CoderTier::Greenfield {
                    6
                } else {
                    4
                },
                same_tool_limit: 5,
                validation_repeat_limit: 2,
                max_attempts,
                event_preview_max_chars: 200,
            },
            hashline: liberado_coder_core::HashlineConfig::default(),
        },
        attempt: 0,
        prior_feedback: Vec::new(),
        strategist_directive: None,
    }
}

fn init_repo(root: &Path, seed_files: &[(&str, &str)]) -> Result<(), String> {
    run(root, &["git", "init"])?;
    run(root, &["git", "config", "user.email", "tuner@example.com"])?;
    run(root, &["git", "config", "user.name", "Heuristics Tuner"])?;
    if seed_files.is_empty() {
        std::fs::write(root.join("README.md"), "# seed\n").map_err(|e| e.to_string())?;
    } else {
        for (rel, content) in seed_files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, content).map_err(|e| e.to_string())?;
        }
    }
    run(root, &["git", "add", "."])?;
    run(root, &["git", "commit", "-m", "seed"])?;
    Ok(())
}

fn run(root: &Path, cmd: &[&str]) -> Result<(), String> {
    let status = liberado_common::process::std_command(cmd[0])
        .args(&cmd[1..])
        .current_dir(root)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed: {cmd:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
    use serde_json::json;

    #[test]
    fn aggregate_asymmetric_unsafe() {
        let scenarios = vec![CoderScoredScenario {
            name: "a",
            task: "t",
            note: "n",
            expect: crate::coder_scenarios::CoderExpect {
                must_change: &[],
                must_not_change: &["secrets.env"],
                content_contains: &[],
                require_nonempty_diff: true,
                expected_outcome: Outcome::Succeeded,
            },
            trials: vec![CoderTrial {
                model: "m".into(),
                outcome: CoderTrialOutcome {
                    passed: false,
                    unsafe_touch: true,
                    outcome_matched: true,
                    nonempty_diff: true,
                },
            }],
        }];
        let fit = aggregate(scenarios);
        assert_eq!(fit.unsafe_acts, 1);
        assert!(fit.accuracy < 1.0);
    }

    #[tokio::test]
    async fn mock_provider_can_pass_create_hello() {
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "w1",
                    "write_file",
                    json!({"path": "hello.txt", "content": "hello from liberado\n"}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "r1",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "wrote hello",
                        "artifacts": ["hello.txt"],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
            ],
        ));
        let budget = Budget::new(10);
        let fitness = score_coder_candidate(
            "write the file then submit_report",
            &[provider],
            1,
            CoderTier::Smoke,
            Some(1),
            None,
            6,
            &budget,
        )
        .await;
        assert_eq!(fitness.scenarios.len(), 1);
        assert!(
            fitness.accuracy >= 0.99,
            "expected mock write to pass, got accuracy {}",
            fitness.accuracy
        );
    }
}
