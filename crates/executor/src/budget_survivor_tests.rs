//! Split from `budget.rs`: kills the baseline campaign's survivors.
//!
//! Covers the wall-clock limit boundary and the turn-cap adjustment that must
//! preserve extra resource limits.

use super::*;

#[test]
fn wall_clock_limit_exhausts_only_at_the_boundary() {
    let limit = WallClockLimit(std::time::Duration::from_secs(10));
    let under = ResourceUsage {
        turns: 1,
        elapsed: std::time::Duration::from_secs(5),
        tokens: 0,
    };
    assert!(!limit.is_exhausted(&under), "5s under a 10s cap is fine");
    let over = ResourceUsage {
        turns: 1,
        elapsed: std::time::Duration::from_secs(11),
        tokens: 0,
    };
    assert!(limit.is_exhausted(&over));
}

#[test]
fn limits_can_be_chained_and_counted() {
    let plain = Budget::new(4);
    assert_eq!(plain.extra_limit_count(), 0);

    let one = plain
        .clone()
        .with_limit(WallClockLimit(std::time::Duration::from_secs(60)));
    assert_eq!(one.extra_limit_count(), 1);

    let two = one.with_limit(TokenLimit(10_000));
    assert_eq!(two.extra_limit_count(), 2);
}

/// Adjusting the turn cap keeps every extra limit — dropping them would lift an
/// operator's wall-clock/token bounds as a side effect.
#[test]
fn with_max_turns_preserves_extra_limits() {
    let budget = Budget::new(4)
        .with_limit(WallClockLimit(std::time::Duration::from_secs(60)))
        .with_limit(TokenLimit(10_000))
        .with_max_turns(9);
    assert_eq!(budget.max_turns, 9);
    assert_eq!(
        budget.extra_limit_count(),
        2,
        "the limits must survive the adjustment"
    );
}
