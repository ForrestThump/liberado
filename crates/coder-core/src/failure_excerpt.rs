//! Pull the specific failing lines out of a check log.
//!
//! Same matcher `liberado ci` uses: compiler diagnostics, test failures, panics,
//! and CRAP regressions. Compile progress and passing crates stay out. Unknown
//! output is empty — callers that still need *something* can fall back to a tail.

/// Default cap, matching `liberado ci`.
pub const EXTRACT_MAX_LINES: usize = 80;

/// Extract failure lines from `output`, capped at [`EXTRACT_MAX_LINES`].
pub fn extract_failures(output: &str) -> String {
    extract_failures_capped(output, EXTRACT_MAX_LINES, None)
}

/// Extract failure lines, with an optional path named when the cap is hit.
pub fn extract_failures_capped(output: &str, max_lines: usize, more_log: Option<&str>) -> String {
    let text = strip_ansi(output);
    let lines: Vec<&str> = text.lines().collect();
    let mut picked = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (idx, line) in lines.iter().enumerate() {
        if !is_failure_line(line) {
            continue;
        }
        let end = extra_context_end(&lines, idx);
        for (i, candidate) in lines.iter().enumerate().take(end).skip(idx) {
            if seen.insert(i) {
                picked.push((*candidate).to_string());
            }
        }
    }
    if max_lines > 0 && picked.len() > max_lines {
        let more = picked.len() - max_lines;
        picked.truncate(max_lines);
        picked.push(match more_log {
            Some(path) => format!("… {more} more matching lines in {path}"),
            None => format!("… {more} more matching lines"),
        });
    }
    picked.join("\n")
}

/// Last `n` lines of `text`, for logs the matcher does not recognise.
pub fn log_tail(text: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= n {
        return text.trim().to_string();
    }
    lines[lines.len() - n..].join("\n")
}

fn extra_context_end(lines: &[&str], anchor: usize) -> usize {
    let mut end = (anchor + 1).min(lines.len());
    while end < lines.len() && end < anchor + 8 {
        if is_diagnostic_context(lines[end]) {
            end += 1;
        } else {
            break;
        }
    }
    end
}

/// rustc span block: `--> file:line:col`, `|`, `= note:`, and `8 |     code`.
fn is_diagnostic_context(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("-->")
        || trimmed.starts_with('|')
        || trimmed.starts_with("= ")
        || is_source_gutter(trimmed)
}

fn is_source_gutter(trimmed: &str) -> bool {
    let Some((left, _)) = trimmed.split_once('|') else {
        return false;
    };
    let nums = left.trim();
    !nums.is_empty() && nums.chars().all(|c| c.is_ascii_digit())
}

fn is_failure_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    line.contains(" FAILED")
        || lower.contains("error[")
        || lower.contains("error:")
        || lower.contains("panicked at")
        || lower.contains("test result: failed")
        || lower.contains("could not compile")
        || lower.contains("regressed")
        || lower.contains("crap check failed")
        || (line.contains('┆') && line.contains('+') && !line.contains("NEW"))
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
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
}
