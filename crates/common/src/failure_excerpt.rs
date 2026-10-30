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
        || is_crap_failure(line, &lower)
        || (line.contains('┆') && line.contains('+') && !line.contains("NEW"))
}

fn is_crap_failure(line: &str, lower: &str) -> bool {
    lower.contains("crap check failed")
        || lower.contains("function(s) exceed crap threshold")
        || (line.contains('✗') && line.contains('┆'))
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
#[path = "failure_excerpt_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "failure_excerpt_survivor_tests.rs"]
mod survivor_tests;
