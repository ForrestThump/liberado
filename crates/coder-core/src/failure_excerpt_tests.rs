//! Split from `failure_excerpt.rs` for module-health boundaries.

use super::*;

#[test]
fn extract_names_compiler_errors_and_keeps_the_span() {
    let log = "\
    Checking done-kickback-sandbox v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.08s
error[E0308]: mismatched types
  --> src/lib.rs:8:18
   |
 8 |     let x: u32 = \"kickback\";
   |                  ^^^^^^^^^^ expected `u32`, found `&str`
error: could not compile `done-kickback-sandbox` (lib) due to 1 previous error
";
    let extracted = extract_failures(log);
    assert!(extracted.contains("error[E0308]"), "{extracted}");
    assert!(extracted.contains("src/lib.rs:8:18"), "{extracted}");
    assert!(
        extracted.contains("expected `u32`, found `&str`"),
        "{extracted}"
    );
    assert!(!extracted.contains("Checking"), "{extracted}");
    assert!(!extracted.contains("Finished"), "{extracted}");
}

#[test]
fn extract_names_tests_and_crap_the_way_just_ci_does() {
    let log = "\
Compiling liberado-notify v0.1.0
running 19 tests
test tests::channel_name_is_telegram ... ok
test tests::from_env_reads_both_telegram_vars_and_default_base ... FAILED

thread 'tests::from_env_reads_both_telegram_vars_and_default_base' panicked at crates/notify/src/lib.rs:797:29:
both vars set -> Some

test result: FAILED. 17 passed; 1 failed; 2 ignored

error: test failed, to rerun pass `-p liberado-notify --lib`

error[E0425]: cannot find value `foo` in this scope
  --> crates/cli/src/ci_cmd.rs:123:5
   |
123 |     foo
    |     ^^^ not found in this scope
    = note: this error originates from a macro

↑ 1 regressed  ↓ 0 improved  ★ 0 new
│ ✓ ┆ 30.0 ┆ +18.0 ┆  5 ┆ compare_to_baseline
";
    let extracted = extract_failures(log);
    assert!(
        extracted.contains("from_env_reads_both_telegram_vars_and_default_base ... FAILED"),
        "{extracted}"
    );
    assert!(extracted.contains("panicked at"), "{extracted}");
    assert!(extracted.contains("error[E0425]"), "{extracted}");
    assert!(
        extracted.contains("crates/cli/src/ci_cmd.rs:123:5"),
        "{extracted}"
    );
    assert!(
        extracted.contains("error: test failed, to rerun pass `-p liberado-notify --lib`"),
        "{extracted}"
    );
    assert!(extracted.contains("↑ 1 regressed"), "{extracted}");
    assert!(extracted.contains("compare_to_baseline"), "{extracted}");
    assert!(!extracted.contains("Compiling"), "{extracted}");
    assert!(
        !extracted.contains("channel_name_is_telegram ... ok"),
        "{extracted}"
    );
}

#[test]
fn extract_caps_and_names_the_log_when_asked() {
    let mut log = String::new();
    for i in 0..(EXTRACT_MAX_LINES + 20) {
        log.push_str(&format!("error[E0001]: boom {i}\n"));
    }
    let extracted = extract_failures_capped(&log, EXTRACT_MAX_LINES, Some("ci.log"));
    let lines: Vec<_> = extracted.lines().collect();
    assert!(lines.len() <= EXTRACT_MAX_LINES + 1, "{}", lines.len());
    assert!(extracted.contains("ci.log"), "{extracted}");
    assert!(extracted.contains("more matching lines"), "{extracted}");
}

#[test]
fn strip_ansi_is_applied_before_matching() {
    let colored = "\u{1b}[31merror[E0425]\u{1b}[0m: missing\n";
    assert!(extract_failures(colored).contains("error[E0425]"));
}

#[test]
fn unknown_output_extracts_nothing() {
    assert!(extract_failures("ordinary line\nCompiling foo\n").is_empty());
}

#[test]
fn log_tail_keeps_the_end() {
    let log = (0..10)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tail = log_tail(&log, 3);
    assert_eq!(tail, "line 7\nline 8\nline 9");
}
