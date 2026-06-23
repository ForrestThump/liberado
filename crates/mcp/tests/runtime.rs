//! Tests for `TurbomcpRuntime` against a real in-process MCP server (channel transport): catalog
//! mapping, tool invocation, provenance injected into `_meta`, and in-band tool errors.

use core::future::Future;
use liberado_common::WriteProvenance;
use liberado_executor::ToolRuntime;
use liberado_mcp::TurbomcpRuntime;
use liberado_provider::ToolInvocation;
use serde_json::Value;
use turbomcp_client::Client;
use turbomcp_core::context::{REQUEST_META_KEY, RequestContext};
use turbomcp_core::error::{McpError, McpResult};
use turbomcp_core::handler::McpHandler;
use turbomcp_core::marker::MaybeSend;
use turbomcp_types::{
    Prompt, PromptResult, Resource, ResourceResult, ServerInfo, Tool, ToolResult,
};

/// A handler exposing two tools: `search` (so the catalog has something to map) and `echo_meta`
/// (returns the request `_meta` it received, so a test can prove provenance reached the server).
#[derive(Clone)]
struct TestServer;

impl McpHandler for TestServer {
    fn server_info(&self) -> ServerInfo {
        ServerInfo::new("test-mcp", "1.0.0")
    }

    fn list_tools(&self) -> Vec<Tool> {
        vec![
            Tool::new("search", "Find things in the corpus"),
            Tool::new("echo_meta", "Echo the request _meta"),
        ]
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
        ctx: &'a RequestContext,
    ) -> impl Future<Output = McpResult<ToolResult>> + MaybeSend + 'a {
        let echoed = ctx
            .get_metadata(REQUEST_META_KEY)
            .map(|m| m.to_string())
            .unwrap_or_else(|| "<none>".to_string());
        let name = name.to_string();
        async move {
            match name.as_str() {
                "search" => Ok(ToolResult::text("2 results")),
                "echo_meta" => Ok(ToolResult::text(echoed)),
                _ => Err(McpError::tool_not_found(&name)),
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

async fn runtime() -> TurbomcpRuntime<turbomcp_server::transport::channel::ChannelTransport> {
    let (transport, _server) = turbomcp_server::transport::channel::run_in_process(&TestServer)
        .await
        .expect("start in-process server");
    let client = Client::new(transport);
    TurbomcpRuntime::connect(client, WriteProvenance::agent("tasks-mcp", "corr-1"))
        .await
        .expect("connect runtime")
}

fn invocation(name: &str) -> ToolInvocation {
    ToolInvocation::new("call-1", name, serde_json::json!({}))
}

#[tokio::test]
async fn connect_maps_the_server_catalog() {
    let runtime = runtime().await;
    let catalog = runtime.catalog();

    let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"search"), "catalog: {names:?}");
    assert!(names.contains(&"echo_meta"), "catalog: {names:?}");

    let search = catalog.iter().find(|t| t.name == "search").unwrap();
    assert_eq!(search.description, "Find things in the corpus");
    assert!(search.parameters.is_object(), "schema should be an object");
}

#[tokio::test]
async fn invoke_runs_the_tool_and_returns_its_text() {
    let runtime = runtime().await;
    let out = runtime.invoke(&invocation("search")).await.unwrap();
    assert_eq!(out, "2 results");
}

#[tokio::test]
async fn invoke_injects_write_provenance_into_meta() {
    let runtime = runtime().await;
    // The server echoes the `_meta` it received; it must carry our provenance.
    let echoed = runtime.invoke(&invocation("echo_meta")).await.unwrap();
    assert!(
        echoed.contains("tasks-mcp") && echoed.contains("corr-1"),
        "provenance not injected into _meta; got: {echoed}"
    );
    assert!(
        echoed.contains("_liberado_provenance"),
        "expected the reserved provenance key; got: {echoed}"
    );
}

#[tokio::test]
async fn unknown_tool_surfaces_as_err() {
    let runtime = runtime().await;
    let result = runtime.invoke(&invocation("does_not_exist")).await;
    assert!(
        result.is_err(),
        "expected Err for unknown tool, got {result:?}"
    );
}
