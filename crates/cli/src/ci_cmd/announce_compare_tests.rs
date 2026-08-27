//! Split from `ci_cmd.rs`: announce_compare banner/ratchet behaviour.

/// `announce_compare` writes its banners into the ci log and derives the ratchet flag from
/// the baseline state. With no baseline file there is nothing to regress against, so the
/// per-function ratchet stays off.
#[test]
fn announce_compare_logs_banners_without_a_baseline() {
    let temp = tempdir().unwrap();
    let log = CiLog::create(temp.path()).unwrap();
    let fail_regression = announce_compare(&log).unwrap();
    assert!(
        !fail_regression,
        "no baseline file means no per-function regression check"
    );
    let logged = fs::read_to_string(&log.path).unwrap();
    assert!(
        logged.contains(CRAP_EMPTY_BASELINE),
        "empty-baseline banner must reach the ci log, got:\n{logged}"
    );
    assert!(
        logged.contains(CRAP_COMPARE_SUMMARY),
        "summary banner must reach the ci log, got:\n{logged}"
    );
}
