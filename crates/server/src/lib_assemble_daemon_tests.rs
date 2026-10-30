//! Exercises [`super::assemble_daemon`] far enough to wire the reminder outbox worker.

use super::boot_helper_tests::ENV_LOCK;
use super::*;
use std::sync::Arc;

fn config_with_vault(path: &str) -> liberado_bootstrap::Config {
    let mut config = liberado_bootstrap::Config::default();
    config.topology.vault_path = std::path::PathBuf::from(path);
    config
}

struct DataDirRestore(Option<std::ffi::OsString>);

impl DataDirRestore {
    fn snapshot() -> Self {
        Self(std::env::var_os("LIBERADO_DATA_DIR"))
    }

    fn restore(self) {
        // SAFETY(test): caller holds `ENV_LOCK`.
        unsafe {
            match self.0 {
                Some(v) => std::env::set_var("LIBERADO_DATA_DIR", v),
                None => std::env::remove_var("LIBERADO_DATA_DIR"),
            }
        }
    }
}

#[tokio::test]
async fn assemble_daemon_attaches_outbox_and_delivery() {
    let _g = ENV_LOCK.lock().await;
    let saved = DataDirRestore::snapshot();
    let data = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    // SAFETY(test): serialized on `ENV_LOCK`.
    unsafe {
        std::env::set_var("LIBERADO_DATA_DIR", data.path());
    }

    let config = config_with_vault(vault.path().to_str().unwrap());
    let coder_tuning = liberado_coder_core::CoderTuning::default();
    let guard = Arc::new(auth::ApiGuard::new(auth::ApiAuth::default(), Vec::new()));

    let (_daemon_handle, _router, state) = assemble_daemon(
        config,
        coder_tuning,
        vault.path().display().to_string(),
        guard,
    )
    .await
    .expect("assemble");

    assert!(
        state.outbox.is_some() && state.outbox_delivery.is_some(),
        "bootstrap should open the outbox and delivery sink"
    );

    saved.restore();
}
