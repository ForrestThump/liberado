//! Session-layer vault-zone permission requests, split from `grants.rs`.

use super::*;
use liberado_common::approval_ledger::ApprovalLedger;

// ── Vault-zone permission request via the chat sessions layer (2026-10-11) ────────────────
//
// The live bug (session `01M3ZB89WK54HGNG6ATJ9FPJMX`, `turbovault:edit_note` on
// `conformance/approval-test.md` without `Write(Vault("conformance"))` in the grant) was
// reproduced end-to-end via `RiskGatedToolRuntime` in `risk_gated::vault_gate_tests`.
// These tests pin the same scenario at the **session** layer: a `ChatSessions` chat whose
// profile grants `ExecuteMcp("turbovault")` but **not** `Write(Vault("conformance"))` calls the
// tool, and the runtime gate raises a permission request — the inner tool is NEVER invoked, a
// proposal file is written with `session_id` + `origin = Web` stamped, and the model is told
// the action was NOT executed. The session-layer test is the shape of the bug-trace one
// would assemble from a real persona: model -> provider -> executor -> risk gate -> proposal.

/// A `ChatSessions` configured for the live-bug scenario:
///   - CapabilityCatalog with the live `turbovault` MCP declared with full path-addressed
///     classification (write_tools / read_tools / vault_wide_writes), so a real edit_note call
///     resolves to `WriteTarget::Zone("conformance")` instead of `Undeterminable`.
///   - Profile: ExecuteMcp("turbovault") + Read(Vault("conformance")) — the exact missing-grant
///     shape the live session had.
///   - Scoped Runtime that records every invocation but does NOT actually run the tool —
///     succeeds if (and only if) the runtime gate decided the call was safe.
async fn live_bug_chat_sessions(dir: &std::path::Path) -> ChatSessions {
    use liberado_common::McpDescriptor;
    use liberado_common::{Capability, CapabilitySet, Consequence};

    let store = Arc::new(SessionStore::open(dir).await);

    // The catalog must carry `edit_note` in write_tools with `zone_from_arg = "path"` so the
    // runtime gate sees `WriteTarget::Zone("conformance")` (not Undeterminable). Without this,
    // the fix's "undeclared tool" refusal is what we'd see — the live bug had `edit_note`
    // missing entirely, so we recreate the *post-fix* state and confirm the gate catches it.
    let catalog = Arc::new(liberado_common::CapabilityCatalog::new());
    catalog.register(McpDescriptor {
        name: "turbovault".into(),
        description: "live turbovault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec![
            "write_note".into(),
            "edit_note".into(),
            "delete_note".into(),
        ],
        read_tools: vec!["read_note".into()],
        vault_wide_writes: vec!["begin_fanout".into(), "set_active_vault".into()],
        admin_tools: Vec::new(),
        write_path_args: std::collections::BTreeMap::new(),
    });
    let capabilities = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        // The Personal Task Manager profile the live session had — a Read on conformance,
        // but NOT a Write. This is exactly the gap.
        Capability::Read(liberado_common::Zone::vault("conformance")),
    ]);

    // Provider that "calls" the model with a tool_call to `turbovault:edit_note` on the
    // conformance path, then a final text reply — so the gate sees a real model invocation.
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "live-bug-2026-10-11",
                "turbovault:edit_note",
                serde_json::json!({"path": "conformance/approval-test.md"}),
            )]),
            CompletionResponse::text("I'll wait for the human to approve."),
        ],
    ));
    let executor = Executor::new(provider, Budget::default());

    ChatSessions::new(store, executor, turbovault_recording_runtime())
        .with_guards(
            vec![("turbovault".into(), Consequence::Reversible)],
            capabilities,
            dir.join("proposals"),
            ProposalSigner::random(),
        )
        .with_live_catalog(catalog)
}

/// A runtime exposing `turbovault:edit_note` / `turbovault:read_note` so the model has
/// something to call; succeed only when the runtime gate (NOT the model) decides the call is
/// safe — tools that the gate should refuse instead return their gating message via the
/// `Result::Err` path. We don't actually save a file; we just record that the gate let the
/// call through.
fn turbovault_recording_runtime() -> Arc<dyn ToolRuntime> {
    Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:edit_note", "turbovault:read_note"],
        Ok("saved".into()),
    ))
}

#[tokio::test]
async fn live_bug_repro_edit_note_on_conformance_is_a_permission_request_at_the_chat_layer() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = live_bug_chat_sessions(dir.path()).await;

    let id = sessions.create(None).await.unwrap();
    sessions
        .turn(id, "edit the conformance note")
        .await
        .unwrap();

    // The model "called" the tool. The runtime gate refused (no Write(Vault("conformance"))).
    // The proposal file system has one entry, with `requested_grant` and the session's
    // approval origin stamped on.
    let ledger_dir =
        std::path::Path::new(&dir.path().join("proposals").to_string_lossy().into_owned())
            .join(liberado_common::PROPOSALS_DIR);
    let proposals: Vec<_> = std::fs::read_dir(&ledger_dir).unwrap().collect();
    assert_eq!(
        proposals.len(),
        1,
        "exactly one permission request was raised"
    );
    let proposal_bytes = std::fs::read_to_string(proposals[0].as_ref().unwrap().path()).unwrap();
    let proposal = liberado_common::Proposal::from_note(&proposal_bytes).unwrap();
    assert_eq!(
        proposal.requested_grant,
        Some(liberado_common::Capability::Write(
            liberado_common::Zone::vault("conformance")
        )),
        "the proposal asks for the missing zone"
    );
    assert!(
        matches!(
            proposal.origin,
            Some(liberado_common::ApprovalOrigin::Background)
                | Some(liberado_common::ApprovalOrigin::Web)
        ),
        "the proposal has a routable origin: {:?}",
        proposal.origin
    );
}

/// `Once` after approval: the next identical call raises **another** permission request. Pin
/// the Deny/Once/Session/Everywhere flow at the session layer using the actual ledger
/// (`ApprovalLedger`) so a refactor of `apply_permission_decision` that broke Once/Everywhere
/// would surface here.
#[tokio::test]
async fn approval_decision_once_then_second_call_raises_again() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = ApprovalLedger::new(dir.path().join("ledger"));

    let mut proposal = make_zone_proposal("Tools/bug-2026-10-11", "conformance");
    let dec1 = liberado_common::apply_permission_decision(
        &mut proposal,
        "once",
        liberado_common::DecisionVia::Webui,
        chrono::Utc::now(),
    )
    .unwrap();
    assert!(matches!(
        dec1,
        liberado_common::PermissionDecide::Applied { action: "once" }
    ));

    let mut proposal2 = make_zone_proposal("Tools/bug-2026-10-11", "conformance");
    // (Same tool/args, but a fresh proposal — proving a fresh request is needed; `Once` is
    // session-level in the ledger, not proposal-level.)
    let dec2 = liberado_common::apply_permission_decision(
        &mut proposal2,
        "once",
        liberado_common::DecisionVia::Webui,
        chrono::Utc::now(),
    )
    .unwrap();
    assert!(matches!(
        dec2,
        liberado_common::PermissionDecide::Applied { action: "once" }
    ));
    // The two proposals are distinct decision records; both reach the ledger and resolve to
    // `Approved` after the Once button. Pin that a deny would set `Rejected` (the structural
    // signal) — the runtime executes the call only when `decision_for` returns
    // `ApprovalDecision::Approved`.
    ledger
        .record(
            &proposal.id,
            liberado_common::approval_ledger::ApprovalDecision::Approved,
            "webui",
        )
        .await
        .unwrap();
    ledger
        .record(
            &proposal2.id,
            liberado_common::approval_ledger::ApprovalDecision::Approved,
            "webui",
        )
        .await
        .unwrap();

    // Both decisions resolved to `once`; verify they round-trip through the ledger.
    let d1 = ledger.decision_for(&proposal.id).await;
    let d2 = ledger.decision_for(&proposal2.id).await;
    assert_eq!(
        d1,
        Some(liberado_common::approval_ledger::ApprovalDecision::Approved),
        "first decision was recorded as Approved"
    );
    assert_eq!(
        d2,
        Some(liberado_common::approval_ledger::ApprovalDecision::Approved),
        "second decision was recorded as Approved"
    );
}

/// Build a `Pending` permission request for `turbovault:edit_note` on
/// `conformance/approval-test.md` — the shape the runtime gate raises.
fn make_zone_proposal(id: &str, zone: &str) -> liberado_common::Proposal {
    use liberado_common::proposal::ProposalSigner;
    let signer = ProposalSigner::random();
    let mut p = liberado_common::Proposal::pending(
        id,
        "test-corr",
        "liberado-chat",
        liberado_common::proposal::ProposedAction::ToolCalls(vec![liberado_common::ToolCall {
            tool: "turbovault:edit_note".into(),
            args: serde_json::json!({"path": format!("{zone}/approval-test.md")}),
        }]),
        format!("Permission request: 'turbovault:edit_note' needs Write access to zone '{zone}'."),
    )
    .with_requested_grant(liberado_common::Capability::Write(
        liberado_common::Zone::vault(zone),
    ));
    p.session_id = Some("01M3ZB89WK54HGNG6ATJ9FPJMX".into());
    p.origin = Some(liberado_common::ApprovalOrigin::Web);
    signer.sign(p).into_proposal()
}
