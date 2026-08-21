//! Ask the human before running a command the policy denied.
//!
//! Uses ACP `session/request_permission`. Paseo maps each option's `name` onto a button and
//! treats two `allow_always` kinds as a chooser, which is how we show workspace vs everywhere
//! plus a question in `toolCall.content`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_coder_core::CommandPolicy;
use liberado_coder_sandbox::{CommandGrantSet, CommandRequest, ensure_command_allowed};
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::wire::StdoutWire;

pub const OPT_DENY: &str = "deny";
pub const OPT_ONCE: &str = "once";
pub const OPT_WORKSPACE: &str = "workspace";
pub const OPT_EVERYWHERE: &str = "everywhere";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Deny,
    Once,
    Workspace,
    Everywhere,
    Cancelled,
}

#[async_trait]
pub trait PermissionAsk: Send + Sync {
    async fn ask(
        &self,
        session_id: &str,
        program: &str,
        args: &[String],
    ) -> Result<PermissionDecision, String>;
}

pub struct PermissionAttach {
    pub ask: Arc<dyn PermissionAsk>,
    pub session_id: String,
    pub client_cwd: PathBuf,
    pub policy: CommandPolicy,
}

pub struct PermissionBroker {
    wire: Mutex<Option<Arc<StdoutWire>>>,
    pending: Mutex<std::collections::HashMap<String, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
}

impl PermissionBroker {
    pub fn new() -> Self {
        Self {
            wire: Mutex::new(None),
            pending: Mutex::new(std::collections::HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn bind_wire(&self, wire: Arc<StdoutWire>) {
        if let Ok(mut slot) = self.wire.lock() {
            *slot = Some(wire);
        }
    }

    pub fn complete(&self, id: &Value, result: Option<Value>, error: Option<Value>) {
        let key = id_key(id);
        let waiter = self.pending.lock().ok().and_then(|mut m| m.remove(&key));
        let Some(tx) = waiter else {
            tracing::debug!(id = %key, "permission reply with no waiter");
            return;
        };
        let msg = if let Some(err) = error {
            Err(err.to_string())
        } else {
            Ok(result.unwrap_or(Value::Null))
        };
        let _ = tx.send(msg);
    }

    pub fn cancel_all(&self) {
        let waiters = self
            .pending
            .lock()
            .map(|mut m| m.drain().map(|(_, tx)| tx).collect::<Vec<_>>())
            .unwrap_or_default();
        for tx in waiters {
            let _ = tx.send(Ok(json!({ "outcome": { "outcome": "cancelled" } })));
        }
    }
}

#[async_trait]
impl PermissionAsk for PermissionBroker {
    async fn ask(
        &self,
        session_id: &str,
        program: &str,
        args: &[String],
    ) -> Result<PermissionDecision, String> {
        let id_num = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("lib-perm-{id_num}");
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|e| format!("permission lock: {e}"))?;
            pending.insert(id.clone(), tx);
        }
        let wire = self
            .wire
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(|| "permission wire is not bound".to_string())?;
        let params = permission_params(session_id, program, args);
        wire.write_rpc_request(json!(id), "session/request_permission", params)?;
        let reply = rx
            .await
            .map_err(|_| "permission waiter dropped".to_string())??;
        Ok(parse_decision(&reply))
    }
}

pub fn wrap(
    inner: Arc<dyn ToolRuntime>,
    grants: CommandGrantSet,
    attach: PermissionAttach,
) -> Arc<dyn ToolRuntime> {
    load_persisted(&grants, &attach.client_cwd);
    Arc::new(PermissionRuntime {
        inner,
        grants,
        attach,
    })
}

struct PermissionRuntime {
    inner: Arc<dyn ToolRuntime>,
    grants: CommandGrantSet,
    attach: PermissionAttach,
}

fn program_from(call: &ToolInvocation) -> Option<(String, Vec<String>)> {
    if call.name != "run_command" && call.name != "run_command_background" {
        return None;
    }
    let program = call.arguments.get("program")?.as_str()?.to_string();
    if program.is_empty() {
        return None;
    }
    let args = call
        .arguments
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some((program, args))
}

fn would_deny(policy: &CommandPolicy, grants: &CommandGrantSet, program: &str, args: &[String]) -> bool {
    if grants.contains(program) {
        return false;
    }
    let mut request = CommandRequest::new(program);
    request.args = args.to_vec();
    ensure_command_allowed(policy, &request).is_err()
}

#[async_trait]
impl ToolRuntime for PermissionRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.inner.catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if let Some((program, args)) = program_from(call)
            && would_deny(&self.attach.policy, &self.grants, &program, &args)
        {
            let decision = self
                .attach
                .ask
                .ask(&self.attach.session_id, &program, &args)
                .await?;
            if decision == PermissionDecision::Once {
                self.grants.allow(&program);
                let result = self.inner.invoke(call).await;
                self.grants.revoke(&program);
                return result;
            }
            apply_decision(
                decision,
                &program,
                &self.grants,
                &self.attach.client_cwd,
            )?;
        }
        self.inner.invoke(call).await
    }

    fn is_read_only(&self, tool_name: &str) -> bool {
        self.inner.is_read_only(tool_name)
    }

    fn parks_for_human(&self, tool_name: &str) -> bool {
        self.inner.parks_for_human(tool_name)
    }
}

fn apply_decision(
    decision: PermissionDecision,
    program: &str,
    grants: &CommandGrantSet,
    client_cwd: &Path,
) -> Result<(), String> {
    match decision {
        PermissionDecision::Deny => Err(format!(
            "you denied `{program}`. The command was not run. Ask them to allow it if you still need it."
        )),
        PermissionDecision::Cancelled => Err(format!(
            "permission prompt for `{program}` was cancelled. The command was not run."
        )),
        PermissionDecision::Once => {
            grants.allow(program);
            Ok(())
        }
        PermissionDecision::Workspace => {
            grants.allow(program);
            persist_grant(workspace_grants_path(client_cwd), program)
        }
        PermissionDecision::Everywhere => {
            grants.allow(program);
            persist_grant(global_grants_path(), program)
        }
    }
}

fn persist_grant(path: PathBuf, program: &str) -> Result<(), String> {
    let mut file = load_grant_file(&path);
    let stem = program_stem(program);
    if !file.programs.iter().any(|p| p.eq_ignore_ascii_case(&stem)) {
        file.programs.push(stem);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| e.to_string())
}

fn load_persisted(grants: &CommandGrantSet, client_cwd: &Path) {
    for path in [workspace_grants_path(client_cwd), global_grants_path()] {
        for program in load_grant_file(&path).programs {
            grants.allow(&program);
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct GrantFile {
    #[serde(default)]
    programs: Vec<String>,
}

fn load_grant_file(path: &Path) -> GrantFile {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return GrantFile::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn workspace_grants_path(cwd: &Path) -> PathBuf {
    cwd.join(".liberado").join("command-grants.json")
}

fn global_grants_path() -> PathBuf {
    if let Ok(dir) = std::env::var("LIBERADO_GRANT_DIR") {
        return PathBuf::from(dir).join("command-grants.json");
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".liberado").join("command-grants.json")
}

fn program_stem(program: &str) -> String {
    let name = program
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(program);
    let trimmed = if name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        &name[..name.len() - 4]
    } else {
        name
    };
    trimmed.to_ascii_lowercase()
}

pub fn permission_params(session_id: &str, program: &str, args: &[String]) -> Value {
    let line = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let question = format!(
        "`{line}` is blocked by command policy. Allow it to run?\n\
         Once applies to this call. Workspace remembers `{program}` in this checkout. \
         Everywhere remembers `{program}` on this machine."
    );
    json!({
        "sessionId": session_id,
        "options": [
            { "optionId": OPT_DENY, "name": "Deny", "kind": "reject_once" },
            { "optionId": OPT_ONCE, "name": "Allow once", "kind": "allow_once" },
            { "optionId": OPT_WORKSPACE, "name": "Allow in this workspace", "kind": "allow_always" },
            { "optionId": OPT_EVERYWHERE, "name": "Allow everywhere", "kind": "allow_always" }
        ],
        "toolCall": {
            "toolCallId": format!("perm-{program}"),
            "title": format!("Run {line}"),
            "kind": "execute",
            "status": "pending",
            "rawInput": { "program": program, "args": args },
            "content": [{
                "type": "content",
                "content": { "type": "text", "text": question }
            }]
        }
    })
}

pub fn parse_decision(reply: &Value) -> PermissionDecision {
    let outcome = reply.get("outcome").unwrap_or(reply);
    let kind = outcome
        .get("outcome")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if kind.eq_ignore_ascii_case("cancelled") {
        return PermissionDecision::Cancelled;
    }
    let option = outcome
        .get("optionId")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match option {
        OPT_ONCE => PermissionDecision::Once,
        OPT_WORKSPACE => PermissionDecision::Workspace,
        OPT_EVERYWHERE => PermissionDecision::Everywhere,
        OPT_DENY => PermissionDecision::Deny,
        _ => PermissionDecision::Deny,
    }
}

fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
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

    fn attach(decision: PermissionDecision, cwd: PathBuf) -> PermissionAttach {
        PermissionAttach {
            ask: Arc::new(StubAsk(decision)),
            session_id: "s1".into(),
            client_cwd: cwd,
            policy: CommandPolicy::default(),
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
        let dir = tempfile::tempdir().unwrap();
        let grants = CommandGrantSet::default();
        let runtime = wrap(Arc::new(DenyGit), grants.clone(), attach(PermissionDecision::Deny, dir.path().to_path_buf()));
        let err = runtime
            .invoke(&ToolInvocation::new(
                "1",
                "run_command",
                json!({"program": "git", "args": ["rebase", "main"]}),
            ))
            .await
            .expect_err("deny must refuse");
        assert!(err.contains("denied"), "{err}");
        assert!(!grants.contains("git"));
    }

    #[tokio::test]
    async fn once_runs_and_does_not_persist() {
        let dir = tempfile::tempdir().unwrap();
        let grants = CommandGrantSet::default();
        let runtime = wrap(
            Arc::new(DenyGit),
            grants.clone(),
            attach(PermissionDecision::Once, dir.path().to_path_buf()),
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
            !workspace_grants_path(dir.path()).exists(),
            "once must not write a grants file"
        );
        assert!(
            !grants.contains("git"),
            "once must not leave the program granted after the call"
        );
    }

    #[tokio::test]
    async fn workspace_persist_writes_the_stem() {
        let dir = tempfile::tempdir().unwrap();
        let grants = CommandGrantSet::default();
        let runtime = wrap(
            Arc::new(DenyGit),
            grants.clone(),
            attach(PermissionDecision::Workspace, dir.path().to_path_buf()),
        );
        runtime
            .invoke(&ToolInvocation::new(
                "1",
                "run_command",
                json!({"program": "git.exe", "args": ["rebase"]}),
            ))
            .await
            .expect("workspace allow must run");
        let path = workspace_grants_path(dir.path());
        let raw = std::fs::read_to_string(&path).expect("grants file");
        assert!(raw.contains("\"git\""), "{raw}");
        let loaded = CommandGrantSet::default();
        load_persisted(&loaded, dir.path());
        assert!(loaded.contains("git"));
    }
}
