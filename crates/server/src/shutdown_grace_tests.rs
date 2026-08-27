//! Split from `shutdown.rs` for module-health boundaries.

use super::*;

/// One lock for every env mutation in this binary's tests — same-named
/// function-local mutexes would be distinct locks and exclude nothing. Held for
/// the whole test (no awaits inside) so parallel tests cannot observe a torn or
/// leaked value even when an assert panics mid-sequence: the poisoned process
/// env is the failure being reported, not a side effect to hide.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The env override wins, an unset var falls back to the 300s default, and garbage parses
/// as "not set" rather than zero — a daemon that reads a broken setting as "no grace" would
/// kill in-flight turns instantly.
#[test]
fn shutdown_grace_reads_env_then_falls_back_to_the_default() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var("LIBERADO_SHUTDOWN_GRACE_SECS").ok();
    // SAFETY(test): this var has a single reader, `shutdown_grace_from_env`; the prior
    // value is restored on every path below so a failed assert cannot leak it.
    unsafe {
        std::env::set_var("LIBERADO_SHUTDOWN_GRACE_SECS", "7");
    }
    let env_wins = shutdown_grace_from_env();
    unsafe {
        std::env::set_var("LIBERADO_SHUTDOWN_GRACE_SECS", "not-a-number");
    }
    let garbage_is_not_zero = shutdown_grace_from_env();
    unsafe {
        std::env::remove_var("LIBERADO_SHUTDOWN_GRACE_SECS");
    }
    let unset_falls_back = shutdown_grace_from_env();
    match saved {
        Some(v) => unsafe { std::env::set_var("LIBERADO_SHUTDOWN_GRACE_SECS", v) },
        None => unsafe { std::env::remove_var("LIBERADO_SHUTDOWN_GRACE_SECS") },
    }

    assert_eq!(env_wins, Duration::from_secs(7));
    assert_eq!(garbage_is_not_zero, DEFAULT_SHUTDOWN_GRACE);
    assert_eq!(unset_falls_back, DEFAULT_SHUTDOWN_GRACE);
}

/// With no signal delivered, the waiter must still be waiting — a variant that returned
/// immediately would end every daemon start before it began serving.
#[tokio::test(start_paused = true)]
async fn wait_for_shutdown_signal_keeps_waiting_without_a_signal() {
    let handle = tokio::spawn(wait_for_shutdown_signal());
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert!(
        !handle.is_finished(),
        "no Ctrl+C and no SIGTERM was delivered; the waiter must still wait"
    );
    handle.abort();
}
