//! Split from `preflight_baseline.rs` for module-health boundaries.

use super::*;

/// Dropping the guard must restore the previous `CARGO_TARGET_DIR` exactly — including
/// the case where there was none. Serialized so concurrent env readers in this binary
/// never see a torn state.
#[test]
fn target_dir_guard_restores_the_previous_value() {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap();

    // Compare against the captured previous value instead of presence/absence:
    // under cargo-mutants the variable is already set (target/mutants), so a
    // presence check would pass even with a dead `drop`.
    let before = std::env::var_os("CARGO_TARGET_DIR");

    {
        let guard = CargoTargetDirGuard::set(std::path::Path::new("/tmp/liberado-guard-probe"));
        assert_eq!(
            std::env::var_os("CARGO_TARGET_DIR").as_deref(),
            Some(std::path::Path::new("/tmp/liberado-guard-probe").as_os_str())
        );
        drop(guard);
    }

    assert_eq!(
        std::env::var_os("CARGO_TARGET_DIR"),
        before,
        "a dropped guard must restore the previous value exactly"
    );
}
