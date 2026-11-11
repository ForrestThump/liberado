//! Split from `lib.rs` for module-health boundaries.

//! Exercises the [`run`] preamble: config load, vault resolution, and control-plane refusal
//! before any daemon assembly or HTTP serve.

use super::boot_helper_tests::ENV_LOCK;

struct EnvRestore {
    config: Option<std::ffi::OsString>,
    bind: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn snapshot() -> Self {
        Self {
            config: std::env::var_os("LIBERADO_CONFIG_DIR"),
            bind: std::env::var_os("LIBERADO_BIND"),
        }
    }

    fn restore(self) {
        // SAFETY(test): caller holds `ENV_LOCK`.
        unsafe {
            match self.config {
                Some(v) => std::env::set_var("LIBERADO_CONFIG_DIR", v),
                None => std::env::remove_var("LIBERADO_CONFIG_DIR"),
            }
            match self.bind {
                Some(v) => std::env::set_var("LIBERADO_BIND", v),
                None => std::env::remove_var("LIBERADO_BIND"),
            }
        }
    }
}

#[tokio::test]
async fn run_refuses_public_bind_before_assembly() {
    let _g = ENV_LOCK.lock().await;
    let saved = EnvRestore::snapshot();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("topology.toml"),
        r#"
vault_path = "/tmp/liberado-test-vault"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("policy.toml"),
        r#"
[[zones]]
zone = "tasks"
write_class = "agent_writable"
"#,
    )
    .unwrap();
    // SAFETY(test): serialized on `ENV_LOCK`.
    unsafe {
        std::env::set_var("LIBERADO_CONFIG_DIR", dir.path());
        std::env::set_var("LIBERADO_BIND", "0.0.0.0:4201");
    }

    let err = crate::run(String::new())
        .await
        .expect_err("must refuse an open bind without API credentials");
    saved.restore();

    let message = err.to_string();
    assert!(
        message.contains("credential") || message.contains("bind") || message.contains("auth"),
        "expected control-plane refusal, got: {message}"
    );
}
