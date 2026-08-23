//! In-loop coding progress guards.
//!
//! Generic doom-loop detection lives in `liberado-executor`. These guards add
//! domain-specific stalls: long read-only exploration, repeated same tools, and
//! repeated identical validation failures.

use liberado_coder_core::ProgressPolicy;
use serde_json::Value;

/// Tools that can produce a real workspace mutation.
const MUTATING_TOOLS: &[&str] = &["write_file", "edit_file", "apply_patch"];

/// Tools that inspect without mutating. `run_command` is treated as non-mutating
/// for stall purposes (it may build/test, but does not count as code progress).
const INSPECT_TOOLS: &[&str] = &[
    "list_files",
    "search_text",
    "read_file",
    "git_status",
    "git_diff",
    "run_command",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressFatal {
    /// Too many consecutive non-mutating tools with no successful edit.
    ReadOnlyStall { consecutive: u32 },
    /// Same validation failure signature repeated past the limit.
    ValidationChurn { signature: String, repeats: u32 },
    /// Same tool name invoked consecutively past the limit after a nudge.
    SameToolChurn { tool: String, consecutive: u32 },
}

impl ProgressFatal {
    pub fn guard_name(&self) -> &'static str {
        match self {
            Self::ReadOnlyStall { .. } => "read_only_stall",
            Self::ValidationChurn { .. } => "validation_churn",
            Self::SameToolChurn { .. } => "same_tool_churn",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::ReadOnlyStall { consecutive } => format!(
                "PROGRESS GUARD (fatal): {consecutive} consecutive inspect/tool calls without a \
                 successful workspace mutation (write_file/edit_file/apply_patch). Stop exploring \
                 and either make the required edits or submit_report with outcome=failed and why \
                 you are blocked."
            ),
            Self::ValidationChurn { signature, repeats } => format!(
                "PROGRESS GUARD (fatal): validate failed {repeats} times with the same signature. \
                 Do not re-run the same broken fix. Change approach or submit_report failed.\n\
                 Signature:\n{signature}"
            ),
            Self::SameToolChurn { tool, consecutive } => format!(
                "PROGRESS GUARD (fatal): tool `{tool}` invoked {consecutive} times in a row \
                 without progress. Use a different tool or submit_report with outcome=failed."
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressAction {
    /// Continue; optionally surface a one-time nudge to the model.
    Continue { nudge: Option<String> },
    /// Hard stop for this coding attempt (still returned in-band to the model).
    Fatal(ProgressFatal),
}

#[derive(Debug)]
pub struct ProgressGuard {
    policy: ProgressPolicy,
    consecutive_non_mutating: u32,
    saw_successful_mutation: bool,
    last_tool: Option<String>,
    consecutive_same_tool: u32,
    read_only_nudged: bool,
    same_tool_nudged: bool,
    last_validation_signature: Option<String>,
    validation_fail_streak: u32,
    fatal: Option<ProgressFatal>,
}

impl ProgressGuard {
    pub fn new(policy: ProgressPolicy) -> Self {
        Self {
            policy,
            consecutive_non_mutating: 0,
            saw_successful_mutation: false,
            last_tool: None,
            consecutive_same_tool: 0,
            read_only_nudged: false,
            same_tool_nudged: false,
            last_validation_signature: None,
            validation_fail_streak: 0,
            fatal: None,
        }
    }

    pub fn fatal(&self) -> Option<&ProgressFatal> {
        self.fatal.as_ref()
    }

    pub fn take_fatal(&mut self) -> Option<ProgressFatal> {
        self.fatal.take()
    }

    /// Observe a completed tool invocation and decide whether to nudge or fail hard.
    pub fn observe(&mut self, tool_name: &str, ok: bool, result_preview: &str) -> ProgressAction {
        // A latched guard used to fail *every* subsequent call — including the two things its own
        // message demands ("make the required edits or submit_report"). That is a deadlock, not a
        // guard: the model is ordered to act and then refused every means of acting, so it burns
        // its whole turn budget achieving nothing. Worse, `observe` runs *after* the tool has
        // already executed, so a `write_file` that genuinely succeeded on disk was reported back
        // to the model as a failure — it then "retries" an edit it had in fact already made.
        //
        // Observed live: 8 inspect calls latched ReadOnlyStall, then write_file/write_file/edit_file
        // were all refused, the 12-turn budget drained, and the run ended `NoChanges`.
        //
        // So: keep refusing *exploration* — that is what the guard is for — but let the remedy
        // through. A successful mutation means the stall is over, so clear it and resume normally.
        if self.fatal.is_some() {
            if !escapes_fatal(tool_name) {
                return ProgressAction::Fatal(self.fatal.clone().expect("fatal checked as Some"));
            }
            if is_mutating(tool_name)
                && ok
                && matches!(self.fatal, Some(ProgressFatal::ReadOnlyStall { .. }))
            {
                // It stopped exploring and actually edited something. The stall is, by definition,
                // over. The churn guards stay latched — a write does not prove a *new approach* —
                // but they no longer block the write or the report.
                self.fatal = None;
                self.saw_successful_mutation = true;
                self.consecutive_non_mutating = 0;
                self.read_only_nudged = false;
                self.consecutive_same_tool = 0;
                self.same_tool_nudged = false;
            }
            return ProgressAction::Continue { nudge: None };
        }

        if tool_name == liberado_executor::SUBMIT_REPORT_TOOL {
            return ProgressAction::Continue { nudge: None };
        }

        // `run_command` is a multiplexer: rg, cargo, git, and echo share one name.
        // Compare 7 counted twenty distinct searches as SameToolChurn and then
        // discarded a filed `succeeded`. The executor's args-aware doom loop
        // still catches a replayed identical command.
        if !is_multiplex_tool(tool_name) {
            self.track_same_tool(tool_name);
        }

        if is_mutating(tool_name) {
            if ok {
                self.saw_successful_mutation = true;
                self.consecutive_non_mutating = 0;
                self.read_only_nudged = false;
                // Successful edit resets same-tool pressure for a new phase of work.
                self.consecutive_same_tool = 0;
                self.same_tool_nudged = false;
            } else {
                self.consecutive_non_mutating = self.consecutive_non_mutating.saturating_add(1);
            }
        } else if is_inspect(tool_name) || tool_name == "validate" {
            self.consecutive_non_mutating = self.consecutive_non_mutating.saturating_add(1);
        }

        if tool_name == "validate"
            && let Some(action) = self.observe_validation(ok, result_preview)
        {
            return action;
        }

        if let Some(action) = self.check_same_tool_limit(tool_name) {
            return action;
        }

        self.check_read_only_limit()
    }

    fn track_same_tool(&mut self, tool_name: &str) {
        if self.last_tool.as_deref() == Some(tool_name) {
            self.consecutive_same_tool = self.consecutive_same_tool.saturating_add(1);
        } else {
            self.last_tool = Some(tool_name.to_string());
            self.consecutive_same_tool = 1;
            self.same_tool_nudged = false;
        }
    }

    fn observe_validation(&mut self, ok: bool, result_preview: &str) -> Option<ProgressAction> {
        if ok && validation_passed(result_preview) {
            self.last_validation_signature = None;
            self.validation_fail_streak = 0;
            return None;
        }

        // Failed validate (tool error or passed=false).
        let signature = validation_signature(result_preview);
        if self.last_validation_signature.as_deref() == Some(signature.as_str()) {
            self.validation_fail_streak = self.validation_fail_streak.saturating_add(1);
        } else {
            self.last_validation_signature = Some(signature.clone());
            self.validation_fail_streak = 1;
        }

        let limit = self.policy.validation_repeat_limit.max(1);
        if self.validation_fail_streak > limit {
            let fatal = ProgressFatal::ValidationChurn {
                signature,
                repeats: self.validation_fail_streak,
            };
            self.fatal = Some(fatal.clone());
            return Some(ProgressAction::Fatal(fatal));
        }
        None
    }

    fn check_same_tool_limit(&mut self, tool_name: &str) -> Option<ProgressAction> {
        let limit = self.policy.same_tool_limit.max(1);
        if self.consecutive_same_tool < limit {
            return None;
        }
        if self.consecutive_same_tool == limit && !self.same_tool_nudged {
            self.same_tool_nudged = true;
            return Some(ProgressAction::Continue {
                nudge: Some(format!(
                    "PROGRESS GUARD: tool `{tool_name}` used {limit} times in a row. \
                     Switch tools or make a workspace edit; repeated identical exploration wastes budget."
                )),
            });
        }
        if self.consecutive_same_tool >= limit.saturating_mul(2) {
            let fatal = ProgressFatal::SameToolChurn {
                tool: tool_name.to_string(),
                consecutive: self.consecutive_same_tool,
            };
            self.fatal = Some(fatal.clone());
            return Some(ProgressAction::Fatal(fatal));
        }
        None
    }

    fn check_read_only_limit(&mut self) -> ProgressAction {
        let limit = self.policy.read_only_turn_limit.max(1);
        // Only stall-detect when we still have no successful mutation; otherwise the agent may
        // legitimately re-read after editing.
        if self.saw_successful_mutation {
            return ProgressAction::Continue { nudge: None };
        }
        if self.consecutive_non_mutating < limit {
            return ProgressAction::Continue { nudge: None };
        }
        if self.consecutive_non_mutating == limit && !self.read_only_nudged {
            self.read_only_nudged = true;
            return ProgressAction::Continue {
                nudge: Some(format!(
                    "PROGRESS GUARD: {limit} tool calls without a successful workspace mutation. \
                     You must write_file/edit_file/apply_patch for the task, or submit_report with \
                     outcome=failed if blocked. Do not keep exploring."
                )),
            };
        }
        if self.consecutive_non_mutating >= limit.saturating_mul(2) {
            let fatal = ProgressFatal::ReadOnlyStall {
                consecutive: self.consecutive_non_mutating,
            };
            self.fatal = Some(fatal.clone());
            return ProgressAction::Fatal(fatal);
        }
        ProgressAction::Continue { nudge: None }
    }
}

fn is_mutating(name: &str) -> bool {
    MUTATING_TOOLS.contains(&name)
}

/// Tools whose *name* is not the action. Same-tool counting on these is a false stall.
fn is_multiplex_tool(name: &str) -> bool {
    matches!(
        name,
        "run_command" | "run_command_background" | "bash" | "exec"
    )
}

/// Tools that must still reach [`ProgressGuard::observe`] once a fatal has latched: the remedy the
/// guard's own message demands, and the report that ends the run.
///
/// This lives here, next to `observe`'s handling, because it has to be applied in **two** places
/// and was previously applied in only one. `CodingToolRuntime::invoke` short-circuits on a latched
/// fatal before calling `observe`, so `observe`'s escape hatch — added specifically to stop the
/// guard from refusing the edits it was demanding — could never run. The deadlock it documents
/// stayed live: a model told to "make the required edits or submit_report" had both refused, and
/// said so ("All mutation tools are blocked by the progress guard") while filing a plan it could
/// not execute.
pub(crate) fn escapes_fatal(tool_name: &str) -> bool {
    is_mutating(tool_name) || tool_name == liberado_executor::SUBMIT_REPORT_TOOL
}

fn is_inspect(name: &str) -> bool {
    INSPECT_TOOLS.contains(&name)
}

fn validation_passed(result_preview: &str) -> bool {
    if let Ok(value) = serde_json::from_str::<Value>(result_preview) {
        return value
            .get("passed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    }
    // ToolRuntime maps JSON to string; also accept plain markers.
    result_preview.contains("\"passed\":true") || result_preview.contains("\"passed\": true")
}

fn validation_signature(result_preview: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(result_preview) {
        let exit = value
            .get("exit_code")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_string());
        let stdout = value
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(200)
            .collect::<String>();
        let stderr = value
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(200)
            .collect::<String>();
        return format!("exit={exit}|stdout={stdout}|stderr={stderr}");
    }
    result_preview.chars().take(400).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> ProgressPolicy {
        ProgressPolicy {
            read_only_turn_limit: 2,
            same_tool_limit: 2,
            validation_repeat_limit: 2,
            max_attempts: 3,
            event_preview_max_chars: 100,
        }
    }

    #[test]
    fn read_only_nudge_then_fatal() {
        let mut guard = ProgressGuard::new(policy());
        assert!(matches!(
            guard.observe("list_files", true, "{}"),
            ProgressAction::Continue { nudge: None }
        ));
        let second = guard.observe("read_file", true, "{}");
        assert!(matches!(
            second,
            ProgressAction::Continue { nudge: Some(_) }
        ));
        assert!(matches!(
            guard.observe("git_status", true, "{}"),
            ProgressAction::Continue { nudge: None }
        ));
        let fourth = guard.observe("search_text", true, "{}");
        assert!(matches!(
            fourth,
            ProgressAction::Fatal(ProgressFatal::ReadOnlyStall { consecutive: 4 })
        ));
    }

    #[test]
    fn a_latched_guard_still_lets_the_remedy_through() {
        // The guard's own message says: "Stop exploring and either make the required edits or
        // submit_report". It used to then refuse BOTH — every call after the latch returned Fatal,
        // including write_file and submit_report. The model was ordered to act and denied every
        // means of acting, so it burned its entire turn budget and the run died `NoChanges`. That
        // is a deadlock, not a guard. Observed live, 2026-07-14.
        let mut guard = ProgressGuard::new(policy());
        for t in ["list_files", "read_file", "git_status", "search_text"] {
            guard.observe(t, true, "{}");
        }
        assert!(
            matches!(
                guard.observe("read_file", true, "{}"),
                ProgressAction::Fatal(_)
            ),
            "exploration stays blocked — that is what the guard is for"
        );

        // The two escapes the message demands must work.
        assert!(
            matches!(
                guard.observe("write_file", true, r#"{"ok":true}"#),
                ProgressAction::Continue { nudge: None }
            ),
            "a latched guard must not block the edit it just demanded"
        );
        assert!(
            matches!(
                guard.observe(liberado_executor::SUBMIT_REPORT_TOOL, true, "{}"),
                ProgressAction::Continue { nudge: None }
            ),
            "nor the report it offered as the alternative"
        );

        // And a successful edit means the stall is over: normal operation resumes.
        assert!(
            guard.fatal().is_none(),
            "a successful mutation ends a read-only stall by definition"
        );
        assert!(matches!(
            guard.observe("read_file", true, "{}"),
            ProgressAction::Continue { .. }
        ));
    }

    #[test]
    fn mutation_resets_read_only_streak() {
        let mut guard = ProgressGuard::new(policy());
        guard.observe("list_files", true, "{}");
        guard.observe("read_file", true, "{}");
        assert!(matches!(
            guard.observe("write_file", true, r#"{"ok":true}"#),
            ProgressAction::Continue { nudge: None }
        ));
        // Post-mutation inspects are allowed without re-triggering pre-mutation stall.
        assert!(matches!(
            guard.observe("list_files", true, "{}"),
            ProgressAction::Continue { nudge: None }
        ));
        assert!(matches!(
            guard.observe("read_file", true, "{}"),
            ProgressAction::Continue { nudge: None }
        ));
        assert!(guard.fatal().is_none());
    }

    #[test]
    fn validation_churn_is_fatal() {
        // Isolate validation_repeat_limit from read-only / same-tool guards.
        let mut policy = policy();
        policy.same_tool_limit = 100;
        policy.read_only_turn_limit = 100;
        let mut guard = ProgressGuard::new(policy);
        let fail = r#"{"passed":false,"exit_code":1,"stdout":"","stderr":"boom"}"#;
        assert!(matches!(
            guard.observe("validate", true, fail),
            ProgressAction::Continue { nudge: None }
        ));
        assert!(matches!(
            guard.observe("validate", true, fail),
            ProgressAction::Continue { nudge: None }
        ));
        // streak 3 > limit 2
        assert!(matches!(
            guard.observe("validate", true, fail),
            ProgressAction::Fatal(ProgressFatal::ValidationChurn { repeats: 3, .. })
        ));
    }

    #[test]
    fn run_command_is_not_same_tool_churn() {
        let mut guard = ProgressGuard::new(policy());
        guard.observe("write_file", true, "{}");
        for _ in 0..20 {
            let action = guard.observe("run_command", true, r#"{"exit_code":0}"#);
            assert!(
                matches!(action, ProgressAction::Continue { .. }),
                "distinct run_command calls are not one tool: {action:?}"
            );
        }
        assert!(guard.fatal().is_none());
    }

    #[test]
    fn same_tool_nudge_then_fatal() {
        let mut guard = ProgressGuard::new(policy());
        // Also advances read-only; use mutation first so only same-tool fires.
        guard.observe("write_file", true, "{}");
        assert!(matches!(
            guard.observe("read_file", true, "{}"),
            ProgressAction::Continue { nudge: None }
        ));
        let second = guard.observe("read_file", true, "{}");
        assert!(matches!(
            second,
            ProgressAction::Continue { nudge: Some(_) }
        ));
        guard.observe("read_file", true, "{}");
        let fourth = guard.observe("read_file", true, "{}");
        assert!(matches!(
            fourth,
            ProgressAction::Fatal(ProgressFatal::SameToolChurn { .. })
        ));
    }
}

#[cfg(test)]
#[path = "progress_survivor_tests.rs"]
mod survivor_tests;
