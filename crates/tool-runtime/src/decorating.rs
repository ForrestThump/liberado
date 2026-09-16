//! # DecoratingRuntime
//!
//! A shared [`ToolRuntime`] wrapper used by every "wrap another runtime and add a small
//! constraint around `invoke`" pattern that does not need its own bespoke struct.
//!
//! Two optional layers, both opt-in:
//!
//! * **Catalog filter** — when set, [`ToolRuntime::catalog`] returns only the inner tools for
//!   which the predicate returns true. This is the shape that used to be hand-rolled inside
//!   every scoped/filtered wrapper (see `mcp::ScopedRuntime`).
//! * **Pre-invoke gate** — when set, [`ToolRuntime::invoke`] consults the closure before
//!   delegating; if it returns `Some(result)`, that result short-circuits the call and the
//!   inner runtime is never reached. `None` falls through.
//!
//! `is_read_only` and `parks_for_human` forward to the inner runtime. Previously the
//! hand-rolled wrappers (e.g. `PassThroughRuntime`, `ScopedRuntime`) silently relied on the
//! trait's `false` default for both — forwarding to inner is the intended passthrough
//! semantics and is a strict improvement, never a behavior regression for callers that
//! already depended on the inner.
//!
//! Complex wrappers that need more than one hook (per-call routing by tool name, transport
//! health reporting, provenance rebinding, post-invoke observation, structured refusal with
//! a face) stay bespoke. This decorator is the cheap, shared kernel of "passthrough with one
//! tiny extra rule", not a replacement for every `impl ToolRuntime`.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_provider::{ToolDef, ToolInvocation};

use crate::ToolRuntime;

type CatalogFilter = Arc<dyn Fn(&ToolDef) -> bool + Send + Sync>;
type PreInvokeGate = Arc<dyn Fn(&ToolInvocation) -> Option<Result<String, String>> + Send + Sync>;

/// A [`ToolRuntime`] built by stacking zero or one catalog filter and zero or one pre-invoke
/// gate on top of an inner runtime. See the [module docs](self).
pub struct DecoratingRuntime {
    inner: Arc<dyn ToolRuntime>,
    /// If set, `catalog()` returns only tools for which this returns true.
    catalog_filter: Option<CatalogFilter>,
    /// If the closure returns `Some(result)`, that short-circuits; `None` means call inner.
    pre_invoke: Option<PreInvokeGate>,
}

impl DecoratingRuntime {
    /// Pure delegation in both directions — used when an `Arc<dyn ToolRuntime>` must be
    /// erased to a `Box<dyn ToolRuntime>` (or vice-versa) with no additional behavior.
    pub fn passthrough(inner: Arc<dyn ToolRuntime>) -> Self {
        Self {
            inner,
            catalog_filter: None,
            pre_invoke: None,
        }
    }

    /// Wrap `inner` and narrow its catalog to the tools for which `filter` returns true.
    pub fn with_catalog_filter(
        inner: Arc<dyn ToolRuntime>,
        filter: impl Fn(&ToolDef) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            catalog_filter: Some(Arc::new(filter)),
            pre_invoke: None,
        }
    }

    /// Wrap `inner` with both a catalog filter and a pre-invoke gate. The
    /// `ScopedRuntime` shape — "show only these tools, refuse
    /// anything outside that set on invoke too" — composes from a single helper here.
    pub fn with_filter_and_gate(
        inner: Arc<dyn ToolRuntime>,
        filter: impl Fn(&ToolDef) -> bool + Send + Sync + 'static,
        gate: impl Fn(&ToolInvocation) -> Option<Result<String, String>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            catalog_filter: Some(Arc::new(filter)),
            pre_invoke: Some(Arc::new(gate)),
        }
    }
}

#[async_trait]
impl ToolRuntime for DecoratingRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        let tools = self.inner.catalog();
        match &self.catalog_filter {
            Some(f) => tools.into_iter().filter(|t| f(t)).collect(),
            None => tools,
        }
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if let Some(gate) = &self.pre_invoke
            && let Some(early) = gate(call)
        {
            return early;
        }
        self.inner.invoke(call).await
    }

    fn is_read_only(&self, tool_name: &str) -> bool {
        self.inner.is_read_only(tool_name)
    }

    fn parks_for_human(&self, tool_name: &str) -> bool {
        self.inner.parks_for_human(tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use async_trait::async_trait;

    /// Minimal in-crate runtime double — a foundation crate must not depend on
    /// `liberado-test-support`, and we want the decorator's tests to live where the
    /// decorator is, not in a downstream consumer's suite.
    #[derive(Clone)]
    struct MockRuntime {
        catalog: Vec<ToolDef>,
        default_result: Result<String, String>,
        per_tool: Arc<Mutex<std::collections::HashMap<String, Result<String, String>>>>,
        invoked: Arc<Mutex<Vec<ToolInvocation>>>,
        read_only: Arc<Mutex<std::collections::HashMap<String, bool>>>,
        parks: Arc<Mutex<std::collections::HashMap<String, bool>>>,
    }

    impl MockRuntime {
        fn new(tool_names: &[&str], default_result: Result<&str, &str>) -> Self {
            let default_result = match default_result {
                Ok(s) => Ok(s.to_string()),
                Err(s) => Err(s.to_string()),
            };
            Self {
                catalog: tool_names
                    .iter()
                    .map(|n| ToolDef::new(*n, "test", serde_json::json!({})))
                    .collect(),
                default_result,
                per_tool: Arc::default(),
                invoked: Arc::default(),
                read_only: Arc::default(),
                parks: Arc::default(),
            }
        }

        fn with_tool_result(self, tool: &str, result: Result<&str, &str>) -> Self {
            let r = match result {
                Ok(s) => Ok(s.to_string()),
                Err(s) => Err(s.to_string()),
            };
            self.per_tool.lock().unwrap().insert(tool.into(), r);
            self
        }

        fn with_read_only(self, tool: &str, value: bool) -> Self {
            self.read_only.lock().unwrap().insert(tool.into(), value);
            self
        }

        fn with_parks(self, tool: &str, value: bool) -> Self {
            self.parks.lock().unwrap().insert(tool.into(), value);
            self
        }
    }

    #[async_trait]
    impl ToolRuntime for MockRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            self.catalog.clone()
        }

        async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
            self.invoked.lock().unwrap().push(call.clone());
            if let Some(r) = self.per_tool.lock().unwrap().get(&call.name).cloned() {
                return r;
            }
            self.default_result.clone()
        }

        fn is_read_only(&self, tool_name: &str) -> bool {
            self.read_only
                .lock()
                .unwrap()
                .get(tool_name)
                .copied()
                .unwrap_or(false)
        }

        fn parks_for_human(&self, tool_name: &str) -> bool {
            self.parks
                .lock()
                .unwrap()
                .get(tool_name)
                .copied()
                .unwrap_or(false)
        }
    }

    fn call(name: &str) -> ToolInvocation {
        ToolInvocation::new("id", name, serde_json::json!({}))
    }

    #[tokio::test]
    async fn passthrough_forwards_catalog_and_invoke() {
        let inner = Arc::new(MockRuntime::new(&["alpha", "beta"], Ok("ok")));
        let decorator = DecoratingRuntime::passthrough(inner);

        let names: Vec<String> = decorator.catalog().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["alpha", "beta"]);

        let result = decorator.invoke(&call("alpha")).await;
        assert_eq!(result, Ok("ok".into()));
    }

    #[tokio::test]
    async fn catalog_filter_narrows_visible_tools_without_touching_invoke() {
        let inner = Arc::new(MockRuntime::new(&["alpha", "beta", "gamma"], Ok("ok")));
        let decorator = DecoratingRuntime::with_catalog_filter(inner, |t| t.name.starts_with('a'));

        let names: Vec<String> = decorator.catalog().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["alpha"]);

        // The filter only narrows what is offered. The inner is still reachable for any
        // call the model manages to name — the gate closure is what enforces it at invoke.
        let result = decorator.invoke(&call("beta")).await;
        assert_eq!(result, Ok("ok".into()));
    }

    #[tokio::test]
    async fn pre_invoke_gate_short_circuits_without_calling_inner() {
        // Make `beta` error loudly if the gate ever lets it through — proves the inner
        // was bypassed rather than that its output happened to match.
        let inner = Arc::new(
            MockRuntime::new(&["alpha", "beta"], Ok("ok"))
                .with_tool_result("beta", Err("should not run")),
        );
        let decorator = DecoratingRuntime::with_filter_and_gate(
            inner.clone(),
            |_| true,
            |c| {
                if c.name == "beta" {
                    Some(Err("beta refused".into()))
                } else {
                    None
                }
            },
        );

        assert_eq!(decorator.invoke(&call("alpha")).await, Ok("ok".into()));
        assert_eq!(
            decorator.invoke(&call("beta")).await,
            Err("beta refused".into()),
            "beta is short-circuited by the gate; the inner is never asked"
        );
        assert_eq!(
            inner.invoked.lock().unwrap().len(),
            1,
            "only alpha reached the inner runtime"
        );
    }

    #[test]
    fn is_read_only_and_parks_for_human_forward_to_inner() {
        let inner = Arc::new(
            MockRuntime::new(&["alpha"], Ok("ok"))
                .with_read_only("alpha", true)
                .with_parks("alpha", true),
        );
        let decorator = DecoratingRuntime::passthrough(inner);
        assert!(decorator.is_read_only("alpha"));
        assert!(decorator.parks_for_human("alpha"));
        assert!(!decorator.is_read_only("beta"));
        assert!(!decorator.parks_for_human("beta"));
    }
}
