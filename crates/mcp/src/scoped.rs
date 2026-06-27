//! # ScopedRuntime
//!
//! A [`ToolRuntime`] wrapper that filters the inner runtime's catalog and invocations to only
//! a specified set of allowed MCP names. This is the runtime-level enforcement of the
//! tool-advisor's output: after the advisor selects which MCPs are relevant, the model sees only
//! those tools in its catalog and any call to a scoped-out MCP is rejected.
//!
//! When `allowed_mcps` is empty, every tool passes through (no scoping) — this preserves the
//! default behavior when no advisor filtering is desired.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::mcp_of;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};

/// A runtime wrapper that limits the visible tool surface to a set of allowed MCPs.
pub struct ScopedRuntime {
    inner: Arc<dyn ToolRuntime>,
    /// Allowed MCP names. Empty = pass-through (all tools visible).
    allowed_mcps: Vec<String>,
}

impl ScopedRuntime {
    /// Build a scoped runtime.
    ///
    /// When `allowed_mcps` is empty, every tool passes through with no filtering.
    pub fn new(inner: Arc<dyn ToolRuntime>, allowed_mcps: Vec<String>) -> Self {
        Self {
            inner,
            allowed_mcps,
        }
    }
}

#[async_trait]
impl ToolRuntime for ScopedRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        if self.allowed_mcps.is_empty() {
            // Empty allow-list = no scoping; return everything.
            return self.inner.catalog();
        }

        self.inner
            .catalog()
            .into_iter()
            .filter(|tool| {
                let mcp = mcp_of(&tool.name);
                self.allowed_mcps.iter().any(|allowed| allowed == mcp)
            })
            .collect()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if !self.allowed_mcps.is_empty() {
            let mcp = mcp_of(&call.name);
            if !self.allowed_mcps.iter().any(|allowed| allowed == mcp) {
                return Err(format!(
                    "MCP '{}' is not in scope for this turn (tool '{}')",
                    mcp,
                    call.name
                ));
            }
        }

        self.inner.invoke(call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// A mock inner runtime that records invocations.
    struct MockInner {
        tools: Vec<ToolDef>,
        invoked: Mutex<Vec<ToolInvocation>>,
        result: Result<String, String>,
    }

    impl MockInner {
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
    impl ToolRuntime for MockInner {
        fn catalog(&self) -> Vec<ToolDef> {
            self.tools.clone()
        }

        async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
            self.invoked.lock().unwrap().push(call.clone());
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn filters_catalog_to_allowed_mcps() {
        let inner = Arc::new(MockInner::new(
            &["tasks-mcp:add", "tasks-mcp:list", "email-mcp:send", "memory-mcp:recall"],
            Ok("ok".into()),
        ));
        let scoped = ScopedRuntime::new(inner, vec!["tasks-mcp".into(), "memory-mcp".into()]);

        let catalog = scoped.catalog();
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();

        assert_eq!(names.len(), 3);
        assert!(names.contains(&"tasks-mcp:add"));
        assert!(names.contains(&"tasks-mcp:list"));
        assert!(names.contains(&"memory-mcp:recall"));
        assert!(!names.contains(&"email-mcp:send"));
    }

    #[tokio::test]
    async fn rejects_invoke_for_scoped_out_mcp() {
        let inner = Arc::new(MockInner::new(
            &["tasks-mcp:add", "email-mcp:send"],
            Ok("ok".into()),
        ));
        let scoped = ScopedRuntime::new(inner, vec!["tasks-mcp".into()]);

        let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({}));
        let result = scoped.invoke(&call).await;
        assert!(result.is_err(), "scoped-out MCP should be rejected");
        assert!(result.unwrap_err().contains("not in scope"));
    }

    #[tokio::test]
    async fn empty_allow_list_passes_everything() {
        let inner = Arc::new(MockInner::new(
            &["tasks-mcp:add", "email-mcp:send"],
            Ok("ok".into()),
        ));
        let scoped = ScopedRuntime::new(inner, vec![]);

        let catalog = scoped.catalog();
        assert_eq!(catalog.len(), 2);

        let call = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({}));
        let result = scoped.invoke(&call).await;
        assert_eq!(result, Ok("ok".into()));
    }

    #[tokio::test]
    async fn allowed_invoke_passes_through() {
        let inner = Arc::new(MockInner::new(
            &["tasks-mcp:add"],
            Ok("added".into()),
        ));
        let scoped = ScopedRuntime::new(inner, vec!["tasks-mcp".into()]);

        let call = ToolInvocation::new("c1", "tasks-mcp:add", serde_json::json!({"title": "milk"}));
        let result = scoped.invoke(&call).await;
        assert_eq!(result, Ok("added".into()));
    }
}
