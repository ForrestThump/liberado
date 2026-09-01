//! Split from `risk_gated.rs`: kills the baseline campaign's survivors.
//!
//! Covers the compact permission-request id, the held-authority summary, and
//! the undeclared-zone fail-safe (no grant → deferred to a human even when the
//! zone is unknown to policy; a held `Write` still runs direct).

use super::*;
use async_trait::async_trait;
use liberado_common::{Capability, CapabilitySet, RiskWaiverSet, Zone};

/// Minimal inner runtime: records invocations, returns a fixed result.
struct MockInner {
    tools: Vec<ToolDef>,
    invoked: std::sync::Mutex<Vec<ToolInvocation>>,
    result: Result<String, String>,
}

impl MockInner {
    fn new(tool_names: &[&str], result: Result<String, String>) -> Self {
        let tools = tool_names
            .iter()
            .map(|n| ToolDef::new(*n, "test tool", serde_json::json!({ "type": "object" })))
            .collect();
        Self {
            tools,
            invoked: std::sync::Mutex::new(Vec::new()),
            result,
        }
    }
}

#[async_trait]
impl ToolRuntime for MockInner {
    fn catalog(&self) -> Vec<ToolDef> {
        self.tools.clone()
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.invoked.lock().unwrap().push(call.clone());
        self.result.clone()
    }
}

/// Notifier that always succeeds, so deferral paths take their happy route.
struct AlwaysOkNotifier;

#[async_trait]
impl Notifier for AlwaysOkNotifier {
    async fn notify(&self, _: &str) -> Result<(), liberado_notify::NotifyError> {
        Ok(())
    }
}

// ── pure helpers ────────────────────────────────────────────────────────────

#[test]
fn permission_request_ids_are_compact_and_numeric() {
    let id = permission_request_id();
    assert!(id.starts_with("perm-"), "{id}");
    let rest = &id["perm-".len()..];
    assert!(!rest.is_empty(), "{id}");
    assert!(
        rest.chars().all(|c| c.is_ascii_digit()),
        "nanos keep it short and sortable: {id}"
    );
    assert!(id.len() <= 64, "Telegram callback_data budget: {id}");
}

#[test]
fn held_summary_renders_mcps_and_both_zone_kinds() {
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::ExecuteMcp("email-mcp".into()),
        Capability::Write(Zone::Vault("tasks".into())),
        Capability::Write(Zone::Named("crm".into())),
    ]);
    assert_eq!(
        held_summary(&caps),
        "mcps=[turbovault,email-mcp] write_zones=[tasks,crm]"
    );
}

#[test]
fn held_summary_of_nothing_is_empty_brackets() {
    let caps = CapabilitySet::empty();
    assert_eq!(held_summary(&caps), "mcps=[] write_zones=[]");
}

// ── undeclared-zone fail-safe ───────────────────────────────────────────────

fn path_write_descriptor() -> McpDescriptor {
    McpDescriptor {
        name: "turbovault".into(),
        description: "path-addressed vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec!["write_note".into()],
    }
}

/// A write into a zone policy has never declared, with no held `Write`, must be
/// deferred to a human — not executed on the strength of the execute grant.
#[tokio::test]
async fn an_undeclared_zone_without_a_grant_is_deferred_not_executed() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(MockInner::new(
        &["turbovault:write_note"],
        Ok("wrote".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![path_write_descriptor()],
        Vec::new(), // no declared zones at all
        dir.path().to_path_buf(),
        "write a note".into(),
        "ta-undeclared".into(),
        ProposalSigner::random(),
        "default",
    )
    .with_notifier(Arc::new(AlwaysOkNotifier));

    let result = rt
        .invoke(&ToolInvocation::new(
            "c1",
            "turbovault:write_note",
            serde_json::json!({"path": "mystery/a.md"}),
        ))
        .await;

    let msg = result.expect("deferred, not failed");
    assert!(
        msg.contains("PERMISSION REQUESTED") || msg.contains("PROPOSAL CREATED"),
        "an ungranted write to an unknown zone goes to a human: {msg}"
    );
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "the action must not run before a human grants it"
    );
}

/// The documented exception: a held `Write(zone)` for an *undeclared* zone can
/// only come from a human tap, so that write runs directly.
#[tokio::test]
async fn a_held_undeclared_zone_write_runs_direct() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(MockInner::new(
        &["turbovault:write_note"],
        Ok("wrote".into()),
    ));
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault("mystery")),
    ]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![path_write_descriptor()],
        Vec::new(),
        dir.path().to_path_buf(),
        "write a note".into(),
        "ta-held".into(),
        ProposalSigner::random(),
        "default",
    );

    let result = rt
        .invoke(&ToolInvocation::new(
            "c1",
            "turbovault:write_note",
            serde_json::json!({"path": "mystery/a.md"}),
        ))
        .await;
    assert_eq!(result, Ok("wrote".into()), "human-granted authority holds");
    assert_eq!(inner.invoked.lock().unwrap().len(), 1);
}
