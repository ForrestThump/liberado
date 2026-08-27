//! Split from `coding_run.rs` for module-health boundaries.

use super::*;

/// Serializes the process-global `LIBERADO_ACP_MAX_TURNS` read.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn env_override_wins_then_config_then_default() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var("LIBERADO_ACP_MAX_TURNS").ok();

    for (raw, expect) in [("7", Some(7)), ("0", None), ("-3", None), ("bananas", None)] {
        unsafe { std::env::set_var("LIBERADO_ACP_MAX_TURNS", raw) };
        assert_eq!(
            resolve_max_turns(Some(20)),
            expect.unwrap_or(20),
            "env {raw:?}: a usable override wins, anything else falls through to config"
        );
    }

    unsafe { std::env::remove_var("LIBERADO_ACP_MAX_TURNS") };
    assert_eq!(
        resolve_max_turns(Some(20)),
        20,
        "config applies without env"
    );
    assert_eq!(
        resolve_max_turns(Some(0)),
        DEFAULT_ACP_MAX_TURNS,
        "a zero config value is not a budget; the default stands in"
    );
    assert_eq!(resolve_max_turns(None), DEFAULT_ACP_MAX_TURNS);

    match saved {
        Some(v) => unsafe { std::env::set_var("LIBERADO_ACP_MAX_TURNS", v) },
        None => unsafe { std::env::remove_var("LIBERADO_ACP_MAX_TURNS") },
    }
}
