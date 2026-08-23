//! Split from `permission.rs` for module-health boundaries.

use super::*;
use liberado_provider::ToolDef;

struct StubAsk(PermissionDecision);

#[async_trait]
impl PermissionAsk for StubAsk {
    async fn ask(
        &self,
        _session_id: &str,
        _program: &str,
        _args: &[String],
    ) -> Result<PermissionDecision, String> {
        Ok(self.0)
    }
}

struct DenyGit;

#[async_trait]
impl ToolRuntime for DenyGit {
    fn catalog(&self) -> Vec<ToolDef> {
        vec![ToolDef::new(
            "run_command",
            "run",
            json!({ "type": "object" }),
        )]
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if call.name == "run_command" {
            return Ok("ran".into());
        }
        Err("unknown".into())
    }
}

/// Temp workspace + temp machine-wide grant dir so tests never read the developer's home.
struct IsolatedGrants {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    global: PathBuf,
}

impl IsolatedGrants {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = root.path().join("workspace");
        let global = root.path().join("global-liberado");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        std::fs::create_dir_all(&global).expect("global grant dir");
        Self {
            _root: root,
            workspace,
            global,
        }
    }

    fn attach(&self, decision: PermissionDecision) -> PermissionAttach {
        PermissionAttach {
            ask: Arc::new(StubAsk(decision)),
            session_id: "s1".into(),
            client_cwd: self.workspace.clone(),
            global_grant_dir: Some(self.global.clone()),
            policy: CommandPolicy::default(),
        }
    }
}

#[test]
fn parse_selected_option_ids() {
    let once = json!({"outcome": {"outcome": "selected", "optionId": "once"}});
    assert_eq!(parse_decision(&once), PermissionDecision::Once);
    let ws = json!({"outcome": {"outcome": "selected", "optionId": "workspace"}});
    assert_eq!(parse_decision(&ws), PermissionDecision::Workspace);
    let all = json!({"outcome": {"outcome": "selected", "optionId": "everywhere"}});
    assert_eq!(parse_decision(&all), PermissionDecision::Everywhere);
    let deny = json!({"outcome": {"outcome": "selected", "optionId": "deny"}});
    assert_eq!(parse_decision(&deny), PermissionDecision::Deny);
    let cancelled = json!({"outcome": {"outcome": "cancelled"}});
    assert_eq!(parse_decision(&cancelled), PermissionDecision::Cancelled);
}

#[test]
fn question_params_are_a_paseo_chooser() {
    let params = question_params(
        "sid",
        "Which crate?",
        &["acp-bridge".into(), "coder-agent".into()],
    );
    let options = params["options"].as_array().expect("options");
    let always: Vec<_> = options
        .iter()
        .filter(|o| o["kind"] == "allow_always")
        .collect();
    assert_eq!(
        always.len(),
        2,
        "two allow_always kinds show the question text"
    );
    assert_eq!(
        params["toolCall"]["content"][0]["content"]["text"],
        "Which crate?"
    );
    let selected = json!({"outcome": {"outcome": "selected", "optionId": "opt-1"}});
    assert_eq!(
        parse_question_answer(&selected, &["acp-bridge".into(), "coder-agent".into()]).unwrap(),
        "coder-agent"
    );
}

#[test]
fn permission_params_are_a_paseo_chooser() {
    let params = permission_params("sid", "git", &["rebase".into(), "main".into()]);
    let options = params["options"].as_array().expect("options");
    assert_eq!(options.len(), 4);
    let always: Vec<_> = options
        .iter()
        .filter(|o| o["kind"] == "allow_always")
        .collect();
    assert_eq!(
        always.len(),
        2,
        "two allow_always kinds flip Paseo into chooser mode so the question text shows"
    );
    let text = params["toolCall"]["content"][0]["content"]["text"]
        .as_str()
        .unwrap_or("");
    assert!(text.contains("git rebase main"), "{text}");
    assert!(text.contains("Allow it to run"), "{text}");
}

#[tokio::test]
async fn deny_does_not_run_the_command() {
    let grants = IsolatedGrants::new();
    let runtime = wrap(
        Arc::new(DenyGit),
        CommandGrantSet::default(),
        grants.attach(PermissionDecision::Deny),
    );
    let err = runtime
        .invoke(&ToolInvocation::new(
            "1",
            "run_command",
            json!({"program": "git", "args": ["rebase", "main"]}),
        ))
        .await
        .expect_err("deny must refuse");
    assert!(err.contains("denied"), "{err}");
}

#[tokio::test]
async fn once_runs_and_does_not_persist() {
    let grants = IsolatedGrants::new();
    let session_grants = CommandGrantSet::default();
    let runtime = wrap(
        Arc::new(DenyGit),
        session_grants.clone(),
        grants.attach(PermissionDecision::Once),
    );
    let out = runtime
        .invoke(&ToolInvocation::new(
            "1",
            "run_command",
            json!({"program": "git", "args": ["rebase", "main"]}),
        ))
        .await
        .expect("once must run");
    assert_eq!(out, "ran");
    assert!(
        !workspace_grants_path(&grants.workspace).exists(),
        "once must not write a workspace grants file"
    );
    assert!(
        !global_grants_path(Some(&grants.global)).exists(),
        "once must not write a machine-wide grants file"
    );
    assert!(
        !session_grants.contains("git"),
        "once must not leave the program granted after the call"
    );
}

#[tokio::test]
async fn workspace_persist_writes_the_stem() {
    let grants = IsolatedGrants::new();
    let runtime = wrap(
        Arc::new(DenyGit),
        CommandGrantSet::default(),
        grants.attach(PermissionDecision::Workspace),
    );
    runtime
        .invoke(&ToolInvocation::new(
            "1",
            "run_command",
            json!({"program": "git.exe", "args": ["rebase"]}),
        ))
        .await
        .expect("workspace allow must run");
    let path = workspace_grants_path(&grants.workspace);
    let raw = std::fs::read_to_string(&path).expect("grants file");
    assert!(raw.contains("\"git\""), "{raw}");
    let loaded = CommandGrantSet::default();
    load_persisted(&loaded, &grants.attach(PermissionDecision::Deny));
    assert!(loaded.contains("git"));
}

#[tokio::test]
async fn a_preloaded_machine_grant_skips_the_prompt() {
    let grants = IsolatedGrants::new();
    let global_file = global_grants_path(Some(&grants.global));
    std::fs::create_dir_all(global_file.parent().unwrap()).unwrap();
    std::fs::write(&global_file, r#"{"programs":["git"]}"#).unwrap();
    let runtime = wrap(
        Arc::new(DenyGit),
        CommandGrantSet::default(),
        grants.attach(PermissionDecision::Deny),
    );
    let out = runtime
        .invoke(&ToolInvocation::new(
            "1",
            "run_command",
            json!({"program": "git", "args": ["rebase"]}),
        ))
        .await
        .expect("a machine grant must bypass policy without asking");
    assert_eq!(out, "ran");
}

#[test]
fn only_command_invocations_carry_a_program() {
    // Other tools pass straight through to the inner runtime.
    assert!(program_from(&ToolInvocation::new("1", "read_file", json!({}))).is_none());
    // A command without a program (or with a blank one) is not askable.
    assert!(
        program_from(&ToolInvocation::new("1", "run_command", json!({}))).is_none(),
        "no program key"
    );
    // A command with an empty program is not askable.
    assert!(
        program_from(&ToolInvocation::new(
            "1",
            "run_command",
            json!({ "program": "" })
        ))
        .is_none(),
        "empty program"
    );
    // Only truly empty is rejected here; a whitespace name flows through and fails
    // downstream, where the policy lookup cannot match it.
    let (program, _) = program_from(&ToolInvocation::new(
        "1",
        "run_command",
        json!({ "program": "  " }),
    ))
    .expect("whitespace is passed through untouched");
    assert_eq!(program, "  ");
    // Args are optional; non-string entries are dropped rather than crashing.
    let (program, args) = program_from(&ToolInvocation::new(
        "1",
        "run_command_background",
        json!({ "program": "git", "args": ["log", 7, null] }),
    ))
    .expect("background commands are permissioned too");
    assert_eq!(program, "git");
    assert_eq!(args, vec!["log".to_string()]);
}

#[test]
fn grant_stems_are_basenames_without_exe_and_lowercase() {
    assert_eq!(program_stem("git"), "git");
    assert_eq!(program_stem("C:\\Tools\\Git.EXE"), "git");
    assert_eq!(program_stem("/usr/local/bin/hg"), "hg");
    assert_eq!(
        program_stem(""),
        "",
        "an empty program stems to empty, never panics"
    );
}

#[test]
fn reply_ids_key_the_same_whatever_the_json_type() {
    assert_eq!(id_key(&json!("lib-perm-1")), "lib-perm-1");
    assert_eq!(
        id_key(&json!(42)),
        "42",
        "numeric client ids match string keys"
    );
    assert_eq!(id_key(&json!(null)), "null");
}

#[test]
fn question_answers_cover_skip_other_out_of_range_and_cancelled() {
    let opts = vec!["alpha".to_string(), "beta".to_string()];
    assert_eq!(
        parse_question_answer(
            &json!({"outcome": {"outcome": "selected", "optionId": "opt-0"}}),
            &opts
        )
        .unwrap(),
        "alpha"
    );
    assert_eq!(
        parse_question_answer(
            &json!({"outcome": {"outcome": "selected", "optionId": "other"}}),
            &opts
        )
        .unwrap(),
        "the human chose something else; they may type it in the next message"
    );
    for (reply, why) in [
        (
            json!({"outcome": {"outcome": "cancelled"}}),
            "cancelled dismisses",
        ),
        (
            json!({"outcome": {"outcome": "selected", "optionId": "skip"}}),
            "skip refuses",
        ),
        (
            json!({"outcome": {"outcome": "selected", "optionId": "opt-9"}}),
            "an out-of-range index is not a choice",
        ),
        (json!({"outcome": {}}), "no optionId is unrecognised"),
        (json!({}), "a bare reply is unrecognised"),
    ] {
        let err =
            parse_question_answer(&reply, &opts).expect_err(&format!("{why} must not answer"));
        assert!(!err.is_empty(), "{why}");
    }
}
