//! How to connect to one MCP server. A connector is **transport-erased** — it returns a boxed
//! [`ToolRuntime`], not a typed client — so servers of different transports (a `npx` stdio server, a
//! remote HTTP server) can sit side by side in one [`McpRegistry`](crate::McpRegistry).
//!
//! Each connector exposes its server's tools under their **bare** names; the registry namespaces
//! them by the name the server is registered under.

use async_trait::async_trait;
use liberado_common::WriteProvenance;
use liberado_executor::{RuntimeSetupError, ToolRuntime};
use turbomcp_client::Client;
use turbomcp_transport::{ChildProcessConfig, ChildProcessTransport};

use crate::TurbomcpRuntime;

/// Connect to one MCP server and return a runtime bound to `provenance` (injected into every call's
/// `_meta`). Connection/handshake failures surface here.
#[async_trait]
pub trait McpConnector: Send + Sync {
    async fn connect(
        &self,
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError>;
}

/// Run an MCP server as a child process and speak MCP over its stdin/stdout — the standard way to
/// host npm/`npx` servers (`StdioConnector::new("npx", vec!["-y", "@scope/server"])`) and Rust
/// server binaries alike. The process is spawned lazily on first connect.
#[derive(Debug, Clone)]
pub struct StdioConnector {
    config: ChildProcessConfig,
}

impl StdioConnector {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            config: ChildProcessConfig {
                command: command.into(),
                args,
                ..Default::default()
            },
        }
    }

    /// Full control over the spawn (working dir, env, timeouts, …).
    pub fn from_config(config: ChildProcessConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl McpConnector for StdioConnector {
    async fn connect(
        &self,
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        let client = Client::new(ChildProcessTransport::new(self.config.clone()));
        let runtime = TurbomcpRuntime::connect(client, provenance)
            .await
            .map_err(|e| RuntimeSetupError(e.to_string()))?;
        Ok(Box::new(runtime))
    }
}

/// Connect to a remote MCP server over streamable HTTP (e.g. deepwiki at
/// `https://mcp.deepwiki.com`). `url` is the server's **base origin**, not its full MCP endpoint
/// path — `Client::connect_http` appends its own default endpoint path (`/mcp`) internally, so
/// passing an already-`/mcp`-suffixed URL here doubles it up (`.../mcp/mcp`) and 404s. Confirmed
/// live against deepwiki before landing this doc fix.
#[derive(Debug, Clone)]
pub struct HttpConnector {
    url: String,
}

impl HttpConnector {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

#[async_trait]
impl McpConnector for HttpConnector {
    async fn connect(
        &self,
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        // `connect_http` performs the initialize handshake itself; `TurbomcpRuntime::connect` then
        // skips re-initializing (it checks `is_initialized`).
        let client = Client::connect_http(self.url.clone())
            .await
            .map_err(|e| RuntimeSetupError(e.to_string()))?;
        let runtime = TurbomcpRuntime::connect(client, provenance)
            .await
            .map_err(|e| RuntimeSetupError(e.to_string()))?;
        Ok(Box::new(runtime))
    }
}
