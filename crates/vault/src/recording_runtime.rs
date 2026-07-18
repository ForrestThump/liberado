//! Provenance recording for writes that go *through an MCP* (the model calling e.g. a TurboVault
//! `write_note` tool), so those writes land in liberado's [`crate::provenance_ledger`] alongside the
//! in-process ones and the daemon's loop-break attribution suppresses them.
//!
//! The in-process write path ([`Vault::write`](crate::Vault::write) etc.) records directly. MCP
//! writes happen inside the executor's tool runtime, a *kernel* layer that cannot depend on this
//! *store* crate. So instead this crate — which may depend downward on the kernel — provides a
//! [`RecordingRuntimeFactory`] the composition root (the daemon) wraps around the real MCP factory:
//! every runtime it produces is a [`ProvenanceRecordingRuntime`] that, after a successful write-tool
//! call, records the write to the ledger. It knows *which* calls write, and which argument holds the
//! path, from the MCP descriptors' `write_tools` + `zone_from_arg` (the same declarations the risk
//! gate resolves a write's target zone from) — so there's one source of truth, not a second list.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{McpDescriptor, WriteProvenance};
use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};
use liberado_provider::{ToolDef, ToolInvocation};

use crate::Vault;

/// `namespaced tool name ("mcp:tool") -> the argument naming the written path`. Built from the MCP
/// descriptors: a path-addressed MCP declares its write tools and the path argument, and that's
/// exactly what we need to record a write.
type WriteSpecs = Arc<HashMap<String, String>>;

/// Build the write-spec map from the MCP descriptors — every write tool of a path-addressed MCP
/// (`zone_from_arg` set) maps its namespaced name to that path argument.
pub fn write_specs_from_descriptors(descriptors: &[McpDescriptor]) -> HashMap<String, String> {
    let mut specs = HashMap::new();
    for d in descriptors {
        if let Some(path_arg) = &d.zone_from_arg {
            for tool in &d.write_tools {
                specs.insert(format!("{}:{}", d.name, tool), path_arg.clone());
            }
        }
    }
    specs
}

/// Wraps a [`RuntimeFactory`] so every runtime it hands out records MCP writes into the vault's
/// provenance ledger. A no-op wrapper when `write_specs` is empty (no path-addressed vault MCP).
pub struct RecordingRuntimeFactory {
    inner: Box<dyn RuntimeFactory>,
    vault: Vault,
    write_specs: WriteSpecs,
}

impl RecordingRuntimeFactory {
    pub fn new(
        inner: Box<dyn RuntimeFactory>,
        vault: Vault,
        write_specs: HashMap<String, String>,
    ) -> Self {
        Self {
            inner,
            vault,
            write_specs: Arc::new(write_specs),
        }
    }
}

#[async_trait]
impl RuntimeFactory for RecordingRuntimeFactory {
    async fn runtime_for(
        &self,
        allowed_mcps: &[String],
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        let inner = self.inner.runtime_for(allowed_mcps, provenance.clone()).await?;
        Ok(Box::new(ProvenanceRecordingRuntime {
            inner,
            vault: self.vault.clone(),
            provenance,
            write_specs: self.write_specs.clone(),
        }))
    }
}

/// A [`ToolRuntime`] that delegates to `inner` and, after a **successful** write-tool call, records
/// the write `(path, after_hash, provenance)` into the vault's ledger. Reads and non-write tools
/// pass straight through; a failed call records nothing (the write didn't happen).
pub struct ProvenanceRecordingRuntime {
    inner: Box<dyn ToolRuntime>,
    vault: Vault,
    provenance: WriteProvenance,
    write_specs: WriteSpecs,
}

#[async_trait]
impl ToolRuntime for ProvenanceRecordingRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.inner.catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        let result = self.inner.invoke(call).await;
        if result.is_ok() {
            if let Some(path_arg) = self.write_specs.get(&call.name) {
                if let Some(path) = call.arguments.get(path_arg).and_then(|v| v.as_str()) {
                    self.vault.record_external_write(path, &self.provenance).await;
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Attribution;
    use tempfile::TempDir;

    /// A fake MCP runtime that "performs the write" by writing the note straight to the vault dir —
    /// standing in for a TurboVault `write_note` landing bytes on disk.
    struct WritingRuntime {
        root: std::path::PathBuf,
    }

    #[async_trait]
    impl ToolRuntime for WritingRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
            // Mirror what the MCP server would do: write `content` to `path`.
            let path = call.arguments.get("path").and_then(|v| v.as_str()).unwrap();
            let content = call.arguments.get("content").and_then(|v| v.as_str()).unwrap();
            let full = self.root.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full, content).unwrap();
            Ok("ok".into())
        }
    }

    #[tokio::test]
    async fn mcp_write_is_recorded_and_attributed_to_the_agent() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open_with_ledger("test", dir.path(), dir.path().join(".prov.jsonl"))
            .await
            .unwrap();

        let mut specs = HashMap::new();
        specs.insert("turbovault:write_note".to_string(), "path".to_string());

        let runtime = ProvenanceRecordingRuntime {
            inner: Box::new(WritingRuntime {
                root: dir.path().to_path_buf(),
            }),
            vault: vault.clone(),
            provenance: WriteProvenance::agent("tasks-mcp", "corr-1"),
            write_specs: Arc::new(specs),
        };

        // The agent writes a note through the (fake) MCP.
        runtime
            .invoke(&ToolInvocation::new(
                "c",
                "turbovault:write_note",
                serde_json::json!({ "path": "note.md", "content": "# from the agent" }),
            ))
            .await
            .unwrap();

        // A Vault over the same dir + ledger attributes the change to the agent → suppress.
        match vault.attribute("note.md").await.unwrap() {
            Attribution::Agent(p) => {
                assert_eq!(p.source, "tasks-mcp");
                assert_eq!(p.correlation_id.as_deref(), Some("corr-1"));
            }
            other => panic!("expected Agent attribution for our MCP write, got {other:?}"),
        }
        assert!(!vault.should_react("note.md").await.unwrap());
    }

    #[tokio::test]
    async fn a_read_tool_call_records_nothing() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open_with_ledger("test", dir.path(), dir.path().join(".prov.jsonl"))
            .await
            .unwrap();
        // An external note exists on disk (a human wrote it); no write-spec matches a read.
        std::fs::write(dir.path().join("note.md"), "human content").unwrap();

        let runtime = ProvenanceRecordingRuntime {
            inner: Box::new(WritingRuntime {
                root: dir.path().to_path_buf(),
            }),
            vault: vault.clone(),
            provenance: WriteProvenance::agent("a", "c"),
            write_specs: Arc::new(HashMap::new()), // nothing is a write tool
        };
        let _ = runtime
            .invoke(&ToolInvocation::new(
                "c",
                "turbovault:read_note",
                serde_json::json!({ "path": "note.md", "content": "ignored" }),
            ))
            .await;

        // Not recorded → the human's content attributes External.
        assert_eq!(
            vault.attribute("note.md").await.unwrap(),
            Attribution::External
        );
    }
}
