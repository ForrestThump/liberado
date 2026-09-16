//! # ScopedRuntime
//!
//! A [`ToolRuntime`] wrapper that filters the inner runtime's catalog and invocations to only
//! a specified set of allowed MCP names. This is the runtime-level enforcement of the
//! tool-advisor's output: after the advisor selects which MCPs are relevant, the model sees only
//! those tools in its catalog and any call to a scoped-out MCP is rejected.
//!
//! When `allowed_mcps` is empty, every tool passes through (no scoping) — this preserves the
//! default behavior when no advisor filtering is desired.
//!
//! # That empty-means-everything default is fail-open
//!
//! It is correct for the advisor ("I selected nothing, so do not filter") and dangerous for anything
//! else. A caller deriving the allow-list from a *capability grant* wants the opposite: a grant that
//! names no MCP must show no tools. Building this with `ScopedRuntime::new(inner, vec![])` from a
//! grant would hand the model the entire fleet.
//!
//! [`ScopedRuntime::from_capabilities`] is the constructor to use for a grant. It answers the
//! authorization question per tool via [`CapabilitySet::grants_tool`], so a grant of
//! `ExecuteTool("turbovault:read_note")` shows exactly that one tool — expressible no other way —
//! and an empty grant shows nothing.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{CapabilitySet, mcp_of};
use liberado_provider::{ToolDef, ToolInvocation};
use liberado_tool_runtime::{DecoratingRuntime, ToolRuntime};

/// How a [`ScopedRuntime`] decides what is in scope.
enum Scope {
    /// Allowed MCP names; empty means pass-through. The tool-advisor's shape — see the module docs
    /// on why this default must not be reused for capability grants.
    Mcps(Vec<String>),
    /// A capability grant, consulted per tool. Never passes through: no grant, no tools.
    Grant(CapabilitySet),
}

/// A runtime wrapper that limits the visible tool surface.
pub struct ScopedRuntime {
    /// Source of truth for [`Self::permits`] — also cloned into the decorator's
    /// catalog/invoke closures at construction.
    scope: Scope,
    /// Shared decorator implementing the catalog filter + pre-invoke gate.
    decorator: DecoratingRuntime,
}

fn refused(tool: &str) -> String {
    format!("tool '{tool}' is not in scope for this turn")
}

impl ScopedRuntime {
    /// Build a scoped runtime from an explicit MCP allow-list.
    ///
    /// When `allowed_mcps` is empty, every tool passes through with no filtering. For a capability
    /// grant use [`from_capabilities`](Self::from_capabilities) instead, which fails closed.
    pub fn new(inner: Arc<dyn ToolRuntime>, allowed_mcps: Vec<String>) -> Self {
        let decorator = DecoratingRuntime::with_filter_and_gate(
            inner.clone(),
            {
                let allowed = allowed_mcps.clone();
                move |tool| {
                    if allowed.is_empty() {
                        return true;
                    }
                    let mcp = mcp_of(&tool.name);
                    allowed.iter().any(|a| a == mcp)
                }
            },
            {
                let allowed = allowed_mcps.clone();
                move |call| {
                    if allowed.is_empty() {
                        return None;
                    }
                    let mcp = mcp_of(&call.name);
                    if allowed.iter().any(|a| a == mcp) {
                        None
                    } else {
                        Some(Err(refused(&call.name)))
                    }
                }
            },
        );
        Self {
            scope: Scope::Mcps(allowed_mcps),
            decorator,
        }
    }

    /// Build a scoped runtime enforcing `capabilities` tool by tool.
    ///
    /// Fails closed: an empty set yields an empty catalog. This is the constructor for a session's
    /// grant, and the only one that can express a partial grant over a single MCP.
    pub fn from_capabilities(inner: Arc<dyn ToolRuntime>, capabilities: CapabilitySet) -> Self {
        let decorator = DecoratingRuntime::with_filter_and_gate(
            inner.clone(),
            {
                let caps = capabilities.clone();
                move |tool| caps.grants_tool(&tool.name)
            },
            {
                let caps = capabilities.clone();
                move |call| {
                    if caps.grants_tool(&call.name) {
                        None
                    } else {
                        Some(Err(refused(&call.name)))
                    }
                }
            },
        );
        Self {
            scope: Scope::Grant(capabilities),
            decorator,
        }
    }

    /// Whether `tool` (a `"<mcp>:<tool>"` name) is in scope.
    pub fn permits(&self, tool: &str) -> bool {
        match &self.scope {
            Scope::Mcps(allowed) if allowed.is_empty() => true,
            Scope::Mcps(allowed) => {
                let mcp = mcp_of(tool);
                allowed.iter().any(|a| a == mcp)
            }
            Scope::Grant(caps) => caps.grants_tool(tool),
        }
    }
}

#[async_trait]
impl ToolRuntime for ScopedRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.decorator.catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.decorator.invoke(call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::Capability;
    use liberado_test_support::InvocationRecordingRuntime;

    fn mock_inner(
        tool_names: &[&str],
        result: Result<String, String>,
    ) -> InvocationRecordingRuntime {
        InvocationRecordingRuntime::default()
            .with_catalog(tool_names)
            .with_default_result(result)
    }

    #[tokio::test]
    async fn filters_catalog_to_allowed_mcps() {
        let inner = Arc::new(mock_inner(
            &[
                "tasks-mcp:add",
                "tasks-mcp:list",
                "email-mcp:send",
                "memory-mcp:recall",
            ],
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
        let inner = Arc::new(mock_inner(
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
        let inner = Arc::new(mock_inner(
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
        let inner = Arc::new(mock_inner(&["tasks-mcp:add"], Ok("added".into())));
        let scoped = ScopedRuntime::new(inner, vec!["tasks-mcp".into()]);

        let call = ToolInvocation::new("c1", "tasks-mcp:add", serde_json::json!({"title": "milk"}));
        let result = scoped.invoke(&call).await;
        assert_eq!(result, Ok("added".into()));
    }

    // ── from_capabilities: the fail-closed constructor ───────────────────────────────────────

    fn fleet() -> Arc<InvocationRecordingRuntime> {
        Arc::new(mock_inner(
            &[
                "turbovault:read_note",
                "turbovault:write_note",
                "turbovault:search_notes",
                "spider-mcp:fetch",
                "email-mcp:send",
            ],
            Ok("ok".into()),
        ))
    }

    /// The case per-tool grants exist for, and which no `Vec<String>` of MCP names can express.
    #[tokio::test]
    async fn a_partial_grant_shows_only_the_granted_tools_of_that_mcp() {
        let caps = CapabilitySet::from_iter([
            Capability::ExecuteMcp("spider-mcp".into()),
            Capability::ExecuteTool("turbovault:read_note".into()),
            Capability::ExecuteTool("turbovault:search_notes".into()),
        ]);
        let scoped = ScopedRuntime::from_capabilities(fleet(), caps);

        let names: Vec<String> = scoped.catalog().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec![
                "turbovault:read_note",
                "turbovault:search_notes",
                "spider-mcp:fetch"
            ],
            "the whole server for spider, two tools for turbovault, nothing for email"
        );

        let denied = ToolInvocation::new("c1", "turbovault:write_note", serde_json::json!({}));
        assert!(
            scoped.invoke(&denied).await.is_err(),
            "an ungranted tool on a partially granted MCP must be refused, not merely hidden"
        );
    }

    /// The regression that motivated a second constructor. `ScopedRuntime::new(inner, vec![])` means
    /// "no filtering" — correct for the tool-advisor, catastrophic for a grant. A grant that permits
    /// nothing must show nothing.
    #[tokio::test]
    async fn an_empty_grant_shows_nothing_rather_than_everything() {
        let scoped = ScopedRuntime::from_capabilities(fleet(), CapabilitySet::empty());
        assert!(
            scoped.catalog().is_empty(),
            "empty grant must fail closed; the MCP-list constructor's pass-through default must not \
             leak into this path"
        );
        let call = ToolInvocation::new("c1", "spider-mcp:fetch", serde_json::json!({}));
        assert!(scoped.invoke(&call).await.is_err());
    }

    /// Zone capabilities say nothing about which tools may be called, so they must not smuggle any in.
    #[tokio::test]
    async fn zone_capabilities_alone_grant_no_tools() {
        let caps = CapabilitySet::from_iter([
            Capability::Read(liberado_common::Zone::vault("Work")),
            Capability::Write(liberado_common::Zone::vault("Work")),
        ]);
        let scoped = ScopedRuntime::from_capabilities(fleet(), caps);
        assert!(scoped.catalog().is_empty());
    }

    /// A model can name a tool it was never shown — from a stale catalog or from nowhere at all.
    #[tokio::test]
    async fn a_tool_absent_from_the_catalog_is_still_refused_at_invoke() {
        let caps =
            CapabilitySet::from_iter([Capability::ExecuteTool("turbovault:read_note".into())]);
        let scoped = ScopedRuntime::from_capabilities(fleet(), caps);

        let hallucinated = ToolInvocation::new("c1", "email-mcp:send", serde_json::json!({}));
        let err = scoped.invoke(&hallucinated).await.unwrap_err();
        assert!(err.contains("not in scope"), "got: {err}");
    }
}
