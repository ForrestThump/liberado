//! Tests the multi-server registry against real in-process MCP servers (channel transport): tool
//! namespacing across servers, call routing, and `allowed_mcps` server selection (Decision 4).

use core::future::Future;
use liberado_common::WriteProvenance;
use liberado_executor::{RuntimeFactory, RuntimeSetupError};
use liberado_mcp::{McpConnector, McpRegistry, TurbomcpRuntime};
use liberado_provider::ToolInvocation;
use serde_json::Value;
use turbomcp_client::Client;
use turbomcp_core::context::RequestContext;
use turbomcp_core::error::{McpError, McpResult};
use turbomcp_core::handler::McpHandler;
use turbomcp_core::marker::MaybeSend;
use turbomcp_types::{
    Prompt, PromptResult, Resource, ResourceResult, ServerInfo, Tool, ToolResult,
};

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
        let (transport, _server) =
            turbomcp_server::transport::channel::run_in_process(&self.server)
                .await
                .map_err(|e| RuntimeSetupError(e.to_string()))?;
        let runtime = TurbomcpRuntime::connect(Client::new(transport), provenance)
            .await
            .map_err(|e| RuntimeSetupError(e.to_string()))?;
        Ok(Box::new(runtime))
    }
}

/// A connector that always fails to connect — simulates a misconfigured or hung MCP server
/// (e.g. `liberado-weather-mcp` defaulting to HTTP when stdio was expected).
struct FailingConnector;

#[async_trait::async_trait]
impl McpConnector for FailingConnector {
    async fn connect(
        &self,
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn liberado_executor::ToolRuntime>, RuntimeSetupError> {
        Err(RuntimeSetupError("deliberate failure".into()))
    }
}

/// A connector that succeeds after an injected delay — proves `connect_all_best_effort` runs
/// connectors concurrently rather than one after another.
struct SlowConnector {
    server: EchoServer,
    delay: std::time::Duration,
}

#[async_trait::async_trait]
impl McpConnector for SlowConnector {
    async fn connect(
        &self,
        provenance: WriteProvenance,
    ) -> Result<Box<dyn liberado_executor::ToolRuntime>, RuntimeSetupError> {
        tokio::time::sleep(self.delay).await;
        let (transport, _server) =
            turbomcp_server::transport::channel::run_in_process(&self.server)
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
    assert_eq!(
        runtime.invoke(&call("memory:store")).await.unwrap(),
        "stored"
    );
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

#[tokio::test]
async fn best_effort_connects_the_healthy_ones_and_reports_the_failed() {
    // One healthy server + one broken one — the healthy one's tools must still be usable, and the
    // broken one must be named, not silently swallowed or allowed to abort the whole connect.
    let registry = registry().register("weather", FailingConnector);

    let (runtime, failed) = registry.connect_all_best_effort(prov()).await;

    assert_eq!(failed, vec!["weather".to_string()]);
    let names: Vec<String> = runtime.catalog().iter().map(|t| t.name.clone()).collect();
    assert!(names.contains(&"tasks:add".to_string()), "{names:?}");
    assert!(names.contains(&"memory:store".to_string()), "{names:?}");
    assert!(
        !names.iter().any(|n| n.starts_with("weather:")),
        "{names:?}"
    );
    assert_eq!(runtime.invoke(&call("tasks:add")).await.unwrap(), "added");
}

#[tokio::test]
async fn best_effort_with_every_connector_failing_yields_an_empty_but_valid_runtime() {
    let registry = McpRegistry::new()
        .register("weather", FailingConnector)
        .register("caldav", FailingConnector);

    let (runtime, mut failed) = registry.connect_all_best_effort(prov()).await;
    failed.sort();

    assert_eq!(failed, vec!["caldav".to_string(), "weather".to_string()]);
    assert!(runtime.catalog().is_empty());
    let err = runtime.invoke(&call("weather:forecast")).await.unwrap_err();
    assert!(err.contains("weather"), "{err}");
}

#[tokio::test]
async fn best_effort_with_every_connector_healthy_matches_runtime_for() {
    let (runtime, failed) = registry().connect_all_best_effort(prov()).await;

    assert!(failed.is_empty());
    let mut names: Vec<String> = runtime.catalog().iter().map(|t| t.name.clone()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["memory:store".to_string(), "tasks:add".to_string()]
    );
}

#[tokio::test]
async fn best_effort_connects_concurrently_not_sequentially() {
    // Three servers each delayed ~200ms: if connections ran sequentially this would take ~600ms.
    // Bound generously above one delay period to avoid CI flakiness while still catching a
    // regression to sequential connection.
    let delay = std::time::Duration::from_millis(200);
    let registry = McpRegistry::new()
        .register(
            "a",
            SlowConnector {
                server: EchoServer {
                    tool: "t",
                    reply: "r",
                },
                delay,
            },
        )
        .register(
            "b",
            SlowConnector {
                server: EchoServer {
                    tool: "t",
                    reply: "r",
                },
                delay,
            },
        )
        .register(
            "c",
            SlowConnector {
                server: EchoServer {
                    tool: "t",
                    reply: "r",
                },
                delay,
            },
        );

    let start = std::time::Instant::now();
    let (runtime, failed) = registry.connect_all_best_effort(prov()).await;
    let elapsed = start.elapsed();

    assert!(failed.is_empty());
    assert_eq!(runtime.catalog().len(), 3);
    assert!(
        elapsed < delay * 2,
        "expected ~{delay:?} (parallel), took {elapsed:?} — looks sequential"
    );
}
