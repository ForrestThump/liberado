//! Exercises [`super::assemble_daemon`] far enough to wire the reminder outbox worker.

use super::boot_helper_tests::ENV_LOCK;
use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;

const FAKE_PROVIDER_KEY_ENV: &str = "LIBERADO_TEST_ASSEMBLE_PROVIDER_KEY";

fn config_with_vault(path: &str) -> liberado_bootstrap::Config {
    let mut config = liberado_bootstrap::Config::default();
    config.topology.vault_path = std::path::PathBuf::from(path);
    config
}

fn config_with_fake_provider(path: &str) -> liberado_bootstrap::Config {
    use liberado_config::ProviderProfile;

    let mut config = config_with_vault(path);
    config.topology.provider = "test".into();
    config.topology.providers = vec![ProviderProfile {
        name: "test".into(),
        base_url: "https://test.invalid".into(),
        default_model: "test-model".into(),
        api_key_env: FAKE_PROVIDER_KEY_ENV.into(),
        model_env: None,
        extra_client_error_status: Vec::new(),
        fallback: None,
    }];
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

/// Snapshots every `*_API_KEY*` (and `*API_KEY*`) env var so tests do not depend on the host.
struct ProviderKeyRestore(BTreeMap<String, Option<std::ffi::OsString>>);

impl ProviderKeyRestore {
    fn snapshot_and_clear() -> Self {
        let mut saved = BTreeMap::new();
        for (key, value) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.to_uppercase().contains("API_KEY") {
                saved.insert(name.into_owned(), Some(value));
            }
        }
        // SAFETY(test): caller holds `ENV_LOCK`.
        unsafe {
            for key in saved.keys() {
                std::env::remove_var(key);
            }
        }
        Self(saved)
    }

    fn restore(self) {
        // SAFETY(test): caller holds `ENV_LOCK`.
        unsafe {
            for (key, value) in self.0 {
                match value {
                    Some(v) => std::env::set_var(&key, v),
                    None => std::env::remove_var(&key),
                }
            }
        }
    }
}

struct FakeKeyRestore(Option<std::ffi::OsString>);

impl FakeKeyRestore {
    fn set() -> Self {
        let saved = std::env::var_os(FAKE_PROVIDER_KEY_ENV);
        // SAFETY(test): caller holds `ENV_LOCK`.
        unsafe {
            std::env::set_var(FAKE_PROVIDER_KEY_ENV, "assemble-test-fake-key");
        }
        Self(saved)
    }

    fn restore(self) {
        // SAFETY(test): caller holds `ENV_LOCK`.
        unsafe {
            match self.0 {
                Some(v) => std::env::set_var(FAKE_PROVIDER_KEY_ENV, v),
                None => std::env::remove_var(FAKE_PROVIDER_KEY_ENV),
            }
        }
    }
}

#[tokio::test]
async fn assemble_daemon_wires_outbox_in_watch_only_without_provider_keys() {
    let _g = ENV_LOCK.lock().await;
    let saved_data = DataDirRestore::snapshot();
    let saved_keys = ProviderKeyRestore::snapshot_and_clear();
    let data = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    // SAFETY(test): serialized on `ENV_LOCK`.
    unsafe {
        std::env::set_var("LIBERADO_DATA_DIR", data.path());
    }

    let config = config_with_vault(vault.path().to_str().unwrap());
    let (_daemon_handle, _router, state) = {
        let guard = Arc::new(auth::ApiGuard::new(auth::ApiAuth::default(), Vec::new()));
        let boot = DaemonBoot {
            config,
            #[cfg(feature = "coding")]
            coder_tuning: liberado_coder_core::CoderTuning::default(),
        };
        assemble_daemon(boot, vault.path().display().to_string(), guard)
            .await
            .expect("assemble")
    };

    assert!(
        state.outbox.is_some() && state.outbox_delivery.is_some(),
        "watch-only assembly must still open the reminder outbox"
    );
    assert!(
        !state.dispatcher_attached,
        "without a provider key the daemon stays watch-only"
    );

    saved_keys.restore();
    saved_data.restore();
}

#[tokio::test]
async fn assemble_daemon_wires_outbox_when_provider_key_is_set() {
    let _g = ENV_LOCK.lock().await;
    let saved_data = DataDirRestore::snapshot();
    let saved_keys = ProviderKeyRestore::snapshot_and_clear();
    let fake_key = FakeKeyRestore::set();
    let data = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    // SAFETY(test): serialized on `ENV_LOCK`.
    unsafe {
        std::env::set_var("LIBERADO_DATA_DIR", data.path());
    }

    let config = config_with_fake_provider(vault.path().to_str().unwrap());
    let (_daemon_handle, _router, state) = {
        let guard = Arc::new(auth::ApiGuard::new(auth::ApiAuth::default(), Vec::new()));
        let boot = DaemonBoot {
            config,
            #[cfg(feature = "coding")]
            coder_tuning: liberado_coder_core::CoderTuning::default(),
        };
        assemble_daemon(boot, vault.path().display().to_string(), guard)
            .await
            .expect("assemble")
    };

    assert!(
        state.outbox.is_some() && state.outbox_delivery.is_some(),
        "full assembly must open the reminder outbox"
    );
    assert!(
        state.dispatcher_attached,
        "fake provider key enables dispatch"
    );

    fake_key.restore();
    saved_keys.restore();
    saved_data.restore();
}
