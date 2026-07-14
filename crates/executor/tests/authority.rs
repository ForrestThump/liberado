//! The authority conformance suite: **withholding a capability must refuse the action.**
//!
//! Every capability in a grant is a claim that the system will *stop* something. Failure class §2 in
//! `docs/architecture/failure-modes.md` is that such a claim can be entirely decorative and nobody
//! notices, because the tests all *grant* the capability and check the happy path. `Capability::Write`
//! was never consulted at the MCP boundary for months: a dispatch profile granted `Read`, explicitly
//! denied `Write`, and wrote to the vault. Every test passed. The guard was not weak — it was absent.
//!
//! So this suite is written the other way round: for each capability, **take it away** and assert the
//! action is refused. A capability with no entry here is a capability nobody has proven does anything.
//!
//! Refusal here means `Err` — an authority failure, not a proposal downgrade. The risk guards ask
//! "this is permitted, but is it safe to do directly?", a question that only makes sense once
//! "is this permitted at all?" is yes.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{
    Capability, CapabilitySet, Consequence, McpDescriptor, ProposalSigner, WriteClass, Zone,
};
use liberado_executor::{RiskGatedToolRuntime, ToolRuntime};
use liberado_provider::{ToolDef, ToolInvocation};

/// An inner runtime that records whether it was ever actually reached. A "refusal" that still ran the
/// tool is not a refusal — and asserting only on the returned `Err` would not notice.
struct SpyRuntime {
    ran: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolRuntime for SpyRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        vec![]
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.ran.lock().unwrap().push(call.name.clone());
        Ok("the tool ran".into())
    }
}

/// A path-addressed vault MCP, like TurboVault: `write_note` lands in whichever zone its `path` names.
fn vault_descriptor() -> McpDescriptor {
    McpDescriptor {
        name: "vault".into(),
        description: "path-addressed vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec!["write_note".into()],
    }
}

/// Build the gate under an explicit grant, and hand back the spy so a test can prove the inner tool
/// was never reached.
fn gate(caps: CapabilitySet) -> (RiskGatedToolRuntime, Arc<std::sync::Mutex<Vec<String>>>) {
    let ran = Arc::new(std::sync::Mutex::new(Vec::new()));
    let inner = Arc::new(SpyRuntime { ran: ran.clone() });
    let rt = RiskGatedToolRuntime::new(
        inner,
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        vec![vault_descriptor()],
        // `tasks` is freely agent-writable: the RISK guards would happily pass every call below.
        // Any refusal therefore comes from AUTHORITY, which is what this suite is about.
        vec![("tasks".to_string(), WriteClass::AgentWritable)],
        std::env::temp_dir(),
        "write a note".into(),
        "authority-suite".into(),
        ProposalSigner::random(),
        "default",
    );
    (rt, ran)
}

fn write_note() -> ToolInvocation {
    ToolInvocation::new(
        "c1",
        "vault:write_note",
        serde_json::json!({"path": "tasks/a.md", "content": "x"}),
    )
}

#[tokio::test]
async fn withholding_execute_mcp_refuses_the_call() {
    // The only check that ever worked. Pinned so it cannot regress into decoration like Write did.
    let caps = CapabilitySet::from_iter([Capability::Write(Zone::vault("tasks"))]);
    let (rt, ran) = gate(caps);

    let err = rt
        .invoke(&write_note())
        .await
        .expect_err("an MCP that was not granted must not be callable");
    assert!(err.contains("not authorized"), "{err}");
    assert!(ran.lock().unwrap().is_empty(), "and the tool must never run");
}

#[tokio::test]
async fn withholding_write_refuses_the_write() {
    // F1, the live failure, as a permanent gate. `ExecuteMcp` says you may CALL the MCP. It does not
    // say you may WRITE with it — and for months nothing else said so either.
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("vault".into()),
        Capability::Read(Zone::vault("tasks")),
    ]);
    let (rt, ran) = gate(caps);

    let err = rt
        .invoke(&write_note())
        .await
        .expect_err("calling an MCP is not permission to write with it");
    assert!(err.contains("not authorized"), "{err}");
    assert!(err.contains("tasks"), "the refusal must name the zone: {err}");
    assert!(ran.lock().unwrap().is_empty(), "and the tool must never run");
}

#[tokio::test]
async fn write_in_one_zone_does_not_authorize_a_write_in_another() {
    // The reason zones are resolved from the call's ARGUMENTS and not from the tool's name: one
    // `write_note` can land anywhere. A tool→zone map would have made `Write(tasks)` a skeleton key.
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("vault".into()),
        Capability::Write(Zone::vault("tasks")),
    ]);
    let (rt, ran) = gate(caps);

    let err = rt
        .invoke(&ToolInvocation::new(
            "c2",
            "vault:write_note",
            serde_json::json!({"path": "decisions/b.md", "content": "x"}),
        ))
        .await
        .expect_err("Write(tasks) must not authorize a write to decisions/");
    assert!(err.contains("decisions"), "{err}");
    assert!(ran.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_write_whose_zone_cannot_be_determined_is_refused() {
    // Fail closed. A write we cannot place is a write we cannot authorize — and folding "I don't
    // know" into "not a write" is precisely how the guard came to be silently absent.
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("vault".into()),
        Capability::Write(Zone::vault("tasks")),
    ]);
    let (rt, ran) = gate(caps);

    let err = rt
        .invoke(&ToolInvocation::new(
            "c3",
            "vault:write_note",
            serde_json::json!({"path": "loose.md", "content": "x"}),
        ))
        .await
        .expect_err("a path naming no zone cannot be authorized");
    assert!(err.contains("not authorized"), "{err}");
    assert!(ran.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_grant_that_holds_everything_is_allowed_through() {
    // The positive control, and it is not optional. Without it, every assertion above would still
    // pass if the gate simply refused *everything* — which would be a catastrophic bug that looks
    // exactly like perfect security. This is the arm that makes the others mean something.
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("vault".into()),
        Capability::Write(Zone::vault("tasks")),
    ]);
    let (rt, ran) = gate(caps);

    let out = rt.invoke(&write_note()).await;
    assert_eq!(
        out,
        Ok("the tool ran".into()),
        "a fully-granted write must succeed — enforcement is not the same as breakage"
    );
    assert_eq!(ran.lock().unwrap().len(), 1, "and the tool must actually have run");
}

#[tokio::test]
async fn a_read_on_a_writing_mcp_needs_no_write_capability() {
    // The other way a guard goes wrong: over-refusing. `read_note` carries a path too, and if the
    // gate could not tell it from `write_note` it would demand `Write` to read — which would make
    // the capability system unusable and get it switched off, which is how guards die.
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("vault".into()),
        Capability::Read(Zone::vault("tasks")),
    ]);
    let (rt, ran) = gate(caps);

    let out = rt
        .invoke(&ToolInvocation::new(
            "c4",
            "vault:read_note",
            serde_json::json!({"path": "tasks/a.md"}),
        ))
        .await;
    assert_eq!(out, Ok("the tool ran".into()), "a read is not a write");
    assert_eq!(ran.lock().unwrap().len(), 1);
}
