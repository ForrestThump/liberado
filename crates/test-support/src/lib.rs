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

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_common::WriteProvenance;
use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};
use liberado_provider::{ToolDef, ToolInvocation};

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
#[derive(Clone, Default)]
pub struct InvocationRecordingRuntime {
    pub invoked: Arc<Mutex<Vec<ToolInvocation>>>,
}

#[async_trait]
impl ToolRuntime for InvocationRecordingRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.invoked.lock().unwrap().push(call.clone());
        Ok("ok".to_string())
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
