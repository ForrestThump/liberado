//! Split from `cron_delivery.rs` for module-health boundaries.

use super::*;

const QUIET: Duration = Duration::from_secs(300);
const CAP: Duration = Duration::from_secs(2700);

#[test]
fn never_active_delivers_immediately() {
    assert_eq!(
        ChatDeliveringNotifier::next_wait(None, Duration::ZERO, QUIET, CAP),
        None
    );
}

#[test]
fn quiet_long_enough_delivers_immediately() {
    assert_eq!(
        ChatDeliveringNotifier::next_wait(Some(QUIET), Duration::ZERO, QUIET, CAP),
        None
    );
    assert_eq!(
        ChatDeliveringNotifier::next_wait(
            Some(QUIET + Duration::from_secs(1)),
            Duration::ZERO,
            QUIET,
            CAP
        ),
        None
    );
}

#[test]
fn recently_active_waits_the_remaining_quiet() {
    // Idle 60s of a 300s window, brief just became ready → wait ~240s.
    let wait = ChatDeliveringNotifier::next_wait(
        Some(Duration::from_secs(60)),
        Duration::ZERO,
        QUIET,
        CAP,
    )
    .unwrap();
    assert_eq!(wait, Duration::from_secs(240));
}

#[test]
fn cap_forces_delivery_even_while_active() {
    // Chat still active (idle 10s), but the brief has been held past the cap → deliver now.
    assert_eq!(
        ChatDeliveringNotifier::next_wait(Some(Duration::from_secs(10)), CAP, QUIET, CAP),
        None
    );
}

#[test]
fn wait_is_bounded_by_the_cap() {
    // Idle is tiny (would want ~300s) but only 100s of headroom remains before the cap → wait
    // the smaller, capped amount.
    let elapsed = CAP - Duration::from_secs(100);
    let wait = ChatDeliveringNotifier::next_wait(Some(Duration::from_secs(1)), elapsed, QUIET, CAP)
        .unwrap();
    assert_eq!(wait, Duration::from_secs(100));
}
