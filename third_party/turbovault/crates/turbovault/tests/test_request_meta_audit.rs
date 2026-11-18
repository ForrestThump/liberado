//! Integration test: the `write_note` MCP tool records request `_meta` on the audit log.
//!
//! This is the end of the provenance wire. A request's `_meta` — the MCP spec's open per-request
//! object, surfaced into the handler context by the router — must land on the write's audit entry,
//! so a separate process (a reactive daemon, say) can re-open the persisted log and attribute the
//! change to its originator instead of reacting to its own output.
//!
//! Both tests drive the real server handler and then re-open the persisted JSONL audit log from
//! disk, which is the same cross-instance read the production loop performs.

use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;
use turbovault_audit::{AuditFilter, AuditLog};
use turbovault_core::VaultConfig;

/// The wire key, spelled literally rather than imported: the point of these tests is the contract
/// a *client* sees, so a typo in the server's own constant should fail them.
const WIRE_META_KEY: &str = "_meta";

async fn server_over(vault_path: &std::path::Path) -> ObsidianMcpServer {
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
    server
}

async fn audit_entries(vault_path: &std::path::Path) -> Vec<turbovault_audit::AuditEntry> {
    let audit = AuditLog::new(vault_path).await.expect("open audit log");
    audit
        .query(&AuditFilter::new().with_limit(100))
        .await
        .expect("query audit")
}

fn provenance() -> serde_json::Value {
    serde_json::json!({
        "_liberado_provenance": { "source": "tasks-mcp", "correlation_id": "corr-1" }
    })
}

/// The create path — `write_note` with no hash and no force is create-by-default.
#[tokio::test]
async fn write_note_records_request_meta_when_creating() {
    let temp = TempDir::new().expect("temp dir");
    let server = server_over(temp.path()).await;
    let meta = provenance();

    server
        .call_tool(
            "write_note",
            serde_json::json!({ "path": "note.md", "content": "# Hello" }),
            &RequestContext::new().with_metadata(WIRE_META_KEY, meta.clone()),
        )
        .await
        .expect("write_note");

    let entries = audit_entries(temp.path()).await;
    let entry = entries
        .iter()
        .find(|e| e.path == "note.md")
        .unwrap_or_else(|| panic!("no audit entry for the write; entries: {entries:?}"));
    assert_eq!(
        entry.metadata, meta,
        "audit metadata should equal the request _meta verbatim"
    );
}

/// The overwrite path — a different branch of `write_note`, so it needs its own coverage.
#[tokio::test]
async fn write_note_records_request_meta_when_overwriting() {
    let temp = TempDir::new().expect("temp dir");
    let server = server_over(temp.path()).await;
    let meta = provenance();

    server
        .call_tool(
            "write_note",
            serde_json::json!({ "path": "note.md", "content": "first" }),
            &RequestContext::new(),
        )
        .await
        .expect("seed write");

    server
        .call_tool(
            "write_note",
            serde_json::json!({ "path": "note.md", "content": "second", "force": true }),
            &RequestContext::new().with_metadata(WIRE_META_KEY, meta.clone()),
        )
        .await
        .expect("overwrite");

    let entries = audit_entries(temp.path()).await;
    // Newest first, so the overwrite is the first entry for this path.
    let entry = entries
        .iter()
        .find(|e| e.path == "note.md")
        .unwrap_or_else(|| panic!("no audit entry for the overwrite; entries: {entries:?}"));
    assert_eq!(entry.metadata, meta);
}

/// A request without `_meta` must behave exactly as before: no metadata on the entry.
#[tokio::test]
async fn a_request_without_meta_records_no_metadata() {
    let temp = TempDir::new().expect("temp dir");
    let server = server_over(temp.path()).await;

    server
        .call_tool(
            "write_note",
            serde_json::json!({ "path": "note.md", "content": "# Hello" }),
            &RequestContext::new(),
        )
        .await
        .expect("write_note");

    let entries = audit_entries(temp.path()).await;
    let entry = entries.iter().find(|e| e.path == "note.md").expect("entry");
    assert_eq!(
        entry.metadata,
        serde_json::json!({}),
        "absent _meta must leave the entry's default untouched"
    );
}
