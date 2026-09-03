//! Interactive ACP coding: a lasting conversation with coding tools.
//!
//! One `session/prompt` is one executor turn, not a [`liberado_coder_agent`] pack run.
//! The one-shot `/goal` driver is [`crate::coding_run`] (ACP mode `goal`).
//!
//! Shared with the pack: [`CodingToolRuntime`], durable worktree, `[coder]` path/command
//! policy, hashline/edit, offered-tool filter, validation command.

use std::path::Path;
use std::sync::Arc;

use liberado_coder_core::{CoderCommandConfig, CoderTuning};
use liberado_coder_sandbox::{CommandRequest, PreflightSpec};
use liberado_coder_tools::CodingToolRuntime;
use liberado_executor::ToolRuntime;

use crate::coding_run;

/// What interactive coding needs that chat does not: a worktree-rooted tool runtime.
pub struct CodingConverse {
    pub tools: Arc<dyn ToolRuntime>,
    pub system: String,
}

/// Prepare the durable worktree and attach coding tools rooted there.
pub async fn prepare_coding_converse(
    cwd: &Path,
    session_id: &str,
    tuning: &CoderTuning,
    ask_human: bool,
    config_dir: Option<&Path>,
    permission: Option<crate::permission::PermissionAttach>,
) -> Result<CodingConverse, String> {
    let workspace = coding_run::prepare_workspace(cwd, session_id).await?;
    crate::workspace_targets::apply_workspace_targets(&tuning.workspace_build, cwd);
    // Project identity is the client's cwd. The durable worktree is not under the
    // declared root, so matching on it would drop the configured bar.
    let spec = coding_run::interactive_preflight_spec(config_dir, cwd);
    let runtime = coding_runtime(&workspace, tuning)?;
    let question_client = permission
        .as_ref()
        .map(|a| (Arc::clone(&a.ask), a.session_id.clone()));
    let tools: Arc<dyn ToolRuntime> = if let Some(attach) = permission {
        let grants = runtime.command_grants();
        crate::permission::wrap(Arc::new(runtime), grants, attach)
    } else {
        Arc::new(runtime)
    };
    let tools = crate::done::wrap(tools, workspace.clone(), spec.clone());
    let tools = crate::ask_human::wrap_with_client(tools, ask_human, question_client);
    let system = system_prompt(cwd, &workspace, tuning, spec.as_ref());
    Ok(CodingConverse { tools, system })
}

/// Build the same coding tool surface the pack uses, without the outer attempt loop.
pub fn coding_runtime(root: &Path, tuning: &CoderTuning) -> Result<CodingToolRuntime, String> {
    let mut runtime = CodingToolRuntime::new(
        root,
        tuning.command_policy.clone(),
        tuning.path_policy.clone(),
    )
    .map_err(|e| format!("coding tools at {}: {e}", root.display()))?
    .with_hashline(tuning.hashline.clone())
    .with_edit(tuning.edit.clone())
    .with_offered_tools(tuning.offered_tools.clone());
    if let Some(cmd) = &tuning.validation_command {
        runtime = runtime.with_validation_command(validation_request(cmd));
    }
    Ok(runtime)
}

fn validation_request(cmd: &CoderCommandConfig) -> CommandRequest {
    CommandRequest {
        program: cmd.program.clone(),
        args: cmd.args.clone(),
        env: cmd.env.clone(),
        timeout_secs: cmd.timeout_secs,
        output_max_bytes: cmd.output_max_bytes,
        offload_dir: None,
    }
}

/// System prompt for interactive coding: baked `interactive.md`, plus the workspace path.
pub fn system_prompt(
    cwd: &Path,
    workspace: &Path,
    tuning: &CoderTuning,
    spec: Option<&PreflightSpec>,
) -> String {
    let dir = tuning.prompt_dir.as_deref().map(Path::new);
    let body = liberado_coder_core::prompts::load(
        dir,
        liberado_coder_core::prompts::INTERACTIVE_FILE,
        liberado_coder_core::prompts::INTERACTIVE,
    );
    let mut text = format!(
        "{body}\n\n\
         Client cwd: {}\n\
         Workspace (tools are rooted here): {}\n\
         For an unattended run-to-terminal, switch ACP mode to **goal**.",
        cwd.display(),
        workspace.display()
    );
    append_done_steps(&mut text, spec);
    text
}

fn append_done_steps(text: &mut String, spec: Option<&PreflightSpec>) {
    let Some(spec) = spec.filter(|s| !s.is_empty()) else {
        return;
    };
    let names = spec
        .steps
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    text.push_str(&format!(
        "\n\nThis project configured `done` to run: {names}."
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::CoderCommandConfig;
    use tempfile::TempDir;

    fn tuning_with_four_tools() -> CoderTuning {
        CoderTuning {
            offered_tools: Some(vec![
                "read_file".into(),
                "write_file".into(),
                "edit_file".into(),
                "run_command".into(),
            ]),
            ..CoderTuning::default()
        }
    }

    #[test]
    fn offered_tools_from_tuning_are_the_catalog() {
        let dir = TempDir::new().unwrap();
        let runtime = coding_runtime(dir.path(), &tuning_with_four_tools()).unwrap();
        let names: Vec<String> = runtime.catalog().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec!["read_file", "write_file", "edit_file", "run_command"],
            "interactive coding must honour [coder] offered_tools the same way the pack does"
        );
    }

    #[test]
    fn full_catalog_includes_edit_and_validate() {
        let dir = TempDir::new().unwrap();
        let runtime = coding_runtime(dir.path(), &CoderTuning::default()).unwrap();
        let names: Vec<String> = runtime.catalog().into_iter().map(|t| t.name).collect();
        for needed in ["read_file", "edit_file", "write_file", "grep", "validate"] {
            assert!(
                names.iter().any(|n| n == needed),
                "{needed} missing from interactive catalog: {names:?}"
            );
        }
        assert!(
            !names.iter().any(|n| n == "submit_report"),
            "submit_report is a pack terminator; converse mode must not offer it: {names:?}"
        );
    }

    #[test]
    fn configured_validation_command_is_attached() {
        let dir = TempDir::new().unwrap();
        let tuning = CoderTuning {
            validation_command: Some(CoderCommandConfig {
                program: "echo".into(),
                args: vec!["ok".into()],
                env: Default::default(),
                timeout_secs: Some(5),
                output_max_bytes: None,
            }),
            ..CoderTuning::default()
        };
        // Building must accept the command; invoke is covered by coder-tools.
        let runtime = coding_runtime(dir.path(), &tuning).unwrap();
        assert!(
            runtime.catalog().iter().any(|t| t.name == "validate"),
            "validate stays in the catalog when a command is configured"
        );
    }

    #[test]
    fn system_prompt_names_the_workspace_and_goal_mode() {
        let text = system_prompt(
            Path::new("/client"),
            Path::new("/work/tree"),
            &CoderTuning::default(),
            None,
        );
        assert!(text.contains("/client"), "cwd must be visible: {text}");
        assert!(
            text.contains("/work/tree"),
            "tool root must be visible: {text}"
        );
        assert!(
            text.to_ascii_lowercase().contains("goal"),
            "must point at goal mode for unattended work: {text}"
        );
        assert!(
            !text.contains("then submit_report"),
            "must not instruct submit_report: {text}"
        );
        assert!(
            !text.contains("This project configured `done`"),
            "no spec means no done ritual in the prompt: {text}"
        );
    }

    #[test]
    fn system_prompt_names_configured_done_steps() {
        let spec = PreflightSpec::new(
            "interactive",
            vec![liberado_coder_sandbox::PreflightStep::new(
                "light", "echo x",
            )],
        );
        let text = system_prompt(
            Path::new("/c"),
            Path::new("/w"),
            &CoderTuning::default(),
            Some(&spec),
        );
        assert!(text.contains("light"), "{text}");
        assert!(
            !text.to_ascii_lowercase().contains("cargo"),
            "prompt must not invent cargo: {text}"
        );
    }

    #[test]
    fn system_prompt_loads_an_on_disk_override() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("interactive.md"), "DISK OVERRIDE PROMPT").unwrap();
        let tuning = CoderTuning {
            prompt_dir: Some(dir.path().display().to_string()),
            ..CoderTuning::default()
        };
        let text = system_prompt(Path::new("/c"), Path::new("/w"), &tuning, None);
        assert!(
            text.contains("DISK OVERRIDE PROMPT"),
            "prompt_dir must win over the baked copy: {text}"
        );
    }

    #[tokio::test]
    async fn prepare_on_non_git_uses_cwd() {
        let dir = TempDir::new().unwrap();
        let prepared = prepare_coding_converse(
            dir.path(),
            "sess-interactive",
            &CoderTuning::default(),
            false,
            None,
            None,
        )
        .await
        .unwrap();
        let names: Vec<String> = prepared
            .tools
            .catalog()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(names.contains(&"read_file".into()), "{names:?}");
        assert!(
            prepared.system.contains(&dir.path().display().to_string()),
            "tools must be rooted at the client cwd when it is not a git repo: {}",
            prepared.system
        );
        assert!(
            !names.iter().any(|n| n == "ask_human"),
            "ask_human=false must not offer the tool: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "done"),
            "no interactive profile means no done tool: {names:?}"
        );
    }

    #[tokio::test]
    async fn prepare_can_offer_ask_human() {
        let dir = TempDir::new().unwrap();
        let prepared = prepare_coding_converse(
            dir.path(),
            "sess-ask",
            &CoderTuning::default(),
            true,
            None,
            None,
        )
        .await
        .unwrap();
        let names: Vec<String> = prepared
            .tools
            .catalog()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(
            names.iter().any(|n| n == "ask_human"),
            "ask_human=true must offer the tool: {names:?}"
        );
        assert!(prepared.tools.parks_for_human("ask_human"));
    }

    fn toml_path(p: &Path) -> String {
        let escaped = p.display().to_string().replace('\\', "\\\\");
        format!("\"{escaped}\"")
    }

    #[tokio::test]
    async fn prepare_offers_done_when_interactive_preflight_is_configured() {
        let cwd = TempDir::new().unwrap();
        let cfg = TempDir::new().unwrap();
        std::fs::write(
            cfg.path().join("topology.toml"),
            format!(
                "vault_path = \"/tmp/vault\"\n\n\
                 [[projects]]\n\
                 name = \"fixture\"\n\
                 root = {root}\n\n\
                 [projects.preflight.interactive]\n\
                 steps = [{{ name = \"only\", run = \"exit 0\" }}]\n",
                root = toml_path(cwd.path()),
            ),
        )
        .unwrap();
        let prepared = prepare_coding_converse(
            cwd.path(),
            "sess-done",
            &CoderTuning::default(),
            false,
            Some(cfg.path()),
            None,
        )
        .await
        .unwrap();
        let names: Vec<String> = prepared
            .tools
            .catalog()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(
            names.iter().any(|n| n == "done"),
            "configured interactive preflight must offer done: {names:?}"
        );
        assert!(
            prepared.system.contains("only"),
            "prompt must name the configured step: {}",
            prepared.system
        );
    }

    #[tokio::test]
    async fn prepare_does_not_offer_done_for_a_ship_only_project() {
        let cwd = TempDir::new().unwrap();
        let cfg = TempDir::new().unwrap();
        std::fs::write(
            cfg.path().join("topology.toml"),
            format!(
                "vault_path = \"/tmp/vault\"\n\n\
                 [[projects]]\n\
                 name = \"fixture\"\n\
                 root = {root}\n\n\
                 [projects.preflight.ship]\n\
                 steps = [{{ name = \"full\", run = \"exit 0\" }}]\n",
                root = toml_path(cwd.path()),
            ),
        )
        .unwrap();
        let prepared = prepare_coding_converse(
            cwd.path(),
            "sess-ship-only",
            &CoderTuning::default(),
            false,
            Some(cfg.path()),
            None,
        )
        .await
        .unwrap();
        let names: Vec<String> = prepared
            .tools
            .catalog()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(
            !names.iter().any(|n| n == "done"),
            "ship steps must not become the interactive done bar: {names:?}"
        );
    }
}
