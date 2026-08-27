//! Survivor tests split from `lib.rs`.
//!
//! Several survivors here are env-var-driven path resolvers; every env touch
//! goes through [`ENV_LOCK`] + [`EnvGuard`] so parallel tests cannot observe
//! each other's variables.

use super::*;
use std::sync::{Mutex, OnceLock};

pub(crate) fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Set one variable for the test, restore the prior value on drop.
pub(crate) struct EnvGuard {
    var: &'static str,
    prior: Option<std::ffi::OsString>,
}

impl EnvGuard {
    pub(crate) fn set(var: &'static str, value: &Path) -> Self {
        let prior = std::env::var_os(var);
        // SAFETY: callers hold ENV_LOCK, so no concurrent test reads these.
        unsafe { std::env::set_var(var, value) };
        EnvGuard { var, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: same lock discipline as `set`.
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(self.var, v),
                None => std::env::remove_var(self.var),
            }
        }
    }
}

const DATA_DIR_ENV: &str = "LIBERADO_DATA_DIR";
const CONFIG_DIR_ENV_NAME: &str = CONFIG_DIR_ENV;
const MCP_INSTALL_ENV: &str = "LIBERADO_MCP_INSTALL_DIR";

// ── path resolvers ───────────────────────────────────────────────────────────

/// Tier 1 of the resolution order: an explicit `LIBERADO_CONFIG_DIR` always
/// wins. Pins the wrapper against body deletion.
#[test]
fn config_dir_env_tier_wins() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set(CONFIG_DIR_ENV_NAME, dir.path());
    assert_eq!(config_dir(), Some(dir.path().to_path_buf()));
}

/// The overlay file is `<data_dir>/grants.overlay.toml`, by exactly that name.
#[test]
fn grants_overlay_path_lands_in_the_data_dir() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set(DATA_DIR_ENV, dir.path());
    let expected = dir.path().join("grants.overlay.toml");
    assert_eq!(grants_overlay_path(), expected);
}

/// `LIBERADO_MCP_INSTALL_DIR` beats the platform default.
#[test]
fn mcp_install_dir_env_wins() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set(MCP_INSTALL_ENV, dir.path());
    assert_eq!(mcp_install_dir(), dir.path().to_path_buf());
}

/// `LIBERADO_DATA_DIR` wins over the `.liberado` cwd default.
#[test]
fn data_dir_env_wins() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set(DATA_DIR_ENV, dir.path());
    assert_eq!(data_dir(), dir.path().to_path_buf());
}

// ── has_any_config_file ─────────────────────────────────────────────────────

/// Any ONE of the three section files counts — `&&` would demand all three
/// and miss every real partial config directory.
#[test]
fn any_single_section_file_counts_as_a_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        !has_any_config_file(dir.path()),
        "empty dir is not a config dir"
    );
    std::fs::write(dir.path().join(TOPOLOGY_FILE), "vault_path = \"v\"\n").unwrap();
    assert!(has_any_config_file(dir.path()), "topology alone counts");

    let dir2 = tempfile::tempdir().unwrap();
    std::fs::write(dir2.path().join(POLICY_FILE), "").unwrap();
    assert!(has_any_config_file(dir2.path()), "policy alone counts");

    let dir3 = tempfile::tempdir().unwrap();
    std::fs::write(dir3.path().join(TUNING_FILE), "").unwrap();
    assert!(has_any_config_file(dir3.path()), "tuning alone counts");
}

// ── grants overlay ───────────────────────────────────────────────────────────

/// A valid overlay written through the append core must come back through
/// the read wrapper with its grant intact.
#[test]
fn overlay_round_trips_through_the_wrapper() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set(DATA_DIR_ENV, dir.path());

    let cap = Capability::AskHuman;
    let appended = append_grant_to_overlay("telegram", &cap).expect("fresh overlay write succeeds");
    assert!(appended, "a fresh grant is a change");

    let overlay = load_grants_overlay();
    assert_eq!(overlay.grants.len(), 1, "{overlay:?}");
    assert_eq!(overlay.grants[0].component, "telegram");
    assert!(overlay.grants[0].capabilities.contains(&cap));
}

/// Appending is idempotent, and two capabilities naming the SAME zone must
/// not duplicate that zone declaration in the overlay file.
#[test]
fn append_is_idempotent_and_does_not_duplicate_zones() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set(DATA_DIR_ENV, dir.path());

    let cap_a = Capability::Read(Zone::Named("shared".to_string()));
    let cap_b = Capability::ReadSummary(Zone::Named("shared".to_string()));
    assert!(
        append_grant_to_overlay("comp", &cap_a).unwrap(),
        "first append changes the file"
    );
    assert!(
        !append_grant_to_overlay("comp", &cap_a).unwrap(),
        "the identical append reports no change"
    );
    assert!(
        append_grant_to_overlay("comp", &cap_b).unwrap(),
        "a different capability is a change"
    );

    let overlay = load_grants_overlay();
    let zone_entries = overlay.zones.iter().filter(|z| z.zone == "shared").count();
    assert_eq!(zone_entries, 1, "one zone declared once: {overlay:?}");
}

/// The machine-owned overlay merges over the base policy at load time: a
/// zone-only-plus-grant overlay must reach the returned config instead of
/// being skipped as "empty".
#[test]
fn load_config_merges_a_nonempty_overlay() {
    let _guard = env_lock().lock().unwrap();
    let data = tempfile::tempdir().unwrap();
    let cfgdir = tempfile::tempdir().unwrap();
    let _data_env = EnvGuard::set(DATA_DIR_ENV, data.path());

    std::fs::write(
        cfgdir.path().join(TOPOLOGY_FILE),
        "vault_path = \"/tmp/survivor-vault\"\n",
    )
    .unwrap();

    // Generate a consistent overlay via the append core (it declares the
    // zone the capability names), then reload through load_config.
    let overlay_path = data.path().join(GRANTS_OVERLAY_FILE);
    let appended =
        append_grant_to_overlay_at(&overlay_path, "telegram-approval", &Capability::AskHuman)
            .expect("overlay write succeeds");
    assert!(appended, "precondition: overlay was written");

    let (config, _) = load_config(Some(cfgdir.path())).expect("valid base + soft overlay");
    let telegram = config
        .policy
        .grants
        .iter()
        .find(|g| g.component == "telegram-approval")
        .unwrap_or_else(|| panic!("overlay grant must reach the merged config"));
    assert!(telegram.capabilities.contains(&Capability::AskHuman));
}

// ── catalog ──────────────────────────────────────────────────────────────────

const TOPOLOGY_TOML: &str = r#"
[topology]
vault_path = "/home/test/vault"

[[topology.mcps]]
name = "memory-mcp"
description = "store and recall memories"
consequence = "reversible"
transport = { kind = "stdio", command = "memory-mcp", args = [] }
writes_vault = false

[[topology.mcps]]
name = "off-mcp"
description = "disabled entry"
consequence = "reversible"
enabled = false
transport = { kind = "stdio", command = "off-mcp", args = [] }
writes_vault = false
"#;

/// Only ENABLED mcps reach the live catalog.
#[test]
fn capability_catalog_lists_only_enabled_mcps() {
    let cfg: Config = toml::from_str(TOPOLOGY_TOML).expect("fixture parses");
    let catalog = capability_catalog_from_config(&cfg);
    let names: Vec<String> = catalog
        .descriptors()
        .iter()
        .map(|d| d.name.clone())
        .collect();
    assert_eq!(names, vec!["memory-mcp".to_string()], "{names:?}");
}

// ── proposal signing key ─────────────────────────────────────────────────────

/// First use generates and persists 32 bytes; later loads return exactly
/// those bytes from the file (never regenerated, never zeroed).
#[test]
fn proposal_key_is_persisted_and_stable_across_calls() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set(DATA_DIR_ENV, dir.path());

    let first = load_or_create_proposal_key();
    assert_eq!(first.len(), PROPOSAL_KEY_LEN, "a full-length key");

    let key_file = dir.path().join(PROPOSAL_KEY_FILE);
    let persisted = std::fs::read(&key_file).expect("the key is persisted to disk");
    assert_eq!(persisted, first, "the file holds the very key in use");

    let second = load_or_create_proposal_key();
    assert_eq!(second, first, "a stored key is reused verbatim");
}

/// An existing well-formed key file is loaded verbatim - not regenerated.
#[test]
fn an_existing_key_file_is_loaded_verbatim() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set(DATA_DIR_ENV, dir.path());

    let known: Vec<u8> = (0..PROPOSAL_KEY_LEN).map(|i| i as u8).collect();
    std::fs::write(dir.path().join(PROPOSAL_KEY_FILE), &known).unwrap();

    assert_eq!(load_or_create_proposal_key(), known);
}

// ── read-error honesty in load_section ───────────────────────────────────────

/// A present-but-unreadable section file must be a hard `Read` error, not
/// silently absorbed as "absent defaults". The NotFound guard exists for
/// absence; letting it swallow permission errors would boot a daemon on
/// defaults while its hand-written policy sat unread next to it.
#[test]
fn an_unreadable_section_file_is_an_error_not_defaults() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(TOPOLOGY_FILE), "vault_path = \"/tmp/v\"\n").unwrap();
    let policy = dir.path().join(POLICY_FILE);
    std::fs::write(&policy, "zones = []\n").unwrap();

    // Deny reads platform-specifically: mode bits on Unix, an ACL deny ACE
    // via icacls on Windows (*S-1-1-0 = locale-independent Everyone).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    #[cfg(windows)]
    {
        let out = liberado_common::process::std_command("icacls")
            .arg(&policy)
            .args(["/deny", "*S-1-1-0:(RD)"])
            .output()
            .expect("icacls runs");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Restore before asserts so tempdir cleanup never fights the ACL.
    let restore = || {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        #[cfg(windows)]
        {
            let _ = liberado_common::process::std_command("icacls")
                .arg(&policy)
                .args(["/remove:d", "*S-1-1-0"])
                .output();
        }
    };

    let outcome = load_config(Some(dir.path()));
    restore();

    let err = outcome.expect_err("an unreadable policy.toml is a Read error");
    let rendered = err.to_string();
    assert!(rendered.contains(POLICY_FILE), "{rendered}");
}

// ── log-only NotFound / schema_version survivors ─────────────────────────────
// Same shape as coder-core's prompts_guard_survivor_tests: both match arms return
// Policy::default(); only the WARN distinguishes missing vs unreadable. The
// schema_version `!=` is likewise only observable via the warn event.

use std::sync::{Arc, Mutex as StdMutex};

#[derive(Default, Clone)]
struct Captured(Arc<StdMutex<Vec<(tracing::Level, String)>>>);

impl<S: tracing::Subscriber> tracing_subscriber::layer::Layer<S> for Captured {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Msg(Vec<String>);
        impl tracing::field::Visit for Msg {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                self.0.push(format!("{}={v:?}", f.name()));
            }
        }
        let mut m = Msg(Vec::new());
        event.record(&mut m);
        self.0
            .lock()
            .unwrap()
            .push((*event.metadata().level(), m.0.join(" ")));
    }
}

/// Missing overlay stays quiet; an unreadable overlay must WARN.
///
/// Kills: match-guard `==`→`!=`, guard→false (missing would warn), and on unix
/// guard→true (unreadable would stay silent).
#[test]
fn missing_grants_overlay_is_quiet_while_unreadable_is_loud() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let captured = Captured::default();
    let sub = tracing_subscriber::registry().with(captured.clone());
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist-grants.overlay.toml");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = dir.path().join("locked.overlay.toml");
        std::fs::write(&locked, "grants = []\n").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        tracing::subscriber::with_default(sub, || {
            let _ = load_grants_overlay_at(&missing);
            let _ = load_grants_overlay_at(&locked);
        });

        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644));

        let seen = captured.0.lock().unwrap();
        assert!(
            seen.iter()
                .any(|(l, m)| *l == tracing::Level::WARN && m.contains("could not be read")),
            "unreadable overlay must warn: {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|(_, m)| m.contains("does-not-exist-grants.overlay.toml")),
            "missing overlay stays silent: {seen:?}"
        );
    }

    #[cfg(not(unix))]
    {
        tracing::subscriber::with_default(sub, || {
            let _ = load_grants_overlay_at(&missing);
        });
        let seen = captured.0.lock().unwrap();
        assert!(
            !seen
                .iter()
                .any(|(l, _)| *l == tracing::Level::WARN),
            "missing overlay stays silent: {seen:?}"
        );
    }
}

/// Mismatched tuning schema_version must WARN; an exact match must not.
///
/// Kills: `ver != CURRENT_SCHEMA_VERSION` → `==` (would warn on match / stay quiet on mismatch).
#[test]
fn tuning_schema_version_mismatch_warns_match_is_quiet() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let _guard = env_lock().lock().unwrap();
    let data = tempfile::tempdir().unwrap();
    let _data_env = EnvGuard::set(DATA_DIR_ENV, data.path());

    let captured = Captured::default();
    let sub = tracing_subscriber::registry().with(captured.clone());

    let cfg_mismatch = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg_mismatch.path().join(TOPOLOGY_FILE),
        "vault_path = \"/tmp/schema-survivor-vault\"\n",
    )
    .unwrap();
    std::fs::write(
        cfg_mismatch.path().join(TUNING_FILE),
        format!("schema_version = \"not-{CURRENT_SCHEMA_VERSION}\"\n"),
    )
    .unwrap();

    let cfg_match = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg_match.path().join(TOPOLOGY_FILE),
        "vault_path = \"/tmp/schema-survivor-vault\"\n",
    )
    .unwrap();
    std::fs::write(
        cfg_match.path().join(TUNING_FILE),
        format!("schema_version = \"{CURRENT_SCHEMA_VERSION}\"\n"),
    )
    .unwrap();

    tracing::subscriber::with_default(sub, || {
        load_config(Some(cfg_mismatch.path())).expect("mismatch still loads");
        load_config(Some(cfg_match.path())).expect("match still loads");
    });

    let seen = captured.0.lock().unwrap();
    let mismatch_warns = seen.iter().any(|(l, m)| {
        *l == tracing::Level::WARN
            && m.contains(&format!("schema_version 'not-{CURRENT_SCHEMA_VERSION}'"))
    });
    assert!(
        mismatch_warns,
        "mismatched schema_version must warn: {seen:?}"
    );
    // Mutant `!=`→`==` warns when the configured value equals current.
    let match_warns = seen.iter().any(|(l, m)| {
        *l == tracing::Level::WARN
            && m.contains(&format!("schema_version '{CURRENT_SCHEMA_VERSION}' does not match"))
    });
    assert!(
        !match_warns,
        "matching schema_version must stay quiet: {seen:?}"
    );
}
