//! Shared test fixtures and doubles for `liberado-daemon` integration tests.

use super::super::*;
use liberado_common::WriteProvenance;
use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};
use liberado_provider::{ToolDef, ToolInvocation};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

pub(crate) async fn temp_daemon() -> (Daemon, TempDir) {
    let dir = TempDir::new().unwrap();
    // F12 scopes the watcher to `inbox/`. Create it before any test starts the
    // watch: Linux inotify does not reliably deliver events for a directory
    // created after the watch is armed. Windows CI still passed without this.
    std::fs::create_dir_all(dir.path().join("inbox")).unwrap();
    let daemon = Daemon::open("test", dir.path())
        .await
        .unwrap()
        // Approval authority lives outside the vault, so a fixture that executes proposals needs
        // one attached — rooted inside the same temp dir so it dies with the test. A test that
        // *approves* something calls `approve_in` for the matching decision; one that does not is
        // asserting the refusal, which is the default.
        .with_approval_ledger(test_ledger(&dir));
    (daemon, dir)
}

/// The ledger `temp_daemon` attaches, addressable from a test that needs to record a decision.
pub(crate) fn test_ledger(dir: &TempDir) -> liberado_common::ApprovalLedger {
    liberado_common::ApprovalLedger::new(dir.path().join(".approvals"))
}

/// Record the human approval a proposal needs before the daemon will run it — the ledger entry a
/// Telegram tap would create. Without this the note's `status: approved` is only a claim.
pub(crate) async fn approve_in(dir: &TempDir, proposal_id: &str) {
    test_ledger(dir)
        .record(
            proposal_id,
            liberado_common::ApprovalDecision::Approved,
            "test",
        )
        .await
        .unwrap();
}

pub(crate) struct NoopRuntime;

#[async_trait::async_trait]
impl ToolRuntime for NoopRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Ok("ok".into())
    }
}

pub(crate) struct NoopFactory;

#[async_trait::async_trait]
impl RuntimeFactory for NoopFactory {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        Ok(Box::new(NoopRuntime))
    }
}

/// Never actually builds a runtime (the Clarify path stops before execution) — exists only to
/// satisfy `Orchestrator::new`'s type.
pub(crate) struct NoopFactoryForClarify;

#[async_trait::async_trait]
impl RuntimeFactory for NoopFactoryForClarify {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        unreachable!("a Clarify never reaches execution")
    }
}

pub(crate) struct UnusedRuntime;

#[async_trait::async_trait]
impl ToolRuntime for UnusedRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Ok("ok".into())
    }
}

pub(crate) struct UnusedFactory;

#[async_trait::async_trait]
impl RuntimeFactory for UnusedFactory {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        Ok(Box::new(UnusedRuntime))
    }
}

#[derive(Default)]
pub(crate) struct RecordingNotifier {
    pub calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl liberado_notify::Notifier for RecordingNotifier {
    async fn notify(&self, _message: &str) -> Result<(), liberado_notify::NotifyError> {
        Ok(())
    }
    async fn deliver_cron(&self, message: &str) -> Result<(), liberado_notify::NotifyError> {
        self.calls.lock().unwrap().push(message.to_string());
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct SpyNotifier {
    pub calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl liberado_notify::Notifier for SpyNotifier {
    async fn notify(&self, message: &str) -> Result<(), liberado_notify::NotifyError> {
        self.calls.lock().unwrap().push(message.to_string());
        Ok(())
    }
}
