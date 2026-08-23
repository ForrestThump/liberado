//! Role selection and prompt/goal assembly for the coding domain pack.
//!
//! Role *slots* (planner/worker/critic/repair) are a general goal-session idea; the coding pack
//! binds them to `CoderRoleConfig` and workspace-oriented goals. A future neutral session kernel
//! should own the graph; this module stays coding-facing.

use liberado_coder_core::{CoderError, CoderRoleConfig, CoderRunRequest};

pub fn worker_role_name(request: &CoderRunRequest) -> &'static str {
    if request.attempt > 0 && request.config.repair.is_some() {
        "repair"
    } else {
        "coder"
    }
}

pub fn worker_role_config(request: &CoderRunRequest) -> &CoderRoleConfig {
    if request.attempt > 0
        && let Some(repair) = &request.config.repair
    {
        return repair;
    }
    &request.config.coder
}
/// The cold diff reviewer's instructions now live in `prompts/coder/diff-reviewer.md`.
///
/// It was a `const` here, which meant retuning a reviewer prompt cost a full workspace rebuild
/// — minutes, on the loop that most wants to be fast. `liberado_coder_core::prompts` bakes the
/// same file in as a fallback, so nothing breaks in a container that ships only the binary.
pub use liberado_coder_core::prompts::DIFF_REVIEWER as COLD_DIFF_REVIEWER_PROMPT;

/// The built-in prompt for a role whose `prompt_path` is absent.
///
/// Repair is the coder under another name, and every gate reviewer reads a diff, so they share
/// the reviewer text rather than each carrying a near-copy that drifts.
fn baked_prompt_for(role_name: &str) -> Option<&'static str> {
    match role_name {
        "coder" | "repair" => Some(liberado_coder_core::prompts::CODER),
        "critic" | "fresh" | "gatekeeper" | "session-critic" => {
            Some(liberado_coder_core::prompts::DIFF_REVIEWER)
        }
        _ => None,
    }
}

pub fn critic_enabled(request: &CoderRunRequest) -> bool {
    request.config.critic.prompt.is_some() || request.config.critic.prompt_path.is_some()
}

/// Planner runs when configured with a prompt (or prompt_path). Empty = skip.
pub fn planner_enabled(request: &CoderRunRequest) -> bool {
    request.config.planner.prompt.is_some() || request.config.planner.prompt_path.is_some()
}

/// Resolve a role's system prompt.
///
/// Order: an inline `prompt`, then `prompt_path`, then the copy compiled in from
/// `prompts/coder/` (see [`liberado_coder_core::prompts`]).
///
/// **A missing `prompt_path` is no longer fatal.** It used to return `Err`, which failed the whole
/// run — so a container that shipped the binary without `prompts/` could not code at all, and
/// enabling the completion gate failed at its first reviewer for the same reason. Falling back to
/// the baked copy of the same file keeps a deployment working while still letting a checkout
/// override it by editing the file.
///
/// A read error that is *not* "missing" still fails: a prompt file that exists and cannot be read
/// means the deployment is broken in a way silence would hide.
pub async fn role_instructions(
    role: &CoderRoleConfig,
    role_name: &str,
) -> Result<String, CoderError> {
    if let Some(prompt) = role.prompt.clone() {
        return Ok(prompt);
    }
    if let Some(path) = &role.prompt_path {
        match tokio::fs::read_to_string(path).await {
            Ok(text) if !text.trim().is_empty() => return Ok(text),
            Ok(_) => tracing::warn!(
                %path,
                %role_name,
                "prompt_path is empty; falling back to the built-in prompt"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => tracing::warn!(
                %path,
                %role_name,
                "prompt_path does not exist; falling back to the built-in prompt"
            ),
            Err(e) => {
                return Err(CoderError::Setup(format!(
                    "read {role_name} prompt_path {path}: {e}"
                )));
            }
        }
    }
    match baked_prompt_for(role_name) {
        Some(text) => Ok(text.to_string()),
        None => Err(CoderError::Setup(format!(
            "{role_name} role requires prompt or prompt_path"
        ))),
    }
}

pub fn coder_goal(request: &CoderRunRequest) -> String {
    let mut goal = format!("Task: {}", request.task.description);
    if let Some(context) = &request.task.context {
        goal.push_str("\n\nContext:\n");
        goal.push_str(context);
    }
    if !request.task.success_criteria.is_empty() {
        goal.push_str("\n\nSuccess criteria:\n");
        for criterion in &request.task.success_criteria {
            goal.push_str("- ");
            goal.push_str(criterion);
            goal.push('\n');
        }
    }
    // A strategist directive outranks everything below it. It only exists because several
    // attempts already failed the same way, so the tactical repair hints have demonstrably not
    // worked — burying it under them would reproduce the loop it exists to break. Rendered on
    // every attempt that carries one, never routed through `repair_focus_block`.
    if let Some(directive) = &request.strategist_directive {
        goal.push_str("\n\n## Structural directive (read this first)\n");
        goal.push_str(
            "Earlier attempts were refused repeatedly for the same reasons. A strategist reviewed \
             the goal and the rejection history and proposes ONE structural change. Apply it. It \
             does not relax the success criteria above — those still hold in full.\n\n",
        );
        goal.push_str(directive);
        goal.push('\n');
    }
    // Repair attempts: put failure-signature routing first so the model prioritizes it.
    if request.attempt > 0 {
        if let Some(focus) = crate::repair_feedback::repair_focus_block(&request.prior_feedback) {
            goal.push_str("\n\n");
            goal.push_str(&focus);
        }
    } else if !request.prior_feedback.is_empty() {
        goal.push_str("\n\nPrior feedback (from earlier attempts / guards / critic):\n");
        for feedback in &request.prior_feedback {
            goal.push_str("- ");
            goal.push_str(feedback);
            goal.push('\n');
        }
    }
    goal
}

pub fn truncate_chars(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max_chars).collect();
    out.push_str("\n…[truncated]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(prompt: Option<&str>, prompt_path: Option<&str>) -> CoderRoleConfig {
        CoderRoleConfig {
            model: "m".to_string(),
            prompt: prompt.map(str::to_string),
            prompt_path: prompt_path.map(str::to_string),
            temperature: None,
            max_tokens: None,
            max_turns: None,
            reasoning: None,
        }
    }

    /// A missing prompt file must not fail the run.
    ///
    /// It used to return `Err`, which meant a container shipping the binary without `prompts/`
    /// could not code at all, and switching the completion gate on failed at its first reviewer
    /// for the same reason. The built-in copy comes from the same file, so falling back changes
    /// what can be *tuned*, not what the model is told.
    #[tokio::test]
    async fn a_missing_prompt_file_falls_back_instead_of_failing_the_run() {
        let resolved = role_instructions(&role(None, Some("prompts/nope/absent.md")), "coder")
            .await
            .expect("a missing override must not end the run");
        assert_eq!(resolved, liberado_coder_core::prompts::CODER);
    }

    /// Every role the harness dispatches must resolve to something. A role with no built-in and
    /// no file is the failure that took the gate down; if a new role name appears without an
    /// entry in `baked_prompt_for`, this is where it surfaces.
    #[tokio::test]
    async fn every_dispatched_role_has_a_prompt_of_last_resort() {
        for name in [
            "coder",
            "repair",
            "critic",
            "fresh",
            "gatekeeper",
            "session-critic",
        ] {
            let resolved = role_instructions(&role(None, None), name)
                .await
                .unwrap_or_else(|e| panic!("role `{name}` has no fallback prompt: {e}"));
            assert!(
                resolved.trim().len() > 200,
                "role `{name}` fell back to something too short to be a prompt"
            );
        }
    }

    /// A `prompt_path` that exists but cannot be read must still fail.
    ///
    /// "Missing" means unconfigured and is fine. "Present and unreadable" means the deployment is
    /// broken — a bad mount, wrong permissions, a path pointing at a directory — and silently
    /// substituting the built-in copy would hide it behind a run that looks normal.
    #[tokio::test]
    async fn an_unreadable_prompt_path_is_an_error_not_a_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A directory is the portable way to get a read error that is not NotFound.
        let err = role_instructions(&role(None, Some(&dir.path().to_string_lossy())), "coder")
            .await
            .expect_err("an unreadable prompt file must not be papered over");
        assert!(
            err.to_string().contains("prompt_path"),
            "the error must name what failed: {err}"
        );
    }

    /// An unknown role is still an error. Silently handing a stranger the coder's instructions
    /// would be worse than refusing.
    #[tokio::test]
    async fn an_unknown_role_without_a_prompt_is_still_an_error() {
        role_instructions(&role(None, None), "invented-role")
            .await
            .expect_err("an unrecognised role must not inherit someone else's prompt");
    }

    /// An inline prompt still wins. That is how a deployment overrides one role.
    #[tokio::test]
    async fn an_inline_prompt_outranks_everything() {
        let resolved = role_instructions(
            &role(Some("INLINE"), Some("prompts/coder/coder.md")),
            "coder",
        )
        .await
        .expect("inline");
        assert_eq!(resolved, "INLINE");
    }

    /// An existing but empty prompt_path file is "unconfigured", not "the prompt is blank":
    /// the baked copy must win.
    #[tokio::test]
    async fn an_empty_prompt_file_falls_back_to_the_baked_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.md");
        std::fs::write(&path, "   \n").unwrap();
        let resolved = role_instructions(&role(None, Some(&path.to_string_lossy())), "coder")
            .await
            .expect("an empty file is not an error");
        assert_eq!(
            resolved,
            liberado_coder_core::prompts::CODER,
            "blank override falls back to the built-in prompt"
        );
    }

    fn request(
        attempt: u32,
        repair: Option<liberado_coder_core::CoderRoleConfig>,
        criteria: &[&str],
        feedback: Vec<String>,
        directive: Option<&str>,
    ) -> CoderRunRequest {
        let mut config: liberado_coder_core::CoderRunConfig =
            serde_json::from_value(serde_json::json!({
                "backend": "liberado-loop",
                "planner": {"model": "m"},
                "coder": {"model": "m", "prompt": "CODER-PROMPT-MARKER"},
                "critic": {"model": "m"}
            }))
            .expect("config fixture");
        config.repair = repair;
        let mut task = liberado_coder_core::CoderTask::new("t", "do the thing");
        task.success_criteria = criteria.iter().map(|s| s.to_string()).collect();
        CoderRunRequest {
            task,
            workspace: liberado_coder_core::WorkspaceRef::new("/w", "HEAD"),
            config,
            attempt,
            prior_feedback: feedback,
            strategist_directive: directive.map(str::to_string),
        }
    }

    const REPAIR_PROMPT: &str = "REPAIR-PROMPT-MARKER";

    /// Repair attempts read the repair role's config, first attempts the coder's.
    #[test]
    fn worker_role_config_switches_on_attempt_and_repair_presence() {
        let repair = role(Some(REPAIR_PROMPT), None);
        let with = request(1, Some(repair), &[], vec![], None);
        assert_eq!(
            worker_role_config(&with).prompt.as_deref(),
            Some(REPAIR_PROMPT),
            "a retry with a repair role uses it"
        );
        // Attempt 0 never uses repair, even when configured.
        let fresh = request(0, Some(role(Some(REPAIR_PROMPT), None)), &[], vec![], None);
        assert_ne!(
            worker_role_config(&fresh).prompt.as_deref(),
            Some(REPAIR_PROMPT),
            "attempt 0 is the coder role even with repair configured"
        );
        // A retry without a configured repair stays on the coder role.
        let bare = request(1, None, &[], vec![], None);
        assert!(worker_role_config(&bare).prompt.is_some());
    }

    /// The goal text carries description, criteria, and routing of prior feedback by attempt:
    /// retries get signature-routed focus, first attempts a plain list.
    #[test]
    fn coder_goal_renders_criteria_and_routes_feedback_by_attempt() {
        // Success criteria render as a list; without them there is no empty section.
        let with = request(0, None, &["tests pass"], vec![], None);
        let goal = coder_goal(&with);
        assert!(goal.contains("Success criteria:"), "{goal}");
        assert!(goal.contains("- tests pass"), "{goal}");
        let without = request(0, None, &[], vec![], None);
        assert!(
            !coder_goal(&without).contains("Success criteria:"),
            "no criteria, no section:\n{}",
            coder_goal(&without)
        );

        // Retry: feedback becomes a repair-focus block.
        let retry = request(
            1,
            None,
            &[],
            vec!["FAILURE_CLASS: command_failed\nDETAIL".into()],
            None,
        );
        let goal = coder_goal(&retry);
        assert!(
            goal.contains("## Repair focus"),
            "retries route through failure-signature focus:\n{goal}"
        );
        assert!(!goal.contains("Prior feedback (from earlier"), "{goal}");

        // First attempt: feedback renders as a plain list.
        let first = request(0, None, &[], vec!["earlier note".into()], None);
        let goal = coder_goal(&first);
        assert!(goal.contains("Prior feedback (from earlier"), "{goal}");
        assert!(goal.contains("- earlier note"), "{goal}");
        assert!(!goal.contains("## Repair focus"), "{goal}");

        // No feedback on a first attempt: neither section appears.
        let clean = coder_goal(&request(0, None, &[], vec![], None));
        assert!(
            !clean.contains("Prior feedback") && !clean.contains("Repair focus"),
            "{clean}"
        );
    }

    /// Truncation keeps the head, marks the cut, and leaves short text untouched.
    #[test]
    fn truncate_chars_keeps_head_marks_cut_passes_short_through() {
        assert_eq!(truncate_chars("short", 10), "short");
        let long = "abcdefgh".to_string();
        let cut = truncate_chars(&long, 5);
        assert!(cut.starts_with("abcde"), "{cut}");
        assert!(cut.ends_with("…[truncated]"), "{cut}");
    }
}
