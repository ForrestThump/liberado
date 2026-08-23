//! Split from `shutdown.rs` for module-health boundaries.

use super::*;

/// The env override wins, an unset var falls back to the 300s default, and garbage parses
/// as "not set" rather than zero — a daemon that reads a broken setting as "no grace" would
/// kill in-flight turns instantly.
#[test]
fn shutdown_grace_reads_env_then_falls_back_to_the_default() {
    // SAFETY(test): this var is read only by this function; restored per case.
    unsafe {
        std::env::set_var("LIBERADO_SHUTDOWN_GRACE_SECS", "7");
    }
    assert_eq!(shutdown_grace_from_env(), Duration::from_secs(7));
    unsafe {
        std::env::set_var("LIBERADO_SHUTDOWN_GRACE_SECS", "not-a-number");
    }
    assert_eq!(shutdown_grace_from_env(), DEFAULT_SHUTDOWN_GRACE);
    unsafe {
        std::env::remove_var("LIBERADO_SHUTDOWN_GRACE_SECS");
    }
    assert_eq!(shutdown_grace_from_env(), DEFAULT_SHUTDOWN_GRACE);
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
