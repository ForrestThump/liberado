//! End-to-end capstone: the provenance loop closes across real components.
//!
//! Gated behind the `e2e` feature because it drives the turbovault **host** MCP server, pulling the
//! whole server graph into the test build. Run it with:
//!
//! ```text
//! cargo test -p liberado-vault --features e2e
//! ```
//!
//! It was previously unrunnable for a different reason — the host would not build against our local
//! turbomcp fork — which turned out to be a `[patch.crates-io]` that named only the `turbomcp`
//! umbrella. Two copies of `turbomcp-core` then landed in one graph and its types stopped unifying.
//! Patching every `turbomcp-*` crate the tree names (see the root `Cargo.toml`) fixed it.
#![cfg(feature = "e2e")]
//!
//! An agent writes a note **through the turbovault MCP server**, carrying its provenance in the
//! request `_meta`. That provenance rides the chain turbomcp surfaces and turbovault records on the
//! audit log. A *separate* liberado `Vault` (standing in for the daemon, a different process in
//! production) then attributes the change — and must recognize it as the agent's own write and
//! suppress it, rather than reacting to it. This exercises every link the unit/integration tests
//! cover, wired together for real.

use liberado_common::WriteProvenance;
use liberado_vault::{Attribution, Vault};
use tempfile::TempDir;
use turbomcp::{McpHandler, REQUEST_META_KEY, RequestContext};
use turbovault::ObsidianMcpServer;
use turbovault_core::VaultConfig;

#[tokio::test]
async fn mcp_write_carrying_provenance_is_attributed_to_the_agent_and_suppressed() {
    let temp = TempDir::new().expect("temp dir");
    let vault_path = temp.path();

    // 1. The agent writes through the MCP server, attaching provenance via request `_meta`.
    let server = ObsidianMcpServer::new().expect("server");
    let config = VaultConfig::builder("test", vault_path.to_str().unwrap())
        .build()
        .expect("vault config");
    server
        .multi_vault()
        .add_vault(config)
        .await
        .expect("add vault");
    server
        .multi_vault()
        .set_active_vault("test")
        .await
        .expect("set active");

    let provenance = WriteProvenance::agent("tasks-mcp", "corr-1");
    let ctx = RequestContext::new().with_metadata(REQUEST_META_KEY, provenance.to_audit_metadata());
    server
        .call_tool(
            "write_note",
            serde_json::json!({ "path": "note.md", "content": "# Hello from the agent" }),
            &ctx,
        )
        .await
        .expect("write_note");

    // 2. A separate liberado Vault over the same directory attributes the change.
    let vault = Vault::open("liberado-daemon", vault_path)
        .await
        .expect("open vault");

    match vault.attribute("note.md").await.expect("attribute") {
        Attribution::Agent(p) => {
            assert_eq!(p.source, "tasks-mcp");
            assert_eq!(p.correlation_id.as_deref(), Some("corr-1"));
        }
        other => panic!("expected Agent attribution (our own MCP write), got {other:?}"),
    }

    // The loop is broken: the daemon does NOT react to a write its own agent made via MCP.
    assert!(
        !vault.should_react("note.md").await.expect("should_react"),
        "an agent's provenance-tagged MCP write must be suppressed, not reacted to"
    );
}
