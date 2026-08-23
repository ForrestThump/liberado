//! A cold agent that fixes what the session critic alleged, on a branch of its own.
//!
//! ## The idea
//!
//! By the time the session critic speaks, the implementer is thirty-odd turns in, its context is
//! mostly tool output, and it has already filed a report it would now have to contradict. Handing
//! it the finding asks the worst-placed model to do the most delicate work. The F6b run is the
//! evidence: it diagnosed its own broken test correctly, with fresher context than it would have
//! had at the end, and shipped it anyway.
//!
//! So the fix is written by a *cold* agent instead — no sunk cost, no narrative to defend, the
//! same reason the diff reviewer is cold. When the PR reaches a human, the finding and a candidate
//! fix arrive together, and the reviewer chooses rather than implements.
//!
//! ## Why this is off by default
//!
//! **A ready-made fix is an argument for the finding that produced it.** A reviewer looking at a
//! working diff is far likelier to take it than to go back and establish whether the allegation
//! was ever true. When the critic is wrong — and at four labelled traces we do not know how often
//! that is — this converts a cheap false positive into a plausible wrong change with momentum
//! behind it.
//!
//! Three things push back on that, and all three are structural rather than hopeful:
//!
//! - the report renders the finding *above* the fix and labels the finding unverified
//!   (`render_findings_markdown`),
//! - the fix lands on its own branch and is never pushed over the implementer's,
//! - `[coder.session_critic] remediation` defaults to false.
//!
//! ## One run, not one per finding
//!
//! Every actionable finding goes into a single task. Three separate runs would produce three
//! branches off the same head, each unaware of the others, and merging them is work the reviewer
//! did not have before.
//!
//! ## No recursion
//!
//! The remediation run has its own session critic disabled. Otherwise a bad finding spawns a fix
//! that spawns a finding about the fix, and the cost of one false positive is unbounded.

use liberado_coder_core::{
    CoderBackend, CoderError, CoderRunRequest, CoderTask, Remedy, SessionFinding,
};

/// The task text for a remediation run, or `None` when no finding can be coded away.
///
/// Every finding is rendered with its verbatim quote and its remedy, and the instructions say
/// plainly that the allegations may be wrong. That is not politeness: a cold agent told to "fix
/// these defects" will produce a change for each one whether or not the defect exists, and an
/// invented fix for an imagined problem is worse than no branch at all.
pub fn remediation_task(findings: &[SessionFinding]) -> Option<String> {
    let actionable: Vec<&SessionFinding> = findings
        .iter()
        .filter(|f| f.remedy.is_actionable())
        .collect();
    if actionable.is_empty() {
        return None;
    }

    let mut task = String::from(
        "A reviewer read the transcript of an earlier coding run on this workspace and raised the \
         concerns below. Each quotes that run verbatim.\n\n\
         These are allegations, not established facts. For each one: decide first whether it is \
         true, by reading the code. If it is not, change nothing for it and say so. Do not \
         manufacture a change to look responsive.\n\n",
    );
    for (i, finding) in actionable.iter().enumerate() {
        let action = match finding.remedy {
            Remedy::Verify => {
                "Run the check that was claimed. If it passes, the code is fine and only the \
                 claim was unsupported - report that. If it fails, fix the cause."
            }
            _ => "If the concern is real, change the code or the tests so it no longer holds.",
        };
        task.push_str(&format!(
            "{}. [{}] {}\n   The run said: \"{}\"\n   What to do: {action}\n\n",
            i + 1,
            finding.kind,
            finding.why,
            finding.quote.replace('\n', " "),
        ));
    }
    task.push_str(
        "Stay inside these concerns. Do not refactor, rename, or improve anything they do not \
         name - a diff a reviewer cannot check against the list above is a diff they will reject \
         wholesale. Report per concern: what you found, and what you changed or why you did not.",
    );
    Some(task)
}

/// The branch a remediation run writes on. Never the implementer's.
pub fn remediation_branch(task_id: &str) -> String {
    let slug: String = task_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("agent/remediation-{}-{stamp}", slug.trim_matches('-'))
}

/// Build the request for the remediation run from the original one.
///
/// Inherits the workspace, the tools and the verifiers — a remediation run is a coding run and
/// has every failure mode of one, so it gets the same acceptance checks rather than a lighter
/// path nobody is watching. What it does *not* inherit is its own session critic.
pub fn remediation_request(base: &CoderRunRequest, task_text: String) -> CoderRunRequest {
    let mut request = base.clone();
    request.task = CoderTask::new(format!("{}-remediation", base.task.id), task_text);
    request.attempt = 0;
    request.prior_feedback = Vec::new();
    request.strategist_directive = None;
    request.config.session_critic.enabled = false;
    request.config.session_critic.remediation = false;
    request
}

/// Run the remediation and describe what happened.
///
/// The caller is responsible for putting the workspace on [`remediation_branch`] first and for
/// committing afterwards: branch management belongs to the surface that owns the worktree, and a
/// backend that checks out branches behind its caller's back is a worse problem than a manual step.
pub async fn run_remediation(
    backend: &dyn CoderBackend,
    base: &CoderRunRequest,
    findings: &[SessionFinding],
    branch: String,
) -> Result<Option<liberado_coder_core::RemediationRecord>, CoderError> {
    let Some(task_text) = remediation_task(findings) else {
        return Ok(None);
    };
    let addressed: Vec<String> = findings
        .iter()
        .filter(|f| f.remedy.is_actionable())
        .map(|f| f.kind.clone())
        .collect();

    let result = backend.run(remediation_request(base, task_text)).await?;
    Ok(Some(liberado_coder_core::RemediationRecord {
        branch,
        outcome: result.outcome,
        summary: result.summary,
        addressed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::CoderRunResult;

    fn finding(kind: &str, remedy: Remedy) -> SessionFinding {
        SessionFinding {
            kind: kind.to_string(),
            quote: "passes even when I break run_headless".to_string(),
            why: "shipped a test that does not bind".to_string(),
            remedy,
        }
    }

    /// A `retract`-only review must not spend a coding run. The remedy for an overstated report
    /// is a text edit, and dispatching an agent for it is how a cheap finding becomes expensive.
    #[test]
    fn nothing_actionable_means_no_run() {
        assert!(remediation_task(&[finding("unsupported_claim", Remedy::Retract)]).is_none());
        assert!(remediation_task(&[finding("x", Remedy::None)]).is_none());
        assert!(remediation_task(&[]).is_none());
    }

    /// One task carrying every actionable finding, not one task each. Separate runs produce
    /// separate branches off the same head that no one asked for.
    #[test]
    fn every_actionable_finding_lands_in_one_task() {
        let task = remediation_task(&[
            finding("abandoned_finding", Remedy::Repair),
            finding("unsupported_claim", Remedy::Retract),
            finding("silent_reversal", Remedy::Verify),
        ])
        .expect("two of these are actionable");
        assert!(task.contains("abandoned_finding"));
        assert!(task.contains("silent_reversal"));
        assert!(
            !task.contains("unsupported_claim"),
            "a retract-only finding must not be handed to a coding agent:\n{task}"
        );
    }

    /// The agent must be told the findings may be wrong.
    ///
    /// Without it, a cold agent handed a list of defects produces a change for each one whether
    /// or not it exists — and a confident fix for an imagined problem is the single worst output
    /// this path can produce, because it looks exactly like a good one.
    #[test]
    fn the_agent_is_told_the_findings_are_unproven() {
        let task = remediation_task(&[finding("abandoned_finding", Remedy::Repair)]).expect("task");
        assert!(
            task.contains("allegations, not established facts"),
            "the agent must be free to disagree:\n{task}"
        );
        assert!(
            task.contains("Do not manufacture a change"),
            "the failure mode is a responsive-looking diff for a defect that is not there:\n{task}"
        );
    }

    #[test]
    fn a_verify_finding_asks_for_the_check_not_a_rewrite() {
        let task = remediation_task(&[finding("unsupported_claim", Remedy::Verify)]).expect("task");
        assert!(
            task.contains("Run the check that was claimed"),
            "verify means go and get the missing evidence; the code may be correct:\n{task}"
        );
    }

    #[test]
    fn the_findings_quote_travels_into_the_task() {
        let task = remediation_task(&[finding("abandoned_finding", Remedy::Repair)]).expect("task");
        assert!(
            task.contains("passes even when I break run_headless"),
            "the agent needs the run's own words to judge the claim:\n{task}"
        );
    }

    /// No recursion. A bad finding must not be able to spawn a fix that spawns a finding about
    /// the fix; one level bounds the cost of a false positive.
    #[test]
    fn the_remediation_run_has_no_session_critic_of_its_own() {
        let mut base = base_request();
        base.config.session_critic.enabled = true;
        base.config.session_critic.remediation = true;
        let derived = remediation_request(&base, "fix it".to_string());
        assert!(!derived.config.session_critic.enabled);
        assert!(!derived.config.session_critic.remediation);
    }

    /// The remediation run keeps the acceptance checks. It is a coding run with every failure
    /// mode of one, and a lighter path nobody watches is where the next fabricated report comes
    /// from.
    #[test]
    fn the_remediation_run_keeps_the_verifiers() {
        let mut base = base_request();
        base.config.verifiers = liberado_coder_core::default_verifiers(std::path::Path::new("."));
        let derived = remediation_request(&base, "fix it".to_string());
        assert_eq!(derived.config.verifiers, base.config.verifiers);
        assert_eq!(derived.attempt, 0, "a remediation run starts fresh");
        assert!(derived.prior_feedback.is_empty());
    }

    #[test]
    fn the_branch_is_never_the_implementers() {
        let branch = remediation_branch("lib-18ca-4321");
        assert!(branch.starts_with("agent/remediation-"), "{branch}");
        assert!(branch.contains("lib-18ca-4321"), "{branch}");
    }

    struct RecordingBackend {
        seen: std::sync::Mutex<Vec<CoderRunRequest>>,
    }

    #[async_trait::async_trait]
    impl CoderBackend for RecordingBackend {
        fn name(&self) -> &str {
            "recording"
        }
        async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
            self.seen.lock().unwrap().push(request);
            Ok(CoderRunResult {
                backend: "recording".into(),
                outcome: liberado_common::Outcome::Succeeded,
                summary: "concern addressed".into(),
                files_changed: vec![],
                file_changes: Vec::new(),
                validation_notes: None,
                critic_verdict: None,
                gate_votes: Vec::new(),
                trace_path: None,
                diff_findings: Vec::new(),
                session_findings: Vec::new(),
                remediation: None,
                diagnostics: serde_json::Value::Null,
            })
        }
    }

    /// End to end: actionable findings produce exactly one run whose request carries the
    /// derived task, and the record reports what was run and what it targeted.
    #[tokio::test]
    async fn a_run_happens_and_its_record_carries_the_outcome() {
        let backend = RecordingBackend {
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let base = base_request();
        let record = run_remediation(
            &backend,
            &base,
            &[finding("abandoned_finding", Remedy::Repair)],
            "agent/remediation-t".to_string(),
        )
        .await
        .expect("the backend succeeds")
        .expect("an actionable finding must produce a run");

        let seen = backend.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one finding, one combined run");
        assert!(
            seen[0].task.description.contains("abandoned_finding"),
            "the run must carry the findings: {}",
            seen[0].task.description
        );
        assert_eq!(record.branch, "agent/remediation-t");
        assert_eq!(record.outcome, liberado_common::Outcome::Succeeded);
        assert_eq!(record.summary, "concern addressed");
        assert_eq!(record.addressed, vec!["abandoned_finding".to_string()]);
    }

    fn base_request() -> CoderRunRequest {
        CoderRunRequest {
            task: CoderTask::new("t", "do a thing"),
            workspace: liberado_coder_core::WorkspaceRef::new("/tmp", "HEAD"),
            config: liberado_coder_core::CoderTuning::default().run_config(),
            attempt: 3,
            prior_feedback: vec!["earlier failure".into()],
            strategist_directive: Some("rethink".into()),
        }
    }
}
