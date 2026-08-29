//! Shared `ToolRuntime`/`RuntimeFactory` test doubles — consolidates what was previously duplicated
//! field-for-field across `liberado-orchestrator`'s own `#[cfg(test)]` module, its
//! `tests/orchestrate.rs` integration tests, and `liberado-daemon`'s tests. Test-only: never a
//! non-dev dependency of anything.
//!
//! Two patterns, kept as distinct types since they serve different assertions:
//! - [`CallRecordingFactory`] — a test cares what `runtime_for` was *called with* (scope,
//!   provenance), not what the runtime does once connected.
//! - [`InvocationRecordingFactory`]/[`InvocationRecordingRuntime`] — a test cares what tool
//!   invocations actually *reached* the runtime (e.g. proving a gated call never got there).
//!
//! Error simulation:
//! - [`InvocationRecordingRuntime`] accepts per-tool error overrides via `with_error` and a
//!   default result via `with_default_result`.
//! - [`FailingFactory`] always returns a [`RuntimeSetupError`] — for testing the orchestrator's
//!   pool-creation failure path.
//!
//! Trace contracts (backlog 0.5): [`trace_contracts`] reconstructs MVL turns and checks joins
//! against the execution-log companion. The path-based suite oracle is [`mvl_oracle`].
//! Production emission lives in `liberado-executor` (`MvlSession`); this crate only judges files.

pub mod mvl_oracle;
pub mod trace_contracts;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_common::WriteProvenance;
use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};
use liberado_provider::{ToolDef, ToolInvocation};

/// A configurable notifier for testing proposal-downgrade alert paths.
pub struct MockNotifier {
    pub ok: bool,
}

impl Default for MockNotifier {
    fn default() -> Self {
        Self { ok: true }
    }
}

#[async_trait]
impl liberado_notify::Notifier for MockNotifier {
    async fn notify(&self, _message: &str) -> Result<(), liberado_notify::NotifyError> {
        if self.ok {
            Ok(())
        } else {
            Err(liberado_notify::NotifyError(
                "mock notification failure".into(),
            ))
        }
    }
}

/// A runtime with an empty catalog that always succeeds with a fixed reply — for tests that don't
/// care what the runtime does, only that the orchestrator reaches it.
pub struct NoopRuntime;

#[async_trait]
impl ToolRuntime for NoopRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Ok("ok".to_string())
    }
}

/// One recorded `runtime_for` call: the `allowed_mcps` scope and the provenance it carried.
pub type RecordedRuntimeCall = (Vec<String>, WriteProvenance);

/// Records every `runtime_for` call's `(allowed_mcps, provenance)` so a test can assert what scope
/// the orchestrator derived from a decision. Always hands out a [`NoopRuntime`].
#[derive(Clone, Default)]
pub struct CallRecordingFactory {
    pub calls: Arc<Mutex<Vec<RecordedRuntimeCall>>>,
}

#[async_trait]
impl RuntimeFactory for CallRecordingFactory {
    async fn runtime_for(
        &self,
        allowed_mcps: &[String],
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        self.calls
            .lock()
            .unwrap()
            .push((allowed_mcps.to_vec(), provenance));
        Ok(Box::new(NoopRuntime))
    }
}

/// A runtime that records every `invoke` call — for tests asserting exactly which calls actually ran
/// (e.g. approved-proposal execution, or that a gated call never reached the real tool).
///
/// By default every invocation succeeds with `"ok"`. Call `with_default_result` to change
/// the default, or `with_error` to make a specific tool name fail.
#[derive(Clone)]
pub struct InvocationRecordingRuntime {
    pub invoked: Arc<Mutex<Vec<ToolInvocation>>>,
    default_result: Arc<Mutex<Result<String, String>>>,
    per_tool: Arc<Mutex<HashMap<String, Result<String, String>>>>,
}

impl Default for InvocationRecordingRuntime {
    fn default() -> Self {
        Self {
            invoked: Arc::default(),
            default_result: Arc::new(Mutex::new(Ok("ok".to_string()))),
            per_tool: Arc::default(),
        }
    }
}

impl InvocationRecordingRuntime {
    /// Set the result for any tool invocation that does not match a per-tool override.
    pub fn with_default_result(self, result: Result<String, String>) -> Self {
        *self.default_result.lock().unwrap() = result;
        self
    }

    /// Make a specific tool name return an error (or success). Overrides the default result.
    pub fn with_error(self, tool: impl Into<String>, err: impl Into<String>) -> Self {
        self.per_tool
            .lock()
            .unwrap()
            .insert(tool.into(), Err(err.into()));
        self
    }

    /// Make a specific tool name return a success result. Overrides the default result.
    pub fn with_result(self, tool: impl Into<String>, result: impl Into<String>) -> Self {
        self.per_tool
            .lock()
            .unwrap()
            .insert(tool.into(), Ok(result.into()));
        self
    }
}

#[async_trait]
impl ToolRuntime for InvocationRecordingRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.invoked.lock().unwrap().push(call.clone());
        let per_tool = self.per_tool.lock().unwrap();
        if let Some(result) = per_tool.get(&call.name) {
            return result.clone();
        }
        self.default_result.lock().unwrap().clone()
    }
}

/// Hands out clones of one [`InvocationRecordingRuntime`], ignoring the requested scope — tests that
/// need per-scope routing behavior should not use this factory.
#[derive(Clone)]
pub struct InvocationRecordingFactory {
    pub runtime: InvocationRecordingRuntime,
}

#[async_trait]
impl RuntimeFactory for InvocationRecordingFactory {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        Ok(Box::new(self.runtime.clone()))
    }
}

/// A [`RuntimeFactory`] that always fails with a [`RuntimeSetupError`] — for testing the
/// orchestrator's pool-creation failure path.
pub struct FailingFactory {
    error: String,
}

impl FailingFactory {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
        }
    }
}

#[async_trait]
impl RuntimeFactory for FailingFactory {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        Err(RuntimeSetupError(self.error.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_notify::Notifier;
    use serde_json::Value;
    #[test]
    fn noop_catalog_is_empty() {
        let rt = NoopRuntime;
        assert!(rt.catalog().is_empty());
    }

    #[tokio::test]
    async fn noop_invoke_returns_ok() {
        let rt = NoopRuntime;
        let call = ToolInvocation {
            id: "t1".into(),
            name: "test".into(),
            arguments: Value::Null,
        };
        let result = rt.invoke(&call).await;
        assert_eq!(result, Ok("ok".to_string()));
    }

    #[test]
    fn recording_catalog_is_empty() {
        let rt = InvocationRecordingRuntime::default();
        assert!(rt.catalog().is_empty());
    }

    #[tokio::test]
    async fn recording_invoke_stores_call_and_returns_ok() {
        let rt = InvocationRecordingRuntime::default();
        let call = ToolInvocation {
            id: "t2".into(),
            name: "test".into(),
            arguments: Value::Null,
        };
        let result = rt.invoke(&call).await;
        assert_eq!(result, Ok("ok".to_string()));
        let recorded = rt.invoked.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].name, "test");
    }

    #[tokio::test]
    async fn recording_invoke_with_per_tool_error() {
        let rt =
            InvocationRecordingRuntime::default().with_error("dangerous_tool", "permission denied");

        let call = ToolInvocation {
            id: "t3".into(),
            name: "dangerous_tool".into(),
            arguments: Value::Null,
        };
        let result = rt.invoke(&call).await;
        assert_eq!(result, Err("permission denied".to_string()));
    }

    #[tokio::test]
    async fn recording_invoke_with_default_error() {
        let rt = InvocationRecordingRuntime::default()
            .with_default_result(Err("transport down".to_string()));

        let call = ToolInvocation {
            id: "t4".into(),
            name: "any_tool".into(),
            arguments: Value::Null,
        };
        let result = rt.invoke(&call).await;
        assert_eq!(result, Err("transport down".to_string()));
    }

    #[tokio::test]
    async fn recording_invoke_per_tool_override_wins_over_default() {
        let rt = InvocationRecordingRuntime::default()
            .with_default_result(Err("transport down".to_string()))
            .with_result("special_tool", "special ok");

        let good = rt
            .invoke(&ToolInvocation {
                id: "g1".into(),
                name: "special_tool".into(),
                arguments: Value::Null,
            })
            .await;
        assert_eq!(good, Ok("special ok".to_string()));

        let bad = rt
            .invoke(&ToolInvocation {
                id: "b1".into(),
                name: "other_tool".into(),
                arguments: Value::Null,
            })
            .await;
        assert_eq!(bad, Err("transport down".to_string()));
    }

    #[tokio::test]
    async fn failing_factory_returns_runtime_setup_error() {
        let factory = FailingFactory::new("MCP launch failed");
        let result = factory.runtime_for(&[], WriteProvenance::human()).await;
        match result {
            Err(e) => assert!(e.0.contains("MCP launch failed")),
            Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn mock_notifier_with_ok_false_returns_error() {
        // The `notify` impl's `if self.ok { Ok(()) } else { Err(...) }` branch was a survivor:
        // cargo-mutants replaced the body with `Ok(())` and every existing test still passed
        // because the default `ok=true` path returns Ok. Asserting the `ok=false` path makes
        // the function's branch visible to the test suite.
        let notifier = MockNotifier { ok: false };
        let result = notifier.notify("anything").await;
        assert!(result.is_err(), "ok=false must yield an error");
    }
}
