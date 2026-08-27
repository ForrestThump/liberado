//! Split from `lib.rs`: kills the baseline campaign's survivors.
//!
//! Covers the platform-config path helpers under `XDG_CONFIG_HOME`, the
//! settings load/save round trip through the real config dir, registry
//! emptiness, and the LoadError display.

use super::*;
use std::sync::Mutex;
use std::sync::OnceLock;

/// Serialises every test that points `XDG_CONFIG_HOME` somewhere else.
fn xdg_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Serialises every test that points the platform config root somewhere else.
///
/// `dirs::config_dir()` reads `XDG_CONFIG_HOME` on Linux but `%APPDATA%` on
/// Windows, so the fixture redirects whichever variable the running platform
/// actually consults. A test that sets only the XDG variable passes on a
/// developer Linux box and silently tests nothing on Windows.
fn config_root_var() -> &'static str {
    if cfg!(windows) {
        "APPDATA"
    } else {
        "XDG_CONFIG_HOME"
    }
}

struct RestoreXdg(Option<std::ffi::OsString>);

impl RestoreXdg {
    fn set_to(path: &Path) -> Self {
        let var = config_root_var();
        let prior = std::env::var_os(var);
        unsafe {
            std::env::set_var(var, path);
        }
        Self(prior)
    }
}

impl Drop for RestoreXdg {
    fn drop(&mut self) {
        let var = config_root_var();
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
    }
}

#[test]
fn user_paths_hang_off_the_platform_config_root() {
    // Windows' `dirs` resolves through the Known-Folders API, which ignores
    // every environment variable, so exact-root redirection is only possible
    // off Windows. The `<root>/liberado` join structure is pinned either way.
    if cfg!(windows) {
        let root = dirs::config_dir().expect("roaming app data resolves");
        assert_eq!(user_config_dir(), Some(root.join("liberado")));
        assert_eq!(
            user_themes_dir(),
            Some(root.join("liberado").join("themes")),
            "themes live one level deeper"
        );
        assert_eq!(
            user_settings_path(),
            Some(root.join("liberado").join("settings.toml"))
        );
        return;
    }
    let _guard = xdg_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _restore = RestoreXdg::set_to(dir.path());
    assert_eq!(
        user_config_dir(),
        Some(dir.path().join("liberado")),
        "config dir is <platform root>/liberado"
    );
    assert_eq!(
        user_themes_dir(),
        Some(dir.path().join("liberado").join("themes")),
        "themes live one level deeper"
    );
    assert_eq!(
        user_settings_path(),
        Some(dir.path().join("liberado").join("settings.toml"))
    );
}

#[test]
fn save_then_load_round_trips_through_the_real_config_dir() {
    let _guard = xdg_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _restore = RestoreXdg::set_to(dir.path());

    save_theme_preference("  solarized  ").expect("save creates the config tree");
    let on_disk = std::fs::read_to_string(user_settings_path().unwrap()).unwrap();
    assert!(on_disk.contains("solarized"), "{on_disk}");
    assert_eq!(load_ui_settings().theme.as_deref(), Some("solarized"));

    // A second save overwrites the preference, not the file.
    save_theme_preference("nord").unwrap();
    assert_eq!(load_ui_settings().theme.as_deref(), Some("nord"));
}

#[test]
fn the_built_in_registry_is_never_empty() {
    let registry = ThemeRegistry::new();
    assert!(!registry.is_empty());
    assert_eq!(registry.len(), 3, "dark, light, nord ship built in");
}

#[test]
fn load_errors_name_the_file_and_the_problem() {
    let err = LoadError {
        path: PathBuf::from("/themes/broken.toml"),
        message: "invalid TOML: bad value".into(),
    };
    assert_eq!(
        err.to_string(),
        "/themes/broken.toml: invalid TOML: bad value"
    );
}
