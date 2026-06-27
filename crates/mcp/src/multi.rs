//! A [`ToolRuntime`] that merges the catalogs of several connected runtimes and dispatches
//! invocations to the correct one based on the `mcp_of()` prefix.
//!
//! Each server is registered under a name; its tools appear as `<name>:<tool>` in the merged
//! catalog, and an invoke to `<name>:<tool>` is routed to that server's runtime with the prefix
//! stripped back off. This is the runtime-level realisation of Decision 4's server-selection
//! narrowing — after the advisor selects which MCPs are relevant, this merged surface presents
//! only those servers' tools.

use std::collections::HashMap;

use async_trait::async_trait;
use liberado_common::mcp_of;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};

/// A [`ToolRuntime`] backed by several sub-runtimes, each under a string key.
///
/// The catalog is the union of all sub-runtime catalogs, with each tool's name prefixed by its
/// server name (`<server>:<tool>`). An [`invoke`](ToolRuntime::invoke) call extracts the server
/// from the tool name via [`mcp_of`], finds the matching sub-runtime, strips the prefix, and
/// forwards the call.
///
/// When no servers are registered, the catalog is empty and every invocation fails.
pub struct MultiMcpRuntime {
    runtimes: HashMap<String, Box<dyn ToolRuntime>>,
}

impl MultiMcpRuntime {
    /// Build a merged runtime from a list of `(name, runtime)` pairs. The order of registration
    /// does not affect routing (routing is key-based); catalog order is undefined.
    pub fn new(servers: Vec<(String, Box<dyn ToolRuntime>)>) -> Self {
        Self {
            runtimes: servers.into_iter().collect(),
        }
    }

    /// `true` when there are no registered sub-runtimes.
    pub fn is_empty(&self) -> bool {
        self.runtimes.is_empty()
    }

    /// The number of registered sub-runtimes.
    pub fn len(&self) -> usize {
        self.runtimes.len()
    }

    /// The names of the registered sub-runtimes (the routable keys).
    pub fn names(&self) -> Vec<&str> {
        self.runtimes.keys().map(String::as_str).collect()
    }
}

#[async_trait]
impl ToolRuntime for MultiMcpRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.runtimes
            .iter()
            .flat_map(|(name, runtime)| {
                runtime.catalog().into_iter().map(move |mut tool| {
                    tool.name = format!("{name}:{}", tool.name);
                    tool
                })
            })
            .collect()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        let server = mcp_of(&call.name);
        let Some((name, runtime)) = self.runtimes.get_key_value(server) else {
            return Err(format!(
                "no MCP named '{server}' is in scope for tool '{}'",
                call.name
            ));
        };
        let bare = call
            .name
            .strip_prefix(&format!("{name}:"))
            .unwrap_or(&call.name);
        let inner = ToolInvocation::new(call.id.clone(), bare, call.arguments.clone());
        runtime.invoke(&inner).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A mock runtime that records invocations and returns a canned result.
    struct MockRuntime {
        tools: Vec<ToolDef>,
        invoked: Mutex<Vec<ToolInvocation>>,
        result: Result<String, String>,
    }

    impl MockRuntime {
        fn new(tool_names: &[&str], result: Result<String, String>) -> Self {
            let tools = tool_names
                .iter()
                .map(|n| ToolDef::new(*n, "test tool", serde_json::json!({ "type": "object" })))
                .collect();
            Self {
                tools,
                invoked: Mutex::new(Vec::new()),
                result,
            }
        }
    }

    #[async_trait]
    impl ToolRuntime for MockRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            self.tools.clone()
        }

        async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
            self.invoked.lock().unwrap().push(call.clone());
            self.result.clone()
        }
    }

    fn make_tasks_runtime() -> MockRuntime {
        MockRuntime::new(&["create", "list"], Ok("ok".into()))
    }

    fn make_wiki_runtime() -> MockRuntime {
        MockRuntime::new(&["search", "get_page"], Ok("found".into()))
    }

    #[tokio::test]
    async fn routes_tasks_mcp_call() {
        let tasks_rt = make_tasks_runtime();
        let wiki_rt = make_wiki_runtime();

        let multi = MultiMcpRuntime::new(vec![
            ("tasks".into(), Box::new(tasks_rt)),
            ("wiki".into(), Box::new(wiki_rt)),
        ]);

        let call = ToolInvocation::new("c1", "tasks:create", serde_json::json!({"title": "test"}));
        let result = multi.invoke(&call).await;
        assert_eq!(result, Ok("ok".into()));

        // Can't access the inner runtime's invoked log directly since it's been moved.
        // Verify routing by checking the call succeeded end-to-end.
    }

    #[tokio::test]
    async fn routes_wiki_call() {
        let tasks_rt = make_tasks_runtime();
        let wiki_rt = make_wiki_runtime();

        let multi = MultiMcpRuntime::new(vec![
            ("tasks".into(), Box::new(tasks_rt)),
            ("wiki".into(), Box::new(wiki_rt)),
        ]);

        let call = ToolInvocation::new("c2", "wiki:search", serde_json::json!({"q": "hello"}));
        let result = multi.invoke(&call).await;
        assert_eq!(result, Ok("found".into()));
    }

    #[tokio::test]
    async fn catalog_returns_all_servers_tools_prefixed() {
        let tasks_rt = MockRuntime::new(&["create", "list"], Ok("ok".into()));
        let wiki_rt = MockRuntime::new(&["search", "get_page"], Ok("ok".into()));

        let multi = MultiMcpRuntime::new(vec![
            ("tasks".into(), Box::new(tasks_rt)),
            ("wiki".into(), Box::new(wiki_rt)),
        ]);

        let catalog = multi.catalog();
        let mut names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
        names.sort();

        assert_eq!(names, vec!["tasks:create", "tasks:list", "wiki:get_page", "wiki:search"]);
    }

    #[tokio::test]
    async fn rejects_call_to_unregistered_server() {
        let tasks_rt = MockRuntime::new(&["create"], Ok("ok".into()));

        let multi = MultiMcpRuntime::new(vec![("tasks".into(), Box::new(tasks_rt))]);

        let call = ToolInvocation::new("c3", "unknown:foo", serde_json::json!({}));
        let err = multi.invoke(&call).await.unwrap_err();
        assert!(err.contains("unknown"), "error should mention the unknown server: {err}");
    }

    #[tokio::test]
    async fn empty_runtime_returns_empty_catalog() {
        let multi = MultiMcpRuntime::new(vec![]);
        assert!(multi.is_empty());
        assert_eq!(multi.len(), 0);
        assert!(multi.catalog().is_empty());
    }

    #[tokio::test]
    async fn names_returns_registered_keys() {
        let rt = MockRuntime::new(&["a"], Ok("ok".into()));
        let multi = MultiMcpRuntime::new(vec![
            ("alpha".into(), Box::new(rt)),
        ]);
        assert_eq!(multi.names(), vec!["alpha"]);
    }
}
