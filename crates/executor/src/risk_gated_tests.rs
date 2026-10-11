use super::*;
use liberado_common::Capability;
use liberado_test_support::{InvocationRecordingRuntime, MockNotifier};

fn test_runtime(
    inner: impl ToolRuntime + 'static,
    capabilities: CapabilitySet,
    consequence_catalog: &[(&str, Consequence)],
) -> RiskGatedToolRuntime {
    let catalog: Vec<(String, Consequence)> = consequence_catalog
        .iter()
        .map(|(n, c)| (n.to_string(), *c))
        .collect();

    RiskGatedToolRuntime::new(
        Arc::new(inner),
        capabilities,
        catalog,
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        "test goal".into(),
        "test-correlation".into(),
        ProposalSigner::random(),
        "default",
    )
}

/// The gate must ask the **tool-level** question. It previously asked `grants_mcp`, which for a
/// partial grant answers "yes, that MCP is reachable" and would have waved through every tool on
/// a server the grant only meant to open a crack of.
#[tokio::test]
async fn a_per_tool_grant_gates_the_rest_of_that_mcp() {
    let inner = InvocationRecordingRuntime::new(
        &["turbovault:read_note", "turbovault:write_note"],
        Ok("ok".into()),
    );
    let caps = CapabilitySet::from_iter([Capability::ExecuteTool("turbovault:read_note".into())]);
    let rt = test_runtime(inner, caps, &[("turbovault", Consequence::ReadOnly)]);

    let granted = ToolInvocation::new("c1", "turbovault:read_note", serde_json::json!({}));
    assert_eq!(rt.invoke(&granted).await, Ok("ok".into()));

    let ungranted = ToolInvocation::new("c2", "turbovault:write_note", serde_json::json!({}));
    let err = rt.invoke(&ungranted).await.unwrap_err();
    assert!(err.contains("not authorized"), "got: {err}");
}

/// And a server-wide grant must keep working - the coarse form is still the common case.
#[tokio::test]
async fn a_server_grant_still_authorizes_every_tool_on_it() {
    let inner = InvocationRecordingRuntime::new(
        &["turbovault:read_note", "turbovault:write_note"],
        Ok("ok".into()),
    );
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
    let rt = test_runtime(inner, caps, &[("turbovault", Consequence::ReadOnly)]);

    for tool in ["turbovault:read_note", "turbovault:write_note"] {
        let call = ToolInvocation::new("c1", tool, serde_json::json!({}));
        assert_eq!(rt.invoke(&call).await, Ok("ok".into()), "{tool}");
    }
}

#[tokio::test]
async fn low_consequence_call_passes_through() {
    let inner = InvocationRecordingRuntime::new(&["my-mcp:read"], Ok("data".into()));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("my-mcp".into())]);
    let rt = test_runtime(inner, caps, &[("my-mcp", Consequence::ReadOnly)]);

    let call = ToolInvocation::new("c1", "my-mcp:read", serde_json::json!({}));
    let result = rt.invoke(&call).await;
    assert_eq!(result, Ok("data".into()));
}

#[tokio::test]
async fn high_consequence_call_is_downgraded_to_proposal() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["email-mcp:send"],
        Ok("sent".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("email-mcp".into())]);
    let signer = ProposalSigner::random();
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("email-mcp".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        dir.path().to_path_buf(),
        "send an email".into(),
        "test-email".into(),
        signer.clone(),
        "default",
    );

    let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({"to": "boss"}));
    let result = rt.invoke(&call).await;
    // A downgrade is a tool *result* (Ok), not an error - so the model relays it cleanly.
    let msg = result.expect("downgrade should be an Ok tool result, not an Err");
    assert!(
        msg.contains("PROPOSAL CREATED") && msg.contains("did not run"),
        "message must state the action did not run: {msg}"
    );

    // The inner tool must NOT have run.
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "high-consequence call must not invoke the inner tool"
    );

    // Verify the proposal file was written, and it's signed with the runtime's own signer.
    let proposals_dir = dir.path().join("proposals");
    let mut entries = tokio::fs::read_dir(&proposals_dir).await.unwrap();
    let entry = entries
        .next_entry()
        .await
        .unwrap()
        .expect("proposal file should exist");
    let content = tokio::fs::read_to_string(entry.path()).await.unwrap();
    let written = liberado_common::Proposal::from_note(&content).unwrap();
    assert!(
        signer.verify(&written),
        "the written proposal must verify against the runtime's own signer"
    );
}

/// Build a high-consequence runtime, optionally with a notifier, for the deferral-flag tests.
fn downgrade_runtime(
    dir: &std::path::Path,
    notifier: Option<MockNotifier>,
) -> RiskGatedToolRuntime {
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["email-mcp:send"],
        Ok("sent".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("email-mcp".into())]);
    let mut rt = RiskGatedToolRuntime::new(
        inner,
        caps,
        vec![("email-mcp".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        dir.to_path_buf(),
        "send an email".into(),
        "test-deferral".into(),
        ProposalSigner::random(),
        "default",
    );
    if let Some(n) = notifier {
        rt = rt.with_notifier(Arc::new(n));
    }
    rt
}

#[tokio::test]
async fn downgrade_with_a_confirmed_notify_records_out_of_band_deferral() {
    // Gap 2: a proposal downgrade whose out-of-band notification actually sent must flag the
    // deferral, so a chat surface can drop the redundant "needs approval" reply.
    let dir = tempfile::TempDir::new().unwrap();
    let rt = downgrade_runtime(dir.path(), Some(MockNotifier { ok: true }));
    let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({"to": "boss"}));
    rt.invoke(&call).await.expect("downgrade is an Ok result");
    assert!(
        rt.took_deferral_to_human(),
        "a confirmed out-of-band notify must record the deferral"
    );
}

#[tokio::test]
async fn downgrade_without_a_notifier_records_no_deferral() {
    // No out-of-band channel -> the chat reply is the only signal and must NOT be suppressed.
    let dir = tempfile::TempDir::new().unwrap();
    let rt = downgrade_runtime(dir.path(), None);
    let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({"to": "boss"}));
    rt.invoke(&call).await.expect("downgrade is an Ok result");
    assert!(
        !rt.took_deferral_to_human(),
        "with no notifier there is no out-of-band surfacing to defer to"
    );
}

#[tokio::test]
async fn downgrade_whose_notify_failed_records_no_deferral() {
    // The human got no ping - suppressing the chat reply would leave them with nothing, so the
    // flag must stay clear even though a proposal note was written.
    let dir = tempfile::TempDir::new().unwrap();
    let rt = downgrade_runtime(dir.path(), Some(MockNotifier { ok: false }));
    let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({"to": "boss"}));
    rt.invoke(&call)
        .await
        .expect("downgrade is still an Ok result");
    assert!(
        !rt.took_deferral_to_human(),
        "a failed notify must not record a deferral (chat reply is the fallback)"
    );
}

#[tokio::test]
async fn permission_request_with_a_confirmed_notify_records_out_of_band_deferral() {
    // The primary Gap 2 scenario: a Write with no Write capability raises a permission request
    // (four scope buttons) instead of a hard refusal when a notifier is wired - and records the
    // out-of-band surfacing so the face agent drops the duplicate "grant permission" reply.
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["vault:write_review"],
        Ok("wrote".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner,
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        vec![vault_descriptor()],
        vec![("reviews".to_string(), WriteClass::AgentWritable)],
        dir.path().to_path_buf(),
        "write a review note".into(),
        "test-perm-deferral".into(),
        ProposalSigner::random(),
        "default",
    )
    .with_notifier(Arc::new(MockNotifier { ok: true }));

    let call = ToolInvocation::new("c1", "vault:write_review", serde_json::json!({"c": "..."}));
    let msg = rt
        .invoke(&call)
        .await
        .expect("with a notifier a missing-Write becomes a permission request, not an Err");
    assert!(msg.contains("PERMISSION REQUESTED"), "{msg}");
    assert!(
        rt.took_deferral_to_human(),
        "a confirmed permission-request notify must record the deferral"
    );
}

#[tokio::test]
async fn out_of_capability_call_is_rejected() {
    let inner = InvocationRecordingRuntime::new(&["email-mcp:send"], Ok("sent".into()));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]); // email not granted
    let rt = test_runtime(inner, caps, &[("email-mcp", Consequence::Reversible)]);

    let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({}));
    let result = rt.invoke(&call).await;
    assert!(result.is_err(), "ungranted call should be rejected");
    assert!(result.unwrap_err().contains("not authorized"));
}

#[tokio::test]
async fn catalog_delegates_to_inner() {
    let inner = InvocationRecordingRuntime::new(&["my-mcp:read", "my-mcp:write"], Ok("ok".into()));
    let rt = test_runtime(
        inner,
        CapabilitySet::empty(),
        &[("my-mcp", Consequence::ReadOnly)],
    );

    let catalog = rt.catalog();
    assert_eq!(catalog.len(), 2);
    let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"my-mcp:read"));
    assert!(names.contains(&"my-mcp:write"));
}

#[tokio::test]
async fn a_proposal_write_failure_is_a_real_error_not_a_silent_ok() {
    // proposals_dir points at a path whose parent component is an existing *file*, so
    // create_dir_all(proposals_dir/"proposals") cannot succeed - this must surface as a real
    // Err, not a fabricated "PROPOSAL CREATED" success with nothing actually written.
    let dir = tempfile::TempDir::new().unwrap();
    let occupied_by_a_file = dir.path().join("occupied");
    tokio::fs::write(&occupied_by_a_file, b"not a directory")
        .await
        .unwrap();

    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["email-mcp:send"],
        Ok("sent".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("email-mcp".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("email-mcp".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        occupied_by_a_file,
        "send an email".into(),
        "test-write-failure".into(),
        ProposalSigner::random(),
        "default",
    );

    let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({"to": "boss"}));
    let result = rt.invoke(&call).await;
    assert!(
        result.is_err(),
        "a genuine write failure must surface as Err, not a fabricated success message"
    );
    assert!(
        !result.unwrap_err().contains("PROPOSAL CREATED"),
        "must not claim a proposal was created when nothing was written"
    );
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "the inner tool must still never run, regardless of whether the proposal was saved"
    );
}

#[tokio::test]
async fn sweeping_destructive_call_is_downgraded() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["vault-mcp:delete"],
        Ok("done".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault-mcp".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("vault-mcp".into(), Consequence::Reversible)], // Low consequence
        Vec::new(),
        Vec::new(),
        dir.path().to_path_buf(),
        "delete all notes".into(), // Sweeping+destructive goal
        "test-sweep".into(),
        ProposalSigner::random(),
        "default",
    );

    let call = ToolInvocation::new("c1", "vault-mcp:delete", serde_json::json!({"path": "all"}));
    let result = rt.invoke(&call).await;
    // A downgrade is a tool *result* (Ok), not an error.
    let msg = result.expect("downgrade should be an Ok tool result, not an Err");
    assert!(msg.contains("PROPOSAL CREATED") && msg.contains("did not run"));
    // The inner tool must NOT have run.
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "sweeping-destructive call must not invoke the inner tool"
    );
}

fn vault_descriptor() -> McpDescriptor {
    McpDescriptor {
        name: "vault".into(),
        description: "git-tracked vault".into(),
        consequence: Consequence::Reversible, // low, so this isolates the zone check
        provenance: None,
        default_zone: Some("tasks".into()),
        tool_zones: vec![("write_review".into(), Some("reviews".into()))],
        zone_from_arg: None,
        write_tools: Vec::new(),
        ..Default::default()
    }
}

#[tokio::test]
async fn zone_restricted_call_is_downgraded_to_proposal() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["vault:write_review"],
        Ok("wrote".into()),
    ));
    // Holds the authority to write `reviews` - this test is about whether the write is SAFE to
    // do directly (write-class), which is a question that only arises once it is PERMITTED.
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("vault".into()),
        Capability::Write(Zone::vault("reviews")),
    ]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        vec![vault_descriptor()],
        vec![("reviews".to_string(), WriteClass::ProposalOnly)],
        dir.path().to_path_buf(),
        "write a review note".into(),
        "test-zone".into(),
        ProposalSigner::random(),
        "default",
    );

    let call = ToolInvocation::new(
        "c1",
        "vault:write_review",
        serde_json::json!({"content": "..."}),
    );
    let result = rt.invoke(&call).await;
    let msg = result.expect("downgrade should be an Ok tool result, not an Err");
    assert!(msg.contains("PROPOSAL CREATED") && msg.contains("did not run"));
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "a zone-restricted call must not invoke the inner tool"
    );
}

#[tokio::test]
async fn zone_agent_writable_call_passes_through() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["vault:write_review"],
        Ok("wrote".into()),
    ));
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("vault".into()),
        Capability::Write(Zone::vault("reviews")),
    ]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        vec![vault_descriptor()],
        vec![("reviews".to_string(), WriteClass::AgentWritable)],
        dir.path().to_path_buf(),
        "write a review note".into(),
        "test-zone-ok".into(),
        ProposalSigner::random(),
        "default",
    );

    let call = ToolInvocation::new(
        "c1",
        "vault:write_review",
        serde_json::json!({"content": "..."}),
    );
    let result = rt.invoke(&call).await;
    assert_eq!(result, Ok("wrote".into()));
    assert_eq!(inner.invoked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn call_to_an_mcp_not_in_the_zone_catalog_is_unaffected() {
    // Backward-compat case: an empty zone_catalog (as every pre-existing test in this file
    // uses) must never trip the zone-write-class check, regardless of zone_write_classes.
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["vault:write_review"],
        Ok("wrote".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        Vec::new(), // no zone declarations at all for "vault"
        vec![("reviews".to_string(), WriteClass::ProposalOnly)],
        dir.path().to_path_buf(),
        "write a review note".into(),
        "test-zone-untracked".into(),
        ProposalSigner::random(),
        "default",
    );

    let call = ToolInvocation::new(
        "c1",
        "vault:write_review",
        serde_json::json!({"content": "..."}),
    );
    let result = rt.invoke(&call).await;
    assert_eq!(result, Ok("wrote".into()));
}

#[tokio::test]
#[ignore = "requires LIBERADO_TELEGRAM_BOT_TOKEN + LIBERADO_TELEGRAM_CHAT_ID + network access"]
async fn live_high_consequence_downgrade_sends_a_real_telegram_notification() {
    // Full-integration live check: a real proposal write through the actual production guard
    // path, with a real Notifier attached, not just liberado-notify's own bare TelegramNotifier
    // in isolation - proves `with_notifier`/the `invoke`-path notify call are wired correctly,
    // not just that the underlying HTTP call works.
    let notifier = liberado_notify::TelegramNotifier::from_env()
        .expect("set LIBERADO_TELEGRAM_BOT_TOKEN and LIBERADO_TELEGRAM_CHAT_ID to run this test");
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["email-mcp:send"],
        Ok("sent".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("email-mcp".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner,
        caps,
        vec![("email-mcp".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        dir.path().to_path_buf(),
        "send an email".into(),
        "live-notify-test".into(),
        ProposalSigner::random(),
        "default",
    )
    .with_notifier(Arc::new(notifier));

    let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({"to": "boss"}));
    let result = rt.invoke(&call).await;
    assert!(
        result
            .expect("downgrade should be Ok")
            .contains("PROPOSAL CREATED")
    );
}

/// F1, the live failure, pinned: a grant that may CALL an MCP but holds no Write for the zone it
/// targets must be refused. Before 2026-07-14 this call succeeded - `Capability::Write` was never
/// consulted here, so `ExecuteMcp("turbovault")` was in effect "write the whole vault".
#[tokio::test]
async fn calling_an_mcp_is_not_permission_to_write_with_it() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["vault:write_review"],
        Ok("wrote".into()),
    ));
    // ExecuteMcp but NO Write - exactly the dispatch-readonly profile from the live control.
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        vec![vault_descriptor()],
        // `reviews` is freely agent-writable: the RISK gate would happily pass this. The refusal
        // must come from AUTHORITY, which is a different question and is asked first.
        vec![("reviews".to_string(), WriteClass::AgentWritable)],
        dir.path().to_path_buf(),
        "write a review note".into(),
        "test-f1".into(),
        ProposalSigner::random(),
        "default",
    );

    let call = ToolInvocation::new("c1", "vault:write_review", serde_json::json!({"c": "..."}));
    let err = rt
        .invoke(&call)
        .await
        .expect_err("a write with no Write capability must be REFUSED, not downgraded");
    assert!(err.contains("not authorized"), "{err}");
    assert!(
        err.contains("reviews"),
        "must name the zone it refused: {err}"
    );
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "and the tool must never have run"
    );
}

/// The path-addressed case, which is what TurboVault actually is: the zone comes from the call's
/// arguments, so `Write(tasks)` must NOT authorize a write to `decisions/`.
#[tokio::test]
async fn a_path_addressed_write_is_checked_against_the_zone_the_path_names() {
    let dir = tempfile::TempDir::new().unwrap();
    let descriptor = McpDescriptor {
        name: "turbovault".into(),
        description: "path-addressed vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec!["write_note".into()],
    ..Default::default()
    };
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault("tasks")),
    ]);
    let classes = vec![
        ("tasks".to_string(), WriteClass::AgentWritable),
        ("decisions".to_string(), WriteClass::AgentWritable),
    ];
    let make = |inner: Arc<InvocationRecordingRuntime>| {
        RiskGatedToolRuntime::new(
            inner,
            caps.clone(),
            vec![("turbovault".into(), Consequence::Reversible)],
            vec![descriptor.clone()],
            classes.clone(),
            dir.path().to_path_buf(),
            "write a note".into(),
            "test-path".into(),
            ProposalSigner::random(),
            "default",
        )
    };

    // In-zone: permitted.
    let ok_inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:write_note"],
        Ok("wrote".into()),
    ));
    let ok = make(ok_inner.clone())
        .invoke(&ToolInvocation::new(
            "c1",
            "turbovault:write_note",
            serde_json::json!({"path": "tasks/a.md"}),
        ))
        .await;
    assert_eq!(ok, Ok("wrote".into()));

    // Out of zone: the SAME tool, the SAME grant - refused, because the path names a zone this
    // grant cannot write. A fixed `default_zone` could never have caught this.
    let bad_inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:write_note"],
        Ok("wrote".into()),
    ));
    let err = make(bad_inner.clone())
        .invoke(&ToolInvocation::new(
            "c2",
            "turbovault:write_note",
            serde_json::json!({"path": "decisions/b.md"}),
        ))
        .await
        .expect_err("Write(tasks) must not authorize a write to decisions/");
    assert!(err.contains("decisions"), "{err}");
    assert!(bad_inner.invoked.lock().unwrap().is_empty());
}

/// The "Approve session" consistency fix: a write to an **undeclared** zone (not in
/// `zone_write_classes`, so it would normally fail safe to `ProposalOnly` and downgrade) must
/// write *directly* when the grant already holds `Write(zone)` for it - because that authority
/// can only have come from a human session/everywhere approval (policy validation forbids
/// granting an undeclared zone). Without this, tapping "Approve session" would pass the authority
/// check only to have the very next write re-gated behind a fresh proposal.
#[tokio::test]
async fn a_granted_write_to_an_undeclared_zone_is_direct_not_downgraded() {
    let dir = tempfile::TempDir::new().unwrap();
    let descriptor = McpDescriptor {
        name: "turbovault".into(),
        description: "path-addressed vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec!["write_note".into()],
    ..Default::default()
    };
    // Holds Write(sandbox) - as an in-memory "Approve session" grant would - but `sandbox` is
    // NOT declared in zone_write_classes (empty), exactly the live homelab shape.
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault("sandbox")),
    ]);
    let granted_inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:write_note"],
        Ok("wrote".into()),
    ));
    let rt = RiskGatedToolRuntime::new(
        granted_inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![descriptor.clone()],
        Vec::new(), // sandbox undeclared
        dir.path().to_path_buf(),
        "write a note".into(),
        "test-undeclared-granted".into(),
        ProposalSigner::random(),
        "default",
    );
    let result = rt
        .invoke(&ToolInvocation::new(
            "c1",
            "turbovault:write_note",
            serde_json::json!({"path": "sandbox/x.md"}),
        ))
        .await;
    assert_eq!(
        result,
        Ok("wrote".into()),
        "a granted undeclared-zone write runs directly"
    );
    assert_eq!(
        granted_inner.invoked.lock().unwrap().len(),
        1,
        "the inner tool must actually run (no proposal downgrade)"
    );

    // Control: the SAME undeclared zone WITHOUT a Write grant still fails - at the authority
    // check (:227), before the write-class question is even asked.
    let ungranted_inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:write_note"],
        Ok("wrote".into()),
    ));
    let rt2 = RiskGatedToolRuntime::new(
        ungranted_inner.clone(),
        CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]),
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![descriptor],
        Vec::new(),
        dir.path().to_path_buf(),
        "write a note".into(),
        "test-undeclared-ungranted".into(),
        ProposalSigner::random(),
        "default",
    );
    let refused = rt2
        .invoke(&ToolInvocation::new(
            "c2",
            "turbovault:write_note",
            serde_json::json!({"path": "sandbox/x.md"}),
        ))
        .await
        .expect_err("no Write(sandbox) grant must be refused, not written");
    assert!(refused.contains("not authorized"), "{refused}");
    assert!(ungranted_inner.invoked.lock().unwrap().is_empty());
}

#[cfg(test)]
mod magnitude_reads_structure_first {
    use super::*;

    /// The descriptor shape that makes reach knowable: path-addressed, so the call names its own
    /// single target.
    fn turbovault() -> McpDescriptor {
        McpDescriptor {
            name: "turbovault".into(),
            description: "path-addressed vault".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: None,
            tool_zones: Vec::new(),
            zone_from_arg: Some("path".into()),
            write_tools: vec!["write_note".into()],
        ..Default::default()
        }
    }

    fn gate(
        dir: &std::path::Path,
        inner: Arc<InvocationRecordingRuntime>,
        descriptor: McpDescriptor,
        goal: &str,
    ) -> RiskGatedToolRuntime {
        let mcp = descriptor.name.clone();
        RiskGatedToolRuntime::new(
            inner,
            CapabilitySet::from_iter([
                Capability::ExecuteMcp(mcp.clone()),
                Capability::Write(Zone::vault("Learning")),
            ]),
            vec![(mcp, Consequence::Reversible)],
            vec![descriptor],
            vec![("Learning".to_string(), WriteClass::AgentWritable)],
            dir.to_path_buf(),
            goal.into(),
            "test-magnitude".into(),
            ProposalSigner::random(),
            "default",
        )
    }

    /// The live false positive, reduced: a research report whose *content* discusses destruction is
    /// data being saved, not an action being ordered.
    ///
    /// `prop-1785557626819756862` (2026-08-01) gated a requested write-up on `destroyed` ×2,
    /// `destroys`, `remove`, `every` ×12 and `all` ×6 - every one of them prose *about*
    /// organizations, in a report the operator had asked for.
    #[tokio::test]
    async fn a_report_that_discusses_destruction_is_not_a_destructive_action() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:write_note"],
            Ok("wrote".into()),
        ));
        let rt = gate(
            dir.path(),
            inner.clone(),
            turbovault(),
            "research nash equilibrium in organizations and write me a report",
        );

        let call = ToolInvocation::new(
            "c1",
            "turbovault:write_note",
            serde_json::json!({
                "path": "Learning/Nash Equilibrium - Research.md",
                "content": "Olson showed that distributional coalitions are destroyed by war, 
                            and that every entrenched group blocks all reform. Moloch destroys 
                            every attempt to remove the entire coordination failure.",
            }),
        );

        let msg = rt.invoke(&call).await.expect("the write should be allowed");
        assert!(
            !msg.contains("PROPOSAL CREATED"),
            "a write naming one path must not be gated on words in its payload: {msg}"
        );
        assert_eq!(
            inner.invoked.lock().unwrap().len(),
            1,
            "the report should have been written"
        );
    }

    /// The guard's real target, unchanged: a sweeping-destructive *instruction* still gates, however
    /// narrow the call that carries it.
    #[tokio::test]
    async fn a_sweeping_destructive_goal_still_gates_a_single_path_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:write_note"],
            Ok("wrote".into()),
        ));
        let rt = gate(
            dir.path(),
            inner.clone(),
            turbovault(),
            "delete all my notes",
        );

        let call = ToolInvocation::new(
            "c1",
            "turbovault:write_note",
            serde_json::json!({ "path": "Learning/x.md", "content": "harmless" }),
        );

        let msg = rt.invoke(&call).await.expect("downgrade is an Ok result");
        assert!(
            msg.contains("PROPOSAL CREATED"),
            "the instruction is always read, whatever the call looks like: {msg}"
        );
        assert!(inner.invoked.lock().unwrap().is_empty());
    }

    /// Reach is only known for the **path-addressed** style. A fixed-zone tool's zone comes from its
    /// *name*, so a resolved zone says which zone is touched and nothing about how much of it -
    /// prose remains the only signal, and the payload is still scanned.
    #[tokio::test]
    async fn a_fixed_zone_tool_still_has_its_payload_scanned() {
        let dir = tempfile::TempDir::new().unwrap();
        let descriptor = McpDescriptor {
            name: "turbovault".into(),
            description: "fixed-zone vault".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: Some("Learning".into()),
            tool_zones: Vec::new(),
            zone_from_arg: None,
            write_tools: Vec::new(),
        ..Default::default()
        };
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:write_note"],
            Ok("wrote".into()),
        ));
        let rt = gate(dir.path(), inner.clone(), descriptor, "tidy up");

        let call = ToolInvocation::new(
            "c1",
            "turbovault:write_note",
            serde_json::json!({ "instruction": "delete every note" }),
        );

        let msg = rt.invoke(&call).await.expect("downgrade is an Ok result");
        assert!(
            msg.contains("PROPOSAL CREATED"),
            "reach is unknown for a fixed-zone tool, so the payload must still be read: {msg}"
        );
        assert!(inner.invoked.lock().unwrap().is_empty());
    }

    /// A write whose path argument is missing is `Undeterminable`, not bounded - it must not buy the
    /// payload exemption. (It is refused earlier by the zone guard; this pins that the magnitude
    /// side does not treat it as safe either.)
    #[tokio::test]
    async fn a_write_with_no_resolvable_path_is_not_treated_as_bounded() {
        let d = turbovault();
        assert!(
            !liberado_common::names_single_write_target(
                &d,
                "write_note",
                &serde_json::json!({ "content": "delete every note" })
            ),
            "a missing path argument names no single target"
        );
        assert!(
            liberado_common::names_single_write_target(
                &d,
                "write_note",
                &serde_json::json!({ "path": "Learning/x.md" })
            ),
            "a resolved zone path does"
        );
    }
}

/// Risk-waiver regressions for the runtime magnitude guard.
///
/// These mirror the dispatcher's pre-flight waivers but operate on a single live call, not the
/// goal text in aggregate. The waivers must match the call's resolved (mcp, tool, zone), and
/// they must NOT affect capability / consequence / zone-write-class guards - only magnitude.
#[cfg(test)]
mod magnitude_respects_risk_waivers {
    use super::*;
    use liberado_common::{Guard, RiskWaiver};

    fn turbovault_descriptor() -> McpDescriptor {
        McpDescriptor {
            name: "turbovault".into(),
            description: "path-addressed vault".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: None,
            tool_zones: Vec::new(),
            zone_from_arg: Some("path".into()),
            write_tools: vec!["write_note".into(), "delete_note".into()],
            read_tools: vec!["read_note".into()],
            vault_wide_writes: Vec::new(),
        }
    }

    fn gate_with_waivers(
        dir: &std::path::Path,
        inner: Arc<InvocationRecordingRuntime>,
        descriptor: McpDescriptor,
        goal: &str,
        consequence_catalog: Vec<(String, Consequence)>,
        waivers: RiskWaiverSet,
    ) -> RiskGatedToolRuntime {
        let mcp = descriptor.name.clone();
        RiskGatedToolRuntime::new(
            inner,
            CapabilitySet::from_iter([
                Capability::ExecuteMcp(mcp.clone()),
                Capability::Write(Zone::vault("Tasks")),
            ]),
            consequence_catalog,
            vec![descriptor],
            vec![("Tasks".to_string(), WriteClass::AgentWritable)],
            dir.to_path_buf(),
            goal.into(),
            "test-waiver".into(),
            ProposalSigner::random(),
            "default",
        )
        .with_risk_waivers(waivers)
    }

    fn waiver(mcp: &str, tools: Option<Vec<&str>>, zones: Option<Vec<&str>>) -> RiskWaiver {
        RiskWaiver {
            mcp: mcp.into(),
            match_tools: tools.map(|t| t.into_iter().map(String::from).collect()),
            match_zones: zones.map(|z| z.into_iter().map(String::from).collect()),
            guard: Guard::Magnitude,
        }
    }

    /// The live false positive - the goal text contains both sweeping (`everything`) and
    /// destructive (`Remove`) words, but every actual call is a path-addressed, scoped
    /// operation. Without a waiver, the magnitude guard downgrades to a proposal.
    #[tokio::test]
    async fn a_waiver_matching_every_call_suppresses_magnitude() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:read_note", "turbovault:write_note"],
            Ok("wrote".into()),
        ));
        let goal = "Read Tasks/Main.md. Then write it back, removing two specific lines. 
                    Keep everything else in the file exactly as-is.";

        // No waiver: the gate fires.
        let rt = gate_with_waivers(
            dir.path(),
            inner.clone(),
            turbovault_descriptor(),
            goal,
            vec![],
            RiskWaiverSet::empty(),
        );
        let read_call = ToolInvocation::new(
            "c1",
            "turbovault:read_note",
            serde_json::json!({"path": "Tasks/Main.md"}),
        );
        let msg = rt
            .invoke(&read_call)
            .await
            .expect("read should be tool result");
        assert!(
            msg.contains("PROPOSAL CREATED"),
            "without a waiver, the read call must trip magnitude: {msg}"
        );

        // With a waiver covering the read tool wholesale: the gate is suppressed for the read call.
        let rt = gate_with_waivers(
            dir.path(),
            inner.clone(),
            turbovault_descriptor(),
            goal,
            vec![],
            RiskWaiverSet {
                waivers: [waiver(
                    "turbovault",
                    Some(vec!["read_note"]),
                    None, // no zone restriction - reads don't resolve a zone here
                )]
                .into_iter()
                .collect(),
            },
        );
        let msg = rt.invoke(&read_call).await.expect("read with waiver");
        assert!(
            !msg.contains("PROPOSAL CREATED"),
            "a covering waiver must suppress magnitude: {msg}"
        );
        assert_eq!(msg, "wrote", "the inner runtime must execute the read");
    }

    /// A waiver that matches the read but not the write tool means the magnitude heuristic
    /// still fires when the write is attempted. Waivers don't widen.
    #[tokio::test]
    async fn a_partial_waiver_does_not_suppress_magnitude_for_uncovered_calls() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:write_note"],
            Ok("wrote".into()),
        ));
        let goal = "Delete all my notes and remove everything else.";
        let rt = gate_with_waivers(
            dir.path(),
            inner.clone(),
            turbovault_descriptor(),
            goal,
            vec![],
            RiskWaiverSet {
                waivers: [waiver(
                    "turbovault",
                    Some(vec!["read_note"]), // only the read, not the write
                    None,
                )]
                .into_iter()
                .collect(),
            },
        );
        let write_call = ToolInvocation::new(
            "c1",
            "turbovault:write_note",
            serde_json::json!({"path": "Tasks/Other.md", "content": "stuff"}),
        );
        let msg = rt
            .invoke(&write_call)
            .await
            .expect("write should be tool result");
        assert!(
            msg.contains("PROPOSAL CREATED"),
            "a partial waiver leaves the write call's magnitude gate firing: {msg}"
        );
    }

    /// A waiver for the same MCP but a different tool does not match.
    #[tokio::test]
    async fn a_waiver_for_a_different_tool_does_not_match() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:write_note"],
            Ok("wrote".into()),
        ));
        let goal = "Delete every note.";
        let rt = gate_with_waivers(
            dir.path(),
            inner.clone(),
            turbovault_descriptor(),
            goal,
            vec![],
            RiskWaiverSet {
                waivers: [waiver(
                    "turbovault",
                    Some(vec!["read_note"]), // covers read, NOT write
                    None,
                )]
                .into_iter()
                .collect(),
            },
        );
        let write_call = ToolInvocation::new(
            "c1",
            "turbovault:write_note",
            serde_json::json!({"path": "Tasks/Main.md", "content": "x"}),
        );
        let msg = rt.invoke(&write_call).await.expect("write");
        assert!(
            msg.contains("PROPOSAL CREATED"),
            "a waiver matching only `read_note` does not cover `write_note`: {msg}"
        );
    }

    /// Waivers do not affect other guards. A call to an `External`-rated MCP must still be
    /// gated by the consequence gate even when a magnitude waiver covers it.
    #[tokio::test]
    async fn a_magnitude_waiver_does_not_bypass_consequence() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["email:send"],
            Ok("sent".into()),
        ));
        let email_descriptor = McpDescriptor {
            name: "email".into(),
            description: "send email".into(),
            consequence: Consequence::External,
            provenance: None,
            default_zone: None,
            tool_zones: Vec::new(),
            zone_from_arg: None,
            write_tools: Vec::new(),
        ..Default::default()
        };
        let rt = gate_with_waivers(
            dir.path(),
            inner.clone(),
            email_descriptor,
            "send to everyone",
            vec![(String::from("email"), Consequence::External)],
            RiskWaiverSet {
                waivers: [waiver("email", None, None)].into_iter().collect(),
            },
        );
        let call = ToolInvocation::new("c1", "email:send", serde_json::json!({"to": "everyone"}));
        // The call returns Ok(...) when downgraded (a tool result, not an error). The
        // consequence gate is what should produce that downgrade.
        let result = rt.invoke(&call).await;
        assert!(
            result.is_ok(),
            "a consequence downgrade is a tool result, not an error: {result:?}"
        );
        let msg = result.unwrap();
        assert!(
            msg.contains("PROPOSAL CREATED"),
            "the consequence gate (not the magnitude waiver) must downgrade this: {msg}"
        );
    }
}

#[cfg(test)]
mod one_intent_one_prompt {
    use super::*;

    fn gate(dir: &std::path::Path, inner: Arc<InvocationRecordingRuntime>) -> RiskGatedToolRuntime {
        RiskGatedToolRuntime::new(
            inner,
            CapabilitySet::from_iter([Capability::ExecuteMcp("vault-mcp".into())]),
            vec![("vault-mcp".into(), Consequence::Reversible)],
            Vec::new(),
            Vec::new(),
            dir.to_path_buf(),
            "delete all notes".into(),
            "test-dedup".into(),
            ProposalSigner::random(),
            "default",
        )
    }

    /// One intent must cost the human one notification.
    ///
    /// A gated call returns as a tool *result*, so the model reads "did not run" and retries. Each
    /// retry used to mint a fresh `prop-{nanos}`: live on 2026-08-01 a single subagent run produced
    /// three proposals for the same write to the same path, 43s apart, and the operator was asked to
    /// approve the same thing three times.
    #[tokio::test]
    async fn a_retried_call_reuses_its_pending_proposal() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["vault-mcp:delete"],
            Ok("done".into()),
        ));
        let rt = gate(dir.path(), inner.clone());
        let call = ToolInvocation::new(
            "c1",
            "vault-mcp:delete",
            serde_json::json!({ "path": "all" }),
        );

        let first = rt.invoke(&call).await.expect("downgrade is an Ok result");
        // The same action again, as a fresh attempt with a different call id - which is exactly what
        // a retrying model sends.
        let retry = ToolInvocation::new(
            "c2",
            "vault-mcp:delete",
            serde_json::json!({ "path": "all" }),
        );
        let second = rt.invoke(&retry).await.expect("downgrade is an Ok result");

        let count = std::fs::read_dir(dir.path().join(liberado_common::PROPOSALS_DIR))
            .unwrap()
            .count();
        assert_eq!(count, 1, "a retry must not create a second proposal");
        assert_eq!(
            first, second,
            "and the model must be pointed at the same one"
        );
        assert!(inner.invoked.lock().unwrap().is_empty());
    }

    /// Dedup keys on the action, so a *different* action still gets its own proposal - otherwise the
    /// first gated call would silently swallow every later one.
    #[tokio::test]
    async fn a_different_action_still_gets_its_own_proposal() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["vault-mcp:delete"],
            Ok("done".into()),
        ));
        let rt = gate(dir.path(), inner.clone());

        rt.invoke(&ToolInvocation::new(
            "c1",
            "vault-mcp:delete",
            serde_json::json!({ "path": "all" }),
        ))
        .await
        .unwrap();
        rt.invoke(&ToolInvocation::new(
            "c2",
            "vault-mcp:delete",
            serde_json::json!({ "path": "everything else" }),
        ))
        .await
        .unwrap();

        let count = std::fs::read_dir(dir.path().join(liberado_common::PROPOSALS_DIR))
            .unwrap()
            .count();
        assert_eq!(count, 2, "distinct actions need distinct approvals");
    }

    /// The message is read by the model, so it must not invite the two things the model otherwise
    /// does: retry, and try to approve the proposal itself.
    #[test]
    fn the_gated_message_tells_the_model_not_to_retry_or_self_approve() {
        let msg = proposal_message(std::path::Path::new("proposals/prop-1.md"));
        assert!(msg.contains("Do NOT retry"), "{msg}");
        assert!(msg.contains("do NOT edit the proposal"), "{msg}");
        assert!(
            msg.contains("cannot approve it yourself"),
            "the old wording said approval was *yours* to give, and a model acted on that: {msg}"
        );
        assert!(
            !msg.contains("your approval"),
            "second-person-to-a-human phrasing is what caused the self-approval attempt: {msg}"
        );
    }
}

// ── path-addressed MCP write-zone regression suite (2026-10-11 live bug) ────────
//
// The live failure: turbovault:edit_note on a path the agent's grant didn't include, with no
// approval card raised. The hole was that write_target returned NotAWrite for tools not in
// write_tools, so the runtime gate treated an undeclared tool as a read. The runtime chokepoint
// is one (RiskGatedToolRuntime::invoke), and the regression set here covers the cases the bug
// report names explicitly: edit_note / tasks_update / manage_tags, multi-path tools, undeclared
// tools on a path-addressed MCP, and the Once / Session / Everywhere shape of the deny.
//
// The shape of these tests mirrors `a_path_addressed_write_is_checked_against_the_zone_the_path_names`
// above: one descriptor under test, one runtime, one invocation. The catalog/declaration lives
// in `turbovault_descriptor()` which classifies every tool the live turbovault exposes.
#[cfg(test)]
mod vault_write_gate_regression {
    use super::*;

    /// Live TurboVault tool surface (2026-10-11): every tool classified into exactly one of
    /// `write_tools` / `read_tools` / `vault_wide_writes`. Approximates the deployed topology:
    /// `write_tools` covers path-addressed mutations, `read_tools` covers the lookup/graph
    /// surface, `vault_wide_writes` covers fanout / vault switch / index rebuild.
    fn livetv_descriptor() -> McpDescriptor {
        McpDescriptor {
            name: "turbovault".into(),
            description: "live turbovault - every tool classified".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: None,
            tool_zones: Vec::new(),
            zone_from_arg: Some("path".into()),
            write_tools: vec![
                "write_note".into(),
                "edit_note".into(),
                "delete_note".into(),
                "move_note".into(),
                "move_file".into(),
                "update_frontmatter".into(),
                "manage_tags".into(),
                "create_from_template".into(),
                "tasks_complete".into(),
                "tasks_update".into(),
                "tasks_delete".into(),
                "generate_index".into(),
                "append_log_entry".into(),
                "visualize".into(),
            ],
            read_tools: vec![
                "get_vault_context".into(),
                "read_note".into(),
                "get_backlinks".into(),
                "get_forward_links".into(),
                "get_related_notes".into(),
                "get_hub_notes".into(),
                "get_dead_end_notes".into(),
                "get_isolated_clusters".into(),
                "quick_health_check".into(),
                "full_health_analysis".into(),
                "get_broken_links".into(),
                "detect_cycles".into(),
                "search".into(),
                "advanced_search".into(),
                "recommend_related".into(),
                "query_frontmatter_sql".into(),
                "list_templates".into(),
                "get_template".into(),
                "list_vaults".into(),
                "get_vault_config".into(),
                "get_active_vault".into(),
                "list_orphan_fanouts".into(),
                "export_health_report".into(),
                "export_broken_links".into(),
                "export_vault_stats".into(),
                "export_analysis_report".into(),
                "query_metadata".into(),
                "get_metadata_value".into(),
                "get_notes_info".into(),
                "suggest_links".into(),
                "get_centrality_ranking".into(),
                "diff_notes".into(),
                "evaluate_note_quality".into(),
                "vault_quality_report".into(),
                "analyze_note_grounding".into(),
                "find_ungrounded_notes".into(),
                "okf_validate".into(),
                "find_stale_notes".into(),
                "semantic_search".into(),
                "find_duplicates".into(),
                "audit_stats".into(),
                "tasks_list".into(),
                "tasks_overdue".into(),
                "tasks_tags".into(),
                "tasks_config".into(),
            ],
            vault_wide_writes: vec![
                "begin_fanout".into(),
                "commit_fanout".into(),
                "abandon_fanout".into(),
                "set_active_vault".into(),
                "vector_reindex".into(),
                "vector_search".into(),
                "vector_status".into(),
                "vector_config".into(),
            ],
        }
    }

    fn gate_with_grant(
        dir: &std::path::Path,
        inner: Arc<InvocationRecordingRuntime>,
        zones: &[&str],
    ) -> RiskGatedToolRuntime {
        let mcp = "turbovault".to_string();
        let mut caps = vec![Capability::ExecuteMcp(mcp.clone())];
        for z in zones {
            caps.push(Capability::Write(Zone::vault(*z)));
        }
        let classes = zones
            .iter()
            .map(|z| (z.to_string(), WriteClass::AgentWritable))
            .collect();
        RiskGatedToolRuntime::new(
            inner,
            CapabilitySet::from_iter(caps),
            vec![("turbovault".into(), Consequence::Reversible)],
            vec![livetv_descriptor()],
            classes,
            dir.to_path_buf(),
            "test regression".into(),
            "test-f1-regression".into(),
            ProposalSigner::random(),
            "default",
        )
    }

    // ── The live bug pinned at the type level ───────────────────────────────

    /// `01M3ZB89WK54HGNG6ATJ9FPJMX` from 2026-10-11, exactly: the session had no
    /// `Write(Vault("conformance"))` in its grant and called `edit_note` on
    /// `conformance/approval-test.md`. The old code returned `NotAWrite` and ran the
    /// call. The fixed code returns `Undeterminable` (since `edit_note` is now declared in
    /// `write_tools`, the resolution actually proceeds), and the capability gate refuses
    /// the call because the grant does not include `Write(Vault("conformance"))`.
    #[tokio::test]
    async fn edit_note_without_a_zone_grant_is_now_a_permission_request() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:edit_note", "turbovault:read_note"],
            Ok("wrote".into()),
        ));
        // ExecuteMcp only - exactly the Personal Task Manager profile from the live trace.
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("turbovault".into(), Consequence::Reversible)],
            vec![livetv_descriptor()],
            vec![("conformance".to_string(), WriteClass::AgentWritable)],
            dir.path().to_path_buf(),
            "edit conformance note".into(),
            "test-edit-note-live-bug".into(),
            ProposalSigner::random(),
            "default",
        )
        .with_approval(crate::ApprovalStamp {
            session_id: "01M3ZB89WK54HGNG6ATJ9FPJMX".into(),
            origin: liberado_common::ApprovalOrigin::Web,
        });

        let call = ToolInvocation::new(
            "live-2026-10-11",
            "turbovault:edit_note",
            serde_json::json!({"path": "conformance/approval-test.md"}),
        );
        let msg = rt
            .invoke(&call)
            .await
            .expect("a chat surface raises a permission request, not an error");
        assert!(msg.contains("PERMISSION REQUESTED"), "{msg}");
        assert!(
            msg.contains("conformance"),
            "the request must name the zone the agent can't write: {msg}"
        );
        assert!(
            inner.invoked.lock().unwrap().is_empty(),
            "the inner tool must NEVER have run - this is the live-bug assertion: {:?}",
            inner.invoked.lock().unwrap()
        );

        // Proposal file written, with the requested grant.
        let note = std::fs::read_dir(dir.path().join("proposals"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let proposal =
            liberado_common::Proposal::from_note(&std::fs::read_to_string(note.path()).unwrap())
                .unwrap();
        assert_eq!(proposal.session_id.as_deref(), Some("01M3ZB89WK54HGNG6ATJ9FPJMX"));
    }

    /// The same call with the `Write(Vault("conformance"))` grant in place - the positive
    /// control. Without it, every assertion above would still pass if the gate simply
    /// refused *everything* (which would be a catastrophic bug that looks exactly like
    /// perfect security).
    #[tokio::test]
    async fn edit_note_with_a_zone_grant_runs() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:edit_note"],
            Ok("edited".into()),
        ));
        let rt = gate_with_grant(dir.path(), inner.clone(), &["conformance"]);
        let call = ToolInvocation::new(
            "c-positive",
            "turbovault:edit_note",
            serde_json::json!({"path": "conformance/approval-test.md"}),
        );
        let result = rt.invoke(&call).await;
        assert_eq!(result, Ok("edited".into()), "granted zone must run");
        assert_eq!(inner.invoked.lock().unwrap().len(), 1);
    }

    /// Reading tools remain unaffected: an unzoned read on a path-addressed MCP must
    /// never ask. If we accidentally promote a read to a write we triple-tap the human
    /// for every `read_note`.
    #[tokio::test]
    async fn read_tools_are_unaffected_by_the_fix() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:read_note", "turbovault:search"],
            Ok("read".into()),
        ));
        let rt = gate_with_grant(dir.path(), inner.clone(), &["Tasks"]);
        for tool in ["turbovault:read_note", "turbovault:search"] {
            let call = ToolInvocation::new(
                "c1",
                tool,
                serde_json::json!({"path": "Tasks/Main.md"}),
            );
            let result = rt.invoke(&call).await.expect("a read must not ask");
            assert_eq!(&result, "read", "tool={tool}");
        }
        assert_eq!(inner.invoked.lock().unwrap().len(), 2);
    }

    /// `manage_tags` and `tasks_update` / `tasks_complete` / `tasks_delete` were missing from
    /// the live `write_tools` list (the old list had `update_task` / `delete_task`, which were
    /// the previous name and no longer exist). They are now declared writes; each must raise
    /// a permission request when the zone isn't in the grant.
    #[tokio::test]
    async fn the_tools_missing_from_the_old_write_tools_list_are_now_gated() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &[
                "turbovault:manage_tags",
                "turbovault:tasks_update",
                "turbovault:tasks_complete",
                "turbovault:tasks_delete",
            ],
            Ok("done".into()),
        ));
        // No zone grant.
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("turbovault".into(), Consequence::Reversible)],
            vec![livetv_descriptor()],
            vec![("conformance".to_string(), WriteClass::AgentWritable)],
            dir.path().to_path_buf(),
            "test regression".into(),
            "test-missing-from-old-list".into(),
            ProposalSigner::random(),
            "default",
        )
        .with_approval(crate::ApprovalStamp {
            session_id: "sess".into(),
            origin: liberado_common::ApprovalOrigin::Web,
        });
        for (tool, path) in [
            ("turbovault:manage_tags", "conformance/tags.md"),
            ("turbovault:tasks_update", "conformance/tasks.md"),
            ("turbovault:tasks_complete", "conformance/tasks.md"),
            ("turbovault:tasks_delete", "conformance/tasks.md"),
        ] {
            let call = ToolInvocation::new("c", tool, serde_json::json!({"path": path}));
            let msg = rt.invoke(&call).await.expect("a write without a grant must ask");
            assert!(
                msg.contains("PERMISSION REQUESTED"),
                "{tool} must now raise a permission request: {msg}"
            );
        }
        assert!(
            inner.invoked.lock().unwrap().is_empty(),
            "none of the gated tools must have actually run"
        );
    }

    /// Multi-zone source-and-destination: `move_file(from, to)` resolves two zones, both must
    /// be in the grant or the call must raise a permission request that names the missing
    /// zones. The old code treated `move_note` / `move_file` as `Undeterminable` (because the
    /// arg name was `path` and they don't take `path`) and *always* refused - a hard-coded
    /// dead end. The new code resolves both `from` and `to` into zones, checks each against
    /// the grant, and refuses if either is missing.
    ///
    /// For this test we use a slim descriptor that explicitly names the multi-path arg so the
    /// resolution can find both zones. The full product wiring depends on a runtime schema
    /// decision (which args carry paths) that lives in the deployed topology - the test
    /// pins the *runtime* behavior, not the deployed topology, which is loaded later by the
    /// config layer.
    /// Vault-wide mutators (`begin_fanout`, `commit_fanout`, `abandon_fanout`,
    /// `set_active_vault`, `vector_reindex`) require the grant to hold at least one
    /// `Write(Zone)` capability. A read-only session (the `research` profile) must not be
    /// able to start a fanout.
    #[tokio::test]
    async fn a_vault_wide_write_refuses_a_session_with_no_zone_write_authority() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:begin_fanout"],
            Ok("started".into()),
        ));
        // ExecuteMcp + a Read, but no Write - the read-only session shape.
        let caps = CapabilitySet::from_iter([
            Capability::ExecuteMcp("turbovault".into()),
            Capability::Read(Zone::vault("Tasks")),
        ]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("turbovault".into(), Consequence::Reversible)],
            vec![livetv_descriptor()],
            Vec::new(),
            dir.path().to_path_buf(),
            "start a fanout".into(),
            "test-vault-wide-refuse".into(),
            ProposalSigner::random(),
            "default",
        )
        .with_approval(crate::ApprovalStamp {
            session_id: "sess".into(),
            origin: liberado_common::ApprovalOrigin::Web,
        });
        let call = ToolInvocation::new("c1", "turbovault:begin_fanout", serde_json::json!({}));
        let msg = rt.invoke(&call).await.expect("a chat surface must ask, not error");
        assert!(
            msg.contains("PERMISSION REQUESTED"),
            "the vault-wide gate must raise a permission request: {msg}"
        );
        assert!(
            inner.invoked.lock().unwrap().is_empty(),
            "and the tool must not have run"
        );
    }

    /// Vault-wide mutators with at least ONE zone write authority in the grant - a
    /// session that's not strictly read-only - may execute, since the guard asks "this
    /// session is not strictly read-only" and the runtime capability check on the actual
    /// call still applies for any concrete zone the call names.
    #[tokio::test]
    async fn a_vault_wide_write_runs_when_the_grant_holds_any_zone_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:begin_fanout"],
            Ok("started".into()),
        ));
        let rt = gate_with_grant(dir.path(), inner.clone(), &["Tasks"]);
        let call = ToolInvocation::new("c1", "turbovault:begin_fanout", serde_json::json!({}));
        let result = rt.invoke(&call).await;
        assert_eq!(
            result,
            Ok("started".into()),
            "a grant with at least one zone write must let vault-wide through"
        );
        assert_eq!(inner.invoked.lock().unwrap().len(), 1);
    }

    /// The staleness hole pinned: the deployed topology.toml named `update_task` and
    /// `delete_task` (the OLD turbovault API), and the new task tools (`tasks_update`,
    /// `tasks_delete`) silently sailed through every gate as NotAWrite. After the fix,
    /// undeclared tools are refused with a remediation hint rather than waved through.
    #[tokio::test]
    async fn an_undeclared_tool_on_a_path_addressed_mcp_is_refused_with_a_hint() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:brand_new_tool_added_in_a_future_release"],
            Ok("ran".into()),
        ));
        let caps = CapabilitySet::from_iter([
            Capability::ExecuteMcp("turbovault".into()),
            Capability::Write(Zone::vault("Tasks")),
        ]);
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("turbovault".into(), Consequence::Reversible)],
            vec![livetv_descriptor()],
            vec![("Tasks".to_string(), WriteClass::AgentWritable)],
            dir.path().to_path_buf(),
            "do the new thing".into(),
            "test-undeclared".into(),
            ProposalSigner::random(),
            "default",
        );
        let call = ToolInvocation::new(
            "c1",
            "turbovault:brand_new_tool_added_in_a_future_release",
            serde_json::json!({"path": "Tasks/x.md"}),
        );
        let err = rt
            .invoke(&call)
            .await
            .expect_err("an undeclared tool must be REFUSED, not waved through");
        // The error names both the tool and the three classification lists, so an operator
        // reading it can fix the topology without running the daemon.
        assert!(err.contains("brand_new_tool_added_in_a_future_release"), "{err}");
        assert!(err.contains("write_tools"), "{err}");
        assert!(err.contains("read_tools"), "{err}");
        assert!(err.contains("vault_wide_writes"), "{err}");
        assert!(
            inner.invoked.lock().unwrap().is_empty(),
            "the inner tool must never have run"
        );
    }

    /// The Once / Session / Everywhere resolution flows through `set_active_vault` exactly
    /// like a per-zone write - same proposal shape, same buttons. A background session
    /// (no approval stamp, no notifier) gets the hard refusal with no proposal at all.
    #[tokio::test]
    async fn a_vault_wide_write_in_a_background_run_is_refused_without_a_proposal() {
        let dir = tempfile::TempDir::new().unwrap();
        let inner = Arc::new(InvocationRecordingRuntime::new(
            &["turbovault:begin_fanout"],
            Ok("started".into()),
        ));
        let caps = CapabilitySet::from_iter([
            Capability::ExecuteMcp("turbovault".into()),
            Capability::Read(Zone::vault("Tasks")),
        ]);
        // No approval stamp, no notifier - a background cron.
        let rt = RiskGatedToolRuntime::new(
            inner.clone(),
            caps,
            vec![("turbovault".into(), Consequence::Reversible)],
            vec![livetv_descriptor()],
            Vec::new(),
            dir.path().to_path_buf(),
            "start a fanout unattended".into(),
            "test-vault-wide-background".into(),
            ProposalSigner::random(),
            "default",
        );
        let call = ToolInvocation::new("c1", "turbovault:begin_fanout", serde_json::json!({}));
        let err = rt.invoke(&call).await.expect_err(
            "a background run must error, not raise a permission request nobody can answer",
        );
        assert!(err.contains("not authorized"), "{err}");
        assert!(err.contains("Write"), "{err}");
        // And no proposal file is written.
        let prop_dir = dir.path().join("proposals");
        assert!(
            !prop_dir.exists()
                || std::fs::read_dir(&prop_dir).unwrap().next().is_none(),
            "no permission request must be created in a background run"
        );
    }
}

