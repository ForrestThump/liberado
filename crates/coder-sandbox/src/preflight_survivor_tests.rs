//! Split from `preflight.rs` for module-health boundaries.

use super::*;

/// The tail-cap must not split a character and must not read past the end of the
/// string when advancing to the next boundary.
#[test]
fn cap_log_tails_on_char_boundaries_without_panicking() {
    assert_eq!(cap_log("short", 100), "short");
    // Cut inside the 2-byte é: advance forward, never past the end.
    let s = String::from("abcé") + &"x".repeat(50);
    let capped = cap_log(&s, 5);
    assert!(capped.starts_with('\u{2026}'));
    assert_eq!(capped.chars().next_back(), Some('x'));
    // Pure-multibyte tail: boundary search must terminate.
    let _ = cap_log("ééééé", 4);
}

/// A bare `RUSTSEC-` with no id after it is not an advisory id; only longer
/// identifiers are collected.
#[test]
fn failure_identities_ignore_a_bare_rustsec_prefix() {
    let log = "error: 1 vulnerability\nbare RUSTSEC- marker line\nreal: RUSTSEC-2023-0001\n";
    let ids = failure_identities(log);
    assert_eq!(
        ids.iter().cloned().collect::<Vec<_>>(),
        vec!["RUSTSEC-2023-0001".to_string()]
    );
}

/// Combined output is stdout and stderr joined by a newline — with no separator noise
/// when one side is empty.
#[cfg(unix)]
#[tokio::test]
async fn run_step_shell_joins_stdout_and_stderr_with_one_newline() {
    let both = run_step_shell(".", "printf out; printf err 1>&2", 30)
        .await
        .unwrap();
    assert!(!both.timed_out);
    assert_eq!(both.exit_code, Some(0));
    assert_eq!(both.combined, "out\nerr");

    // Empty stderr adds nothing: no stray newline after stdout.
    let quiet = run_step_shell(".", "printf out", 30).await.unwrap();
    assert_eq!(quiet.combined, "out");

    // Empty stdout adds nothing before stderr either.
    let only_err = run_step_shell(".", "printf err 1>&2", 30).await.unwrap();
    assert_eq!(only_err.combined, "err");
}
