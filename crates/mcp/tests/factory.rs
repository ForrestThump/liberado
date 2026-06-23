//! Tests the multi-server registry against real in-process MCP servers (channel transport): tool
//! namespacing across servers, call routing, and `allowed_mcps` server selection (Decision 4).

use core::future::Future;
use liberado_common::WriteProvenance;
use liberado_mcp::{McpConnector, McpRegistry, TurbomcpRuntime};
use liberado_orchestrator::{RuntimeFactory, RuntimeSetupError};
use liberado_provider::ToolInvocation;
use serde_json::Value;
use turbomcp_client::Client;
use turbomcp_core::context::RequestContext;
use turbomcp_core::error::{McpError, McpResult};
use turbomcp_core::handler::McpHandler;
use turbomcp_core::marker::MaybeSend;
use turbomcp_types::{Prompt, PromptResult, Resource, ResourceResult, ServerInfo, Tool, ToolResult};

/// A one-tool server (bare tool name + canned reply), so a registry can namespace it.
#[derive(Clone)]
struct EchoServer {
    tool: &'static str,
    reply: &'static str,
}

impl McpHandler for EchoServer {
    fn server_info(&self) -> ServerInfo {
        ServerInfo::new("echo", "1.0.0")
    }
    fn list_tools(&self) -> Vec<Tool> {
        vec![Tool::new(self.tool, "echo tool")]
    }
    fn list_resources(&self) -> Vec<Resource> {
        Vec::new()
    }
    fn list_prompts(&self) -> Vec<Prompt> {
        Vec::new()
    }
    fn call_tool<'a>(
        &'a self,
        name: &'a str,
        _args: Value,
        _ctx: &'a RequestContext,
    ) -> impl Future<Output = McpResult<ToolResult>> + MaybeSend + 'a {
        let matches = name == self.tool;
        let reply = self.reply;
        let name = name.to_string();
        async move {
            if matches {
                Ok(ToolResult::text(reply))
            } else {
                Err(McpError::tool_not_found(&name))
            }
        }
    }
    fn read_resource<'a>(
        &'a self,
        uri: &'a str,
        _ctx: &'a RequestContext,
    ) -> impl Future<Output = McpResult<ResourceResult>> + MaybeSend + 'a {
        let uri = uri.to_string();
        async move { Err(McpError::resource_not_found(&uri)) }
    }
    fn get_prompt<'a>(
        &'a self,
        name: &'a str,
        _args: Option<Value>,
        _ctx: &'a RequestContext,
    ) -> impl Future<Output = McpResult<PromptResult>> + MaybeSend + 'a {
        let name = name.to_string();
        async move { Err(McpError::prompt_not_found(&name)) }
    }
}

/// A connector that stands the given server up in-process and binds provenance.
struct ChannelConnector {
    server: EchoServer,
}

#[async_trait::async_trait]
impl McpConnector for ChannelConnector {
    async fn connect(
        &self,
        provenance: WriteProvenance,
    ) -> Result<Box<dyn liberado_executor::ToolRuntime>, RuntimeSetupError> {
        let (transport, _server) = turbomcp_server::transport::channel::run_in_process(&self.server)
            .await
            .map_err(|e| RuntimeSetupError(e.to_string()))?;
        let runtime = TurbomcpRuntime::connect(Client::new(transport), provenance)
            .await
            .map_err(|e| RuntimeSetupError(e.to_string()))?;
        Ok(Box::new(runtime))
    }
}

fn registry() -> McpRegistry {
    McpRegistry::new()
        .register(
            "tasks",
            ChannelConnector {
                server: EchoServer {
                    tool: "add",
                    reply: "added",
                },
            },
        )
        .register(
            "memory",
            ChannelConnector {
                server: EchoServer {
                    tool: "store",
                    reply: "stored",
                },
            },
        )
}

fn prov() -> WriteProvenance {
    WriteProvenance::agent("liberado-executor", "corr-1")
}

fn call(name: &str) -> ToolInvocation {
    ToolInvocation::new("c", name, serde_json::json!({}))
}

#[tokio::test]
async fn registry_namespaces_tools_across_servers_and_routes_calls() {
    let runtime = registry().runtime_for(&[], prov()).await.unwrap();

    let names: Vec<String> = runtime.catalog().iter().map(|t| t.name.clone()).collect();
    assert!(names.contains(&"tasks:add".to_string()), "{names:?}");
    assert!(names.contains(&"memory:store".to_string()), "{names:?}");

    // Each namespaced call routes to the owning server (prefix stripped before it arrives).
    assert_eq!(runtime.invoke(&call("tasks:add")).await.unwrap(), "added");
    assert_eq!(runtime.invoke(&call("memory:store")).await.unwrap(), "stored");
}

#[tokio::test]
async fn allowed_mcps_selects_which_servers_are_in_scope() {
    let runtime = registry()
        .runtime_for(&["tasks".to_string()], prov())
        .await
        .unwrap();

    // Only the selected server's tools are present.
    let names: Vec<String> = runtime.catalog().iter().map(|t| t.name.clone()).collect();
    assert_eq!(names, vec!["tasks:add".to_string()]);

    // A call into an out-of-scope server is refused (that server was never connected).
    let err = runtime.invoke(&call("memory:store")).await.unwrap_err();
    assert!(err.contains("memory"), "{err}");

    assert_eq!(runtime.invoke(&call("tasks:add")).await.unwrap(), "added");
}
