//! # liberado-mcp
//!
//! The production [`ToolRuntime`] (the trait the executor's agent loop drives) backed by an MCP
//! server over `turbomcp-client`. It does three things:
//!
//! 1. **Catalog** — fetches the server's tools once at connect and maps each to the provider's
//!    [`ToolDef`], so the agent loop can offer them to the model.
//! 2. **Invoke** — runs a model-requested tool via `call_tool_with_meta`, converting the
//!    [`CallToolResult`] back to a string for the loop (and surfacing an `isError` result as an
//!    in-band `Err` the model can react to).
//! 3. **Provenance** — injects the dispatch's [`WriteProvenance`] into every call's `_meta`, so any
//!    vault write the tool performs is recorded on the audit log as *ours* and the daemon's
//!    attribution suppresses it (Decision 5 loop-breaking, validated end-to-end in
//!    `liberado-vault`'s `provenance_e2e`).
//!
//! The runtime is constructed per execution (per dispatch correlation): the `WriteProvenance` it
//! carries is fixed for its lifetime, so every tool call in that execution shares one correlation.

mod connector;
mod factory;
mod multi;
mod risk_gated;
mod scoped;

pub use connector::{HttpConnector, McpConnector, StdioConnector};
pub use factory::McpRegistry;
pub use multi::MultiMcpRuntime;
pub use risk_gated::RiskGatedToolRuntime;
pub use scoped::ScopedRuntime;

use async_trait::async_trait;
use liberado_common::WriteProvenance;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use serde_json::Value;
use thiserror::Error;
use turbomcp_client::{CallToolResult, Client, Tool, Transport};

/// Errors raised while establishing the runtime. Per-call tool failures are *not* here — they are
/// returned in-band from [`ToolRuntime::invoke`] so the model can adapt.
#[derive(Debug, Error)]
pub enum McpRuntimeError {
    #[error("MCP initialize failed: {0}")]
    Initialize(String),
    #[error("listing MCP tools failed: {0}")]
    ListTools(String),
}

/// A [`ToolRuntime`] backed by a connected `turbomcp-client` [`Client`], scoped to one execution's
/// write provenance.
pub struct TurbomcpRuntime<T: Transport + 'static> {
    client: Client<T>,
    catalog: Vec<ToolDef>,
    /// Pre-rendered `_meta` payload (`{ "_liberado_provenance": { … } }`) injected into every call.
    provenance_meta: Value,
}

impl<T: Transport + 'static> TurbomcpRuntime<T> {
    /// Initialize the MCP session, fetch + map the tool catalog, and bind the provenance that every
    /// call in this execution will carry.
    pub async fn connect(
        client: Client<T>,
        provenance: WriteProvenance,
    ) -> Result<Self, McpRuntimeError> {
        // Some connectors (e.g. HTTP via `connect_http`) initialize during connection; only
        // initialize here when the client hasn't been already, so either path is safe.
        if !client.is_initialized() {
            client
                .initialize()
                .await
                .map_err(|e| McpRuntimeError::Initialize(e.to_string()))?;
        }

        let tools = client
            .list_tools()
            .await
            .map_err(|e| McpRuntimeError::ListTools(e.to_string()))?;
        let catalog = tools.iter().map(to_tool_def).collect();

        Ok(Self {
            client,
            catalog,
            provenance_meta: provenance.to_audit_metadata(),
        })
    }
}

#[async_trait]
impl<T: Transport + 'static> ToolRuntime for TurbomcpRuntime<T> {
    fn catalog(&self) -> Vec<ToolDef> {
        self.catalog.clone()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        let args = arguments_to_map(&call.arguments);
        let result = self
            .client
            .call_tool_with_meta(&call.name, args, Some(self.provenance_meta.clone()))
            .await
            .map_err(|e| e.to_string())?;

        let text = result_text(&result);
        // MCP's idiomatic tool failure: `isError` true means the content is error info. Surface it
        // as `Err` so the executor feeds it back in-band (it prefixes "tool error:").
        if result.is_error == Some(true) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

/// Map an MCP [`Tool`] to the provider's [`ToolDef`]. The tool's `input_schema` is serialized to a
/// JSON-Schema object for `parameters`; an empty/odd schema falls back to a permissive object.
fn to_tool_def(tool: &Tool) -> ToolDef {
    let parameters = serde_json::to_value(&tool.input_schema)
        .unwrap_or_else(|_| serde_json::json!({ "type": "object" }));
    ToolDef::new(
        tool.name.clone(),
        tool.description.clone().unwrap_or_default(),
        parameters,
    )
}

/// Convert a model-produced arguments value into the `name -> value` map the client expects. A
/// non-object (or absent) value yields no arguments rather than an error.
fn arguments_to_map(arguments: &Value) -> Option<std::collections::HashMap<String, Value>> {
    arguments
        .as_object()
        .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// Flatten a tool result into the text the agent loop feeds back to the model: concatenated text
/// blocks, or the serialized structured content when there are no text blocks.
fn result_text(result: &CallToolResult) -> String {
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        text
    } else if let Some(structured) = &result.structured_content {
        structured.to_string()
    } else {
        String::new()
    }
}
