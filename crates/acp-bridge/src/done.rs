//! Interactive `done` tool: same-session kickback through configured preflight.
//!
//! Offered when the covering project declares `[projects.preflight.interactive]` with at
//! least one step. The commands come from that file; this tool does not invent them.
//! A red check is a tool result. The files stay. The conversation continues.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use liberado_coder_core::{extract_failures, log_tail};
use liberado_coder_sandbox::{PreflightReport, PreflightSpec, PreflightStepResult, run_preflight};
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use serde_json::json;

/// Wire name. Keep it stable — traces and tests match on this string.
pub const DONE_TOOL: &str = "done";

/// Wrap a coding runtime with `done` when the project declared an interactive spec.
pub fn wrap(
    inner: Arc<dyn ToolRuntime>,
    workspace: PathBuf,
    spec: Option<PreflightSpec>,
) -> Arc<dyn ToolRuntime> {
    match spec {
        Some(spec) if !spec.is_empty() => Arc::new(DoneRuntime {
            inner,
            workspace,
            spec,
        }),
        _ => inner,
    }
}

struct DoneRuntime {
    inner: Arc<dyn ToolRuntime>,
    workspace: PathBuf,
    spec: PreflightSpec,
}

fn tool_def(spec: &PreflightSpec) -> ToolDef {
    let names = step_names(spec);
    ToolDef::new(
        DONE_TOOL,
        format!(
            "Call this when you have finished the work they asked. Runs the project's \
             configured interactive checks ({names}). A failure comes back as this tool's \
             result; files stay on disk. Fix them and call done again, or explain in prose \
             if you cannot."
        ),
        json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "What you finished. Optional."
                }
            }
        }),
    )
}

fn step_names(spec: &PreflightSpec) -> String {
    spec.steps
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn optional_summary(call: &ToolInvocation) -> Option<&str> {
    call.arguments
        .get("summary")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn accepted_message(report: &PreflightReport, summary: Option<&str>) -> String {
    match summary {
        Some(s) => format!("`done` accepted — {}.\n{s}", report.summary),
        None => format!("`done` accepted — {}.", report.summary),
    }
}

fn refused_message(report: &PreflightReport) -> String {
    let details = report
        .steps
        .iter()
        .filter(|s| !s.ok)
        .map(format_step_failure)
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "`done` was NOT accepted — {}. Your files are still on disk; nothing was reverted. \
         Fix the failures in this session, then call `done` again. If you cannot finish, \
         explain in prose and wait.\n\n{details}",
        report.summary
    )
}

/// The specific failing diagnostic, not the whole check log.
///
/// Same extractor as `liberado ci`: rustc spans, test names, panics. Compile
/// progress stays out. Logs the matcher does not recognise fall back to a short tail.
fn format_step_failure(step: &PreflightStepResult) -> String {
    let extracted = extract_failures(&step.log_excerpt);
    let body = if extracted.is_empty() {
        log_tail(&step.log_excerpt, 20)
    } else {
        extracted
    };
    if body.is_empty() {
        format!(
            "{}: failed (exit={:?}, timeout={})",
            step.name, step.exit_code, step.timed_out
        )
    } else {
        format!("{}:\n{body}", step.name)
    }
}

fn host_failed_message(error: &str) -> String {
    format!(
        "`done` was NOT accepted — the host could not run the configured checks ({error}). \
         Your files are still on disk. Do not try to repair this. An operator must act."
    )
}

async fn invoke_done(
    workspace: &Path,
    spec: &PreflightSpec,
    summary: Option<&str>,
) -> Result<String, String> {
    match run_preflight(workspace, spec).await {
        Ok(report) if report.ok => Ok(accepted_message(&report, summary)),
        Ok(report) => Err(refused_message(&report)),
        Err(e) => Err(host_failed_message(&e.to_string())),
    }
}

#[async_trait]
impl ToolRuntime for DoneRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        let mut tools = self.inner.catalog();
        tools.push(tool_def(&self.spec));
        tools
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if call.name == DONE_TOOL {
            return invoke_done(&self.workspace, &self.spec, optional_summary(call)).await;
        }
        self.inner.invoke(call).await
    }

    fn is_read_only(&self, tool_name: &str) -> bool {
        if tool_name == DONE_TOOL {
            return false;
        }
        self.inner.is_read_only(tool_name)
    }

    fn parks_for_human(&self, tool_name: &str) -> bool {
        if tool_name == DONE_TOOL {
            return false;
        }
        self.inner.parks_for_human(tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_sandbox::PreflightStep;
    use serde_json::json;

    struct Stub;

    #[async_trait]
    impl ToolRuntime for Stub {
        fn catalog(&self) -> Vec<ToolDef> {
            vec![ToolDef::new("read_file", "r", json!({ "type": "object" }))]
        }
        async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
            Ok(format!("stub:{}", call.name))
        }
    }

    fn spec(name: &str, run: &str) -> PreflightSpec {
        PreflightSpec::new("interactive", vec![PreflightStep::new(name, run)])
    }

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn wrap_without_spec_does_not_offer_done() {
        let names: Vec<_> = wrap(Arc::new(Stub), PathBuf::from("/tmp"), None)
            .catalog()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn wrap_with_empty_spec_does_not_offer_done() {
        let names: Vec<_> = wrap(
            Arc::new(Stub),
            PathBuf::from("/tmp"),
            Some(PreflightSpec::new("interactive", vec![])),
        )
        .catalog()
        .into_iter()
        .map(|t| t.name)
        .collect();
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn wrap_with_spec_appends_done_named_from_config() {
        let runtime = wrap(
            Arc::new(Stub),
            PathBuf::from("/tmp"),
            Some(spec("light", "exit 0")),
        );
        let tools = runtime.catalog();
        let done = tools
            .iter()
            .find(|t| t.name == DONE_TOOL)
            .expect("done must be offered");
        assert!(
            done.description.contains("light"),
            "description must name the configured step, not invent a command: {}",
            done.description
        );
        assert!(
            !done.description.to_ascii_lowercase().contains("cargo"),
            "the tool must not hard-code cargo: {}",
            done.description
        );
        assert!(!runtime.parks_for_human(DONE_TOOL));
        assert!(!runtime.is_read_only(DONE_TOOL));
    }

    #[tokio::test]
    async fn invoke_forwards_other_tools() {
        let dir = workspace();
        let runtime = wrap(
            Arc::new(Stub),
            dir.path().to_path_buf(),
            Some(spec("ok", "exit 0")),
        );
        let out = runtime
            .invoke(&ToolInvocation::new("1", "read_file", json!({})))
            .await
            .unwrap();
        assert_eq!(out, "stub:read_file");
    }

    #[tokio::test]
    async fn a_passing_configured_step_accepts_done() {
        let dir = workspace();
        let runtime = wrap(
            Arc::new(Stub),
            dir.path().to_path_buf(),
            Some(spec("ok", "exit 0")),
        );
        let out = runtime
            .invoke(&ToolInvocation::new(
                "1",
                DONE_TOOL,
                json!({ "summary": "finished the task" }),
            ))
            .await
            .expect("green configured step must accept");
        assert!(out.contains("accepted"), "{out}");
        assert!(out.contains("finished the task"), "{out}");
        assert!(
            !out.to_ascii_lowercase().contains("cargo"),
            "acceptance must not mention cargo: {out}"
        );
    }

    #[tokio::test]
    async fn a_failing_configured_step_refuses_and_keeps_the_files() {
        let dir = workspace();
        let runtime = wrap(
            Arc::new(Stub),
            dir.path().to_path_buf(),
            Some(spec("broken", "exit 1")),
        );
        let err = runtime
            .invoke(&ToolInvocation::new("1", DONE_TOOL, json!({})))
            .await
            .expect_err("red configured step must kick back");
        assert!(err.contains("NOT accepted"), "{err}");
        assert!(err.contains("still on disk"), "{err}");
        assert!(err.contains("broken"), "{err}");
        assert!(
            !err.to_ascii_lowercase().contains("cargo"),
            "kickback must not mention cargo: {err}"
        );
    }

    #[tokio::test]
    async fn host_failure_does_not_ask_the_model_to_fix_the_check() {
        let missing = PathBuf::from("/this/path/does/not/exist/liberado-done-preflight");
        let runtime = wrap(Arc::new(Stub), missing, Some(spec("ok", "exit 0")));
        let err = runtime
            .invoke(&ToolInvocation::new("1", DONE_TOOL, json!({})))
            .await
            .expect_err("missing workspace is a host failure");
        assert!(err.contains("host could not run"), "{err}");
        assert!(err.contains("operator"), "{err}");
        assert!(
            !err.contains("Fix the failures"),
            "a missing root is not a red check: {err}"
        );
    }

    fn failed_step(name: &str, log: &str) -> PreflightStepResult {
        PreflightStepResult {
            name: name.into(),
            exit_code: Some(1),
            duration_ms: 1,
            timed_out: false,
            ok: false,
            log_excerpt: log.into(),
        }
    }

    fn refused(log: &str) -> String {
        refused_message(&PreflightReport {
            profile_id: "interactive".into(),
            ok: false,
            steps: vec![failed_step("compile", log)],
            summary: "preflight 'interactive': failed at step(s) 'compile'".into(),
            duration_ms: 1,
        })
    }

    #[test]
    fn refused_message_does_not_name_a_language() {
        let shown = refused("fmt check failed");
        assert!(shown.contains("still on disk"), "{shown}");
        assert!(shown.contains("NOT accepted"), "{shown}");
        assert!(!shown.to_ascii_lowercase().contains("cargo"), "{shown}");
        assert!(!shown.contains("submit_report"), "{shown}");
    }

    #[test]
    fn refused_message_names_the_compiler_error_and_span() {
        let shown = refused(
            "\
    Checking done-kickback-sandbox v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.08s
error[E0308]: mismatched types
  --> src/lib.rs:8:18
   |
 8 |     let x: u32 = \"kickback\";
   |                  ^^^^^^^^^^ expected `u32`, found `&str`
error: could not compile `done-kickback-sandbox` (lib) due to 1 previous error
",
        );
        assert!(shown.contains("error[E0308]"), "{shown}");
        assert!(shown.contains("src/lib.rs:8:18"), "{shown}");
        assert!(shown.contains("expected `u32`, found `&str`"), "{shown}");
        assert!(
            !shown.contains("Checking"),
            "compile progress is not a focused fix: {shown}"
        );
        assert!(!shown.contains("Finished"), "{shown}");
        assert!(
            !shown.contains("log="),
            "must not dump the raw log: {shown}"
        );
    }
}
