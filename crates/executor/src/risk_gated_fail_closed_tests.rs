//! Fail-closed gate regressions split from `risk_gated_tests.rs` for module-health boundaries.

use super::*;
use liberado_test_support::InvocationRecordingRuntime;
use std::path::Path;
use std::sync::Arc;

fn gate_with_descriptor_but_no_consequence(
    inner: Arc<InvocationRecordingRuntime>,
    capabilities: CapabilitySet,
    descriptor: McpDescriptor,
) -> RiskGatedToolRuntime {
    RiskGatedToolRuntime::new(
        inner,
        capabilities,
        vec![("some-other-mcp".into(), Consequence::ReadOnly)],
        vec![descriptor],
        Vec::new(),
        Path::new("/tmp/liberado-test-proposals").to_path_buf(),
        "test goal".into(),
        "test-fail-closed-consequence".into(),
        ProposalSigner::random(),
        "default",
    )
}

#[tokio::test]
async fn an_mcp_missing_from_the_consequence_catalog_is_refused() {
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["undeclared-mcp:read_note"],
        Ok("should not run".into()),
    ));
    let descriptor = McpDescriptor {
        name: "undeclared-mcp".into(),
        description: "mcp in zone catalog only".into(),
        consequence: Consequence::Reversible,
        zone_from_arg: Some("path".into()),
        write_tools: Vec::new(),
        read_tools: vec!["read_note".into()],
        ..McpDescriptor::default()
    };
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("undeclared-mcp".into())]);
    let rt = gate_with_descriptor_but_no_consequence(inner.clone(), caps, descriptor);

    let call = ToolInvocation::new(
        "c1",
        "undeclared-mcp:read_note",
        serde_json::json!({"path": "anything/x.md"}),
    );
    let err = rt
        .invoke(&call)
        .await
        .expect_err("an undeclared MCP must be refused, not run");
    assert!(
        err.contains("not authorized") && err.contains("undeclared-mcp"),
        "the refusal must name the MCP: {err}"
    );
    assert!(
        err.contains("topology.toml"),
        "the refusal must tell the operator what to fix: {err}"
    );
    assert!(
        err.contains("consequence"),
        "the refusal must identify the consequence catalog as the gap: {err}"
    );
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "the inner tool must NEVER have run on a fail-closed refusal"
    );
}

#[tokio::test]
async fn a_fully_declared_read_runs() {
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["declared-mcp:read_note"],
        Ok("ok".into()),
    ));
    let descriptor = McpDescriptor {
        name: "declared-mcp".into(),
        description: "mcp in zone catalog only".into(),
        consequence: Consequence::Reversible,
        zone_from_arg: Some("path".into()),
        write_tools: Vec::new(),
        read_tools: vec!["read_note".into()],
        ..McpDescriptor::default()
    };
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("declared-mcp".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![
            ("declared-mcp".into(), Consequence::ReadOnly),
            ("some-other-mcp".into(), Consequence::ReadOnly),
        ],
        vec![descriptor],
        Vec::new(),
        Path::new("/tmp/liberado-test-proposals").to_path_buf(),
        "test goal".into(),
        "test-declared".into(),
        ProposalSigner::random(),
        "default",
    );

    let call = ToolInvocation::new(
        "c1",
        "declared-mcp:read_note",
        serde_json::json!({"path": "anything/x.md"}),
    );
    let result = rt.invoke(&call).await;
    assert_eq!(result, Ok("ok".into()));
    assert_eq!(inner.invoked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_truly_undeclared_mcp_is_refused_end_to_end() {
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["undeclared-mcp:write_note"],
        Ok("wrote".into()),
    ));
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("undeclared-mcp".into()),
        Capability::Write(Zone::vault("tasks")),
    ]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Path::new("/tmp/liberado-test-proposals").to_path_buf(),
        "test goal".into(),
        "test-fail-closed-target".into(),
        ProposalSigner::random(),
        "default",
    );

    let call = ToolInvocation::new(
        "c1",
        "undeclared-mcp:write_note",
        serde_json::json!({"path": "tasks/x.md"}),
    );
    let err = rt
        .invoke(&call)
        .await
        .expect_err("a truly undeclared MCP must be refused, not run");
    assert!(
        err.contains("not declared") || err.contains("not authorized"),
        "the refusal must point at the missing declaration: {err}"
    );
    assert!(
        err.contains("undeclared-mcp"),
        "the refusal must name the MCP: {err}"
    );
    assert!(
        err.contains("topology.toml"),
        "the refusal must tell the operator what to fix: {err}"
    );
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "the inner tool must NEVER have run"
    );
}

#[tokio::test]
async fn a_declared_zone_less_mcp_keeps_working() {
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["weather:forecast"],
        Ok("sunny".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("weather".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("weather".into(), Consequence::ReadOnly)],
        Vec::new(),
        Vec::new(),
        Path::new("/tmp/liberado-test-proposals").to_path_buf(),
        "what's the weather".into(),
        "test-zone-less".into(),
        ProposalSigner::random(),
        "default",
    );

    let call = ToolInvocation::new("c1", "weather:forecast", serde_json::json!({}));
    let result = rt.invoke(&call).await;
    assert_eq!(
        result,
        Ok("sunny".into()),
        "a declared MCP with no zone fields must keep working"
    );
    assert_eq!(inner.invoked.lock().unwrap().len(), 1);
}
