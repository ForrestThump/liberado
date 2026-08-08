//! Hashline: line-anchored patch language for LLM-driven file edits.
//!
//! Ported from oh-my-pi's `@oh-my-pi/hashline` dialect (file-level content-hash tags + line
//! numbers). Liberado differences:
//! - Tag alphabet is uppercase base-36 (`0-9A-Z`), not hex-only.
//! - Tag length is configurable (4–10) via [`HashlineConfig`].
//! - Core ops only: `PUT` range/insert, `CUT`, `REM` (no tree-sitter block ops, clipboard
//!   registers, or stale-tag recovery in this first cut).

use liberado_coder_core::HashlineConfig;
use sha2::{Digest, Sha256};

/// Alphabet for content-hash tags: digits + uppercase letters (base-36).
const HASH_ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Normalize file text before hashing: strip BOM, force LF, trim trailing spaces/tabs/CR per line.
pub fn normalize_for_hash(text: &str) -> String {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        let trimmed = line.trim_end_matches([' ', '\t']);
        out.push_str(trimmed);
    }
    out
}

/// Compute a content-derived hashline tag of the given length (uppercase base-36).
pub fn compute_file_hash(text: &str, hash_length: u8) -> String {
    let len = hash_length.clamp(
        HashlineConfig::HASH_LENGTH_MIN,
        HashlineConfig::HASH_LENGTH_MAX,
    ) as usize;
    let normalized = normalize_for_hash(text);
    let digest = Sha256::digest(normalized.as_bytes());
    // Take the first 16 bytes as a big-endian u128 and emit `len` base-36 digits.
    let mut value = u128::from_be_bytes(digest[..16].try_into().expect("16 bytes"));
    let base = HASH_ALPHABET.len() as u128;
    let mut chars = vec![b'0'; len];
    for i in (0..len).rev() {
        let digit = (value % base) as usize;
        chars[i] = HASH_ALPHABET[digit];
        value /= base;
    }
    String::from_utf8(chars).expect("ascii alphabet")
}

/// Format a section header `[path#TAG]`.
pub fn format_header(path: &str, tag: &str) -> String {
    format!("[{path}#{tag}]")
}

/// One low-level edit against original line numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Edit {
    InsertBefore { line: usize, text: String },
    InsertAfter { line: usize, text: String },
    InsertBof { text: String },
    InsertEof { text: String },
    Delete { line: usize },
    Replace { line: usize, text: String },
}

/// File-level op for a section.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FileOp {
    Update,
    Remove,
}

/// One parsed `[path#tag]` section.
#[derive(Debug, Clone)]
pub struct Section {
    pub path: String,
    pub file_hash: Option<String>,
    edits: Vec<Edit>,
    file_op: FileOp,
}

/// Result of applying a multi-section patch.
#[derive(Debug, Clone)]
pub struct ApplyReport {
    pub path: String,
    pub op: &'static str,
    pub file_hash: Option<String>,
    pub first_changed_line: Option<usize>,
}

/// Parse a full hashline patch into sections.
pub fn parse_patch(input: &str) -> Result<Vec<Section>, String> {
    let input = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let lines: Vec<&str> = input.lines().collect();
    let mut sections = Vec::new();
    let mut i = 0;

    // Skip leading blank lines.
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    if i >= lines.len() {
        return Err("hashline patch is empty".into());
    }

    while i < lines.len() {
        let header = lines[i].trim_end();
        let (path, file_hash) = parse_header(header)?;
        i += 1;

        let mut body = Vec::new();
        while i < lines.len() {
            let line = lines[i];
            if line.trim_start().starts_with('[') && line.trim_end().ends_with(']') {
                // Could be next section header.
                if parse_header(line.trim_end()).is_ok() {
                    break;
                }
            }
            body.push(line);
            i += 1;
        }

        // Drop trailing empty sections with no ops (header-only trailer).
        let body_has_content = body.iter().any(|l| !l.trim().is_empty());
        if !body_has_content {
            continue;
        }

        let (edits, file_op) = parse_section_body(&body)?;
        sections.push(Section {
            path,
            file_hash,
            edits,
            file_op,
        });
    }

    if sections.is_empty() {
        return Err("hashline patch has no operations".into());
    }
    Ok(sections)
}

fn parse_header(line: &str) -> Result<(String, Option<String>), String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(format!(
            "hashline section must start with [PATH#TAG]; got {}",
            json_preview(trimmed)
        ));
    }
    let body = &trimmed[1..trimmed.len() - 1];
    if body.is_empty() {
        return Err("hashline header path is empty".into());
    }
    // Tag is trailing #XXXX where XXXX is 4–10 base-36 chars.
    if let Some(hash_at) = body.rfind('#') {
        let path = body[..hash_at].trim();
        let tag = body[hash_at + 1..].trim().to_uppercase();
        if path.is_empty() {
            return Err("hashline header path is empty".into());
        }
        if path.contains('#') {
            return Err(format!(
                "hashline path must not contain '#'; got {}",
                json_preview(path)
            ));
        }
        if !is_valid_tag(&tag) {
            return Err(format!(
                "hashline tag must be 4–10 uppercase base-36 characters (0-9A-Z); got {}",
                json_preview(&tag)
            ));
        }
        return Ok((normalize_path(path), Some(tag)));
    }
    let path = body.trim();
    if path.is_empty() {
        return Err("hashline header path is empty".into());
    }
    Ok((normalize_path(path), None))
}

fn is_valid_tag(tag: &str) -> bool {
    let len = tag.len() as u8;
    if !(HashlineConfig::HASH_LENGTH_MIN..=HashlineConfig::HASH_LENGTH_MAX).contains(&len) {
        return false;
    }
    tag.bytes()
        .all(|b: u8| b.is_ascii_digit() || b.is_ascii_uppercase())
}

fn normalize_path(path: &str) -> String {
    path.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .replace('\\', "/")
}

fn parse_section_body(lines: &[&str]) -> Result<(Vec<Edit>, FileOp), String> {
    let mut edits = Vec::new();
    let mut file_op = FileOp::Update;
    let mut i = 0;

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }

        // File-level ops.
        if trimmed == "REM" {
            file_op = FileOp::Remove;
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("MV ") {
            return Err(format!(
                "MV (move/rename) is not supported yet; got dest {}",
                json_preview(rest.trim())
            ));
        }

        // CUT N.=M or CUT N
        if let Some(rest) = strip_keyword(trimmed, "CUT") {
            let (start, end) = parse_range(rest.trim())?;
            for line_no in start..=end {
                edits.push(Edit::Delete { line: line_no });
            }
            i += 1;
            continue;
        }

        // PUT variants
        if let Some(rest) = strip_keyword(trimmed, "PUT") {
            let rest = rest.trim();
            let (locator, had_colon) = if let Some(stripped) = rest.strip_suffix(':') {
                (stripped.trim(), true)
            } else {
                (rest, false)
            };

            if !had_colon {
                return Err(format!(
                    "colonless PUT (register paste) is not supported yet; got PUT {}",
                    json_preview(locator)
                ));
            }

            // Collect + body rows.
            i += 1;
            let mut payloads = Vec::new();
            while i < lines.len() {
                let body_line = lines[i].trim_end_matches('\r');
                let body_trim = body_line.trim_start();
                // Next op or section?
                if is_op_header(body_trim)
                    || (body_trim.starts_with('[') && body_trim.ends_with(']'))
                {
                    break;
                }
                if body_trim.is_empty() {
                    // Interior blank: only keep if we already have payloads (trailing blanks discarded later).
                    if !payloads.is_empty() {
                        // Peek ahead — trailing blanks before next header are layout, not content.
                        let mut j = i + 1;
                        while j < lines.len() && lines[j].trim().is_empty() {
                            j += 1;
                        }
                        if j >= lines.len() || is_op_header(lines[j].trim_start()) {
                            break;
                        }
                        payloads.push(String::new());
                    }
                    i += 1;
                    continue;
                }
                if let Some(text) = body_line.strip_prefix('+') {
                    payloads.push(text.to_string());
                } else if let Some(text) = body_trim.strip_prefix('+') {
                    // Indent before + is unusual; accept and keep content after +.
                    payloads.push(text.to_string());
                } else {
                    // Bare body row: treat as payload with warning-level leniency.
                    payloads.push(strip_read_prefix(body_line));
                }
                i += 1;
            }

            apply_put_locator(locator, &payloads, &mut edits)?;
            continue;
        }

        return Err(format!(
            "unrecognized hashline op {}; use PUT / CUT / REM",
            json_preview(trimmed)
        ));
    }

    Ok((edits, file_op))
}

fn is_op_header(s: &str) -> bool {
    let s = s.trim();
    s == "REM"
        || s.starts_with("MV ")
        || strip_keyword(s, "PUT").is_some()
        || strip_keyword(s, "CUT").is_some()
}

fn strip_keyword<'a>(s: &'a str, keyword: &str) -> Option<&'a str> {
    let s = s.trim_start();
    if s.len() < keyword.len() {
        return None;
    }
    if !s[..keyword.len()].eq_ignore_ascii_case(keyword) {
        return None;
    }
    let rest = &s[keyword.len()..];
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
}

fn strip_read_prefix(line: &str) -> String {
    // Strip accidental `N:` read-output prefixes when models paste bare rows.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i < bytes.len() && bytes[i] == b':' {
        return line[i + 1..].to_string();
    }
    line.to_string()
}

fn apply_put_locator(
    locator: &str,
    payloads: &[String],
    edits: &mut Vec<Edit>,
) -> Result<(), String> {
    let locator = locator.trim();

    // Block ops not supported: N*
    if locator.contains('*') {
        return Err(format!(
            "block ops (PUT N*:) are not supported yet; use an explicit range PUT N.=M: — got {}",
            json_preview(locator)
        ));
    }

    // Gap inserts: <N, >N, >$, <1
    if let Some(rest) = locator.strip_prefix('<') {
        let rest = rest.trim();
        if rest == "1" {
            for text in payloads {
                edits.push(Edit::InsertBof { text: text.clone() });
            }
            if payloads.is_empty() {
                return Err("PUT <1: requires at least one + body row".into());
            }
            return Ok(());
        }
        let line = parse_line_number(rest)?;
        if payloads.is_empty() {
            return Err(format!("PUT <{line}: requires at least one + body row"));
        }
        for text in payloads {
            edits.push(Edit::InsertBefore {
                line,
                text: text.clone(),
            });
        }
        return Ok(());
    }

    if let Some(rest) = locator.strip_prefix('>') {
        let rest = rest.trim();
        if rest == "$" {
            for text in payloads {
                edits.push(Edit::InsertEof { text: text.clone() });
            }
            if payloads.is_empty() {
                return Err("PUT >$: requires at least one + body row".into());
            }
            return Ok(());
        }
        let line = parse_line_number(rest)?;
        if payloads.is_empty() {
            return Err(format!("PUT >{line}: requires at least one + body row"));
        }
        for text in payloads {
            edits.push(Edit::InsertAfter {
                line,
                text: text.clone(),
            });
        }
        return Ok(());
    }

    // Range replace: N.=M or N (single)
    let (start, end) = parse_range(locator)?;
    // Empty body + range = invalid (use CUT to delete).
    if payloads.is_empty() {
        return Err(format!(
            "empty PUT {start}.={end}: — use CUT {start}.={end} to delete, or provide + body rows"
        ));
    }
    for line in start..=end {
        edits.push(Edit::Delete { line });
    }
    // Replacements insert at the start line position (before the deleted span).
    for text in payloads {
        edits.push(Edit::Replace {
            line: start,
            text: text.clone(),
        });
    }
    Ok(())
}

fn parse_range(s: &str) -> Result<(usize, usize), String> {
    let s = s.trim();
    // Accept .= , .. , - as separators.
    for sep in [".=", "..", "-", "…"] {
        if let Some((a, b)) = s.split_once(sep) {
            let start = parse_line_number(a.trim())?;
            let end = parse_line_number(b.trim())?;
            if end < start {
                return Err(format!("range end {end} is before start {start}"));
            }
            return Ok((start, end));
        }
    }
    // Single line.
    let n = parse_line_number(s)?;
    Ok((n, n))
}

fn parse_line_number(s: &str) -> Result<usize, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("line number is empty".into());
    }
    let n: usize = s
        .parse()
        .map_err(|_| format!("invalid line number {}", json_preview(s)))?;
    if n == 0 {
        return Err("line numbers are 1-indexed; got 0".into());
    }
    Ok(n)
}

fn json_preview(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("`{s}`"))
}

/// Apply edits to file text. Line numbers refer to the **original** file.
fn apply_edits(text: &str, edits: &[Edit]) -> Result<(String, Option<usize>), String> {
    if edits.is_empty() {
        return Ok((text.to_string(), None));
    }

    // Preserve whether the original ended with a trailing newline via split behaviour.
    let mut file_lines: Vec<String> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();

    // Phantom trailing empty from split on trailing newline is addressable for inserts.
    let mut first_changed: Option<usize> = None;
    let track = |line: usize, first: &mut Option<usize>| {
        if first.is_none_or(|f| line < f) {
            *first = Some(line);
        }
    };

    // Partition BOF/EOF vs anchor edits.
    let mut bof: Vec<String> = Vec::new();
    let mut eof: Vec<String> = Vec::new();
    let mut by_line: std::collections::BTreeMap<usize, Vec<&Edit>> =
        std::collections::BTreeMap::new();

    for edit in edits {
        match edit {
            Edit::InsertBof { text } => bof.push(text.clone()),
            Edit::InsertEof { text } => eof.push(text.clone()),
            Edit::InsertBefore { line, .. }
            | Edit::InsertAfter { line, .. }
            | Edit::Delete { line }
            | Edit::Replace { line, .. } => {
                by_line.entry(*line).or_default().push(edit);
            }
        }
    }

    // Validate bounds.
    let line_count = if file_lines.len() == 1 && file_lines[0].is_empty() {
        0
    } else if text.ends_with('\n') && file_lines.last().is_some_and(|l| l.is_empty()) {
        // trailing phantom
        file_lines.len() - 1
    } else {
        file_lines.len()
    };

    for &line in by_line.keys() {
        if line == 0 || line > line_count.max(1) && line_count > 0 {
            // Allow line == line_count for last line; reject beyond.
            if line > line_count {
                return Err(format!(
                    "line {line} does not exist (file has {line_count} lines)"
                ));
            }
        }
        if line_count == 0 && line > 0 {
            return Err(format!("line {line} does not exist (file is empty)"));
        }
    }

    // Apply bottom-up so earlier indices stay valid.
    let lines_desc: Vec<usize> = by_line.keys().copied().rev().collect();
    for line in lines_desc {
        let bucket = by_line.get(&line).cloned().unwrap_or_default();
        let idx = line - 1;
        if idx >= file_lines.len() {
            return Err(format!(
                "line {line} does not exist (file has {} lines)",
                file_lines.len()
            ));
        }

        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut replacements = Vec::new();
        let mut delete = false;

        for edit in bucket {
            match edit {
                Edit::InsertBefore { text, .. } => before.push(text.clone()),
                Edit::InsertAfter { text, .. } => after.push(text.clone()),
                Edit::Replace { text, .. } => replacements.push(text.clone()),
                Edit::Delete { .. } => delete = true,
                Edit::InsertBof { .. } | Edit::InsertEof { .. } => unreachable!(),
            }
        }

        let current = file_lines[idx].clone();
        let replacement: Vec<String> = if delete {
            before
                .into_iter()
                .chain(replacements)
                .chain(after)
                .collect()
        } else {
            before
                .into_iter()
                .chain(replacements)
                .chain(std::iter::once(current))
                .chain(after)
                .collect()
        };

        file_lines.splice(idx..=idx, replacement);
        track(line, &mut first_changed);
    }

    if !bof.is_empty() {
        if file_lines.len() == 1 && file_lines[0].is_empty() {
            file_lines = bof;
        } else {
            for (i, line) in bof.into_iter().enumerate() {
                file_lines.insert(i, line);
            }
        }
        track(1, &mut first_changed);
    }

    if !eof.is_empty() {
        let insert_at = if file_lines.last().is_some_and(|l| l.is_empty()) && text.ends_with('\n') {
            file_lines.len() - 1
        } else if file_lines.len() == 1 && file_lines[0].is_empty() {
            0
        } else {
            file_lines.len()
        };
        if insert_at == 0 && file_lines.len() == 1 && file_lines[0].is_empty() {
            file_lines = eof;
        } else {
            for (offset, line) in eof.into_iter().enumerate() {
                file_lines.insert(insert_at + offset, line);
            }
        }
        track(insert_at + 1, &mut first_changed);
    }

    let mut result = file_lines.join("\n");
    // If original had trailing newline and we still have content, prefer keeping final newline
    // only when last element was empty phantom — join already includes it when last is "".
    // Empty file edge: single empty string joins to "".
    if result == "\n" {
        result = String::new();
    }

    Ok((result, first_changed))
}

/// Apply a multi-section patch against an in-memory file map (preflight + commit).
///
/// `read` and `write` / `remove` are callbacks so the host can enforce path policy.
pub fn apply_patch_sections<R, W, D>(
    sections: &[Section],
    hash_length: u8,
    require_tag: bool,
    mut read: R,
    mut write: W,
    mut remove: D,
) -> Result<Vec<ApplyReport>, String>
where
    R: FnMut(&str) -> Result<String, String>,
    W: FnMut(&str, &str) -> Result<(), String>,
    D: FnMut(&str) -> Result<(), String>,
{
    // Preflight all sections first (all-or-nothing).
    struct Prepared {
        path: String,
        op: &'static str,
        after: Option<String>,
        file_hash: Option<String>,
        first_changed_line: Option<usize>,
    }

    let mut prepared = Vec::with_capacity(sections.len());

    for section in sections {
        if section.file_op == FileOp::Remove {
            let before = read(&section.path)?;
            if require_tag {
                verify_tag(
                    &section.path,
                    &before,
                    section.file_hash.as_deref(),
                    hash_length,
                )?;
            }
            prepared.push(Prepared {
                path: section.path.clone(),
                op: "delete",
                after: None,
                file_hash: None,
                first_changed_line: None,
            });
            continue;
        }

        let before = read(&section.path)?;
        if require_tag {
            verify_tag(
                &section.path,
                &before,
                section.file_hash.as_deref(),
                hash_length,
            )?;
        }
        let (after, first) = apply_edits(&before, &section.edits)?;
        let new_hash = compute_file_hash(&after, hash_length);
        let op = if after == before { "noop" } else { "update" };
        prepared.push(Prepared {
            path: section.path.clone(),
            op,
            after: Some(after),
            file_hash: Some(new_hash),
            first_changed_line: first,
        });
    }

    // Commit.
    let mut reports = Vec::with_capacity(prepared.len());
    for p in prepared {
        match p.op {
            "delete" => {
                remove(&p.path)?;
            }
            "update" | "noop" => {
                if let Some(after) = &p.after
                    && p.op == "update"
                {
                    write(&p.path, after)?;
                }
            }
            _ => {}
        }
        reports.push(ApplyReport {
            path: p.path,
            op: p.op,
            file_hash: p.file_hash,
            first_changed_line: p.first_changed_line,
        });
    }
    Ok(reports)
}

fn verify_tag(
    path: &str,
    content: &str,
    expected: Option<&str>,
    hash_length: u8,
) -> Result<(), String> {
    let Some(expected) = expected else {
        return Err(format!(
            "hashline section for {path} is missing #TAG — re-read the file and use [path#TAG] from the read header"
        ));
    };
    let actual = compute_file_hash(content, hash_length);
    // Compare using the configured length; also accept if expected was computed at same length.
    if !actual.eq_ignore_ascii_case(expected) {
        // If the expected tag has a different length, recompute at that length for a clearer error.
        let expected_len = expected.len() as u8;
        let actual_at_expected = if (HashlineConfig::HASH_LENGTH_MIN
            ..=HashlineConfig::HASH_LENGTH_MAX)
            .contains(&expected_len)
        {
            compute_file_hash(content, expected_len)
        } else {
            actual.clone()
        };
        if actual_at_expected.eq_ignore_ascii_case(expected) {
            return Ok(());
        }
        return Err(format!(
            "stale hashline tag for {path}: patch has #{expected}, file is now #{actual}. Re-read the file and retry with the current tag."
        ));
    }
    Ok(())
}

/// Short prompt appendix when hashline mode is enabled.
pub fn prompt_guidance(hash_length: u8) -> String {
    format!(
        "\n\n## Hashline edit mode (enabled)\n\
         - `read_file` returns `[path#TAG]` then `LINE:content` rows. TAG is a {hash_length}-char \
         uppercase base-36 (0-9A-Z) content hash of the whole file.\n\
         - Prefer `hashline_edit` for existing files. Pass a patch string:\n\
         ```\n\
         [path/to/file.rs#TAG]\n\
         PUT 3.=5:\n\
         +replacement line one\n\
         +replacement line two\n\
         PUT >10:\n\
         +inserted after line 10\n\
         CUT 12.=12\n\
         ```\n\
         - Ops: `PUT N.=M:` replace inclusive range; `PUT <N:` / `PUT >N:` insert before/after; \
         `PUT >$:` append; `CUT N.=M` delete; `REM` delete whole file.\n\
         - Body rows are only final content, each starting with `+`. Ranges use **original** line \
         numbers from your last read. After every edit the tag changes — re-read before the next edit.\n\
         - Still use `write_file` for new files. `edit_file` / `apply_patch` remain available for exact string replacements.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_correct_length() {
        let h4 = compute_file_hash("hello\n", 4);
        assert_eq!(h4.len(), 4);
        assert!(h4.bytes().all(|b| HASH_ALPHABET.contains(&b)));
        assert_eq!(compute_file_hash("hello\n", 4), h4);

        let h10 = compute_file_hash("hello\n", 10);
        assert_eq!(h10.len(), 10);
    }

    #[test]
    fn hash_ignores_trailing_whitespace_and_crlf() {
        let a = compute_file_hash("line  \r\n", 6);
        let b = compute_file_hash("line\n", 6);
        assert_eq!(a, b);
    }

    #[test]
    fn apply_replace_single_line() {
        let content = "aaa\nbbb\nccc";
        let patch = format!("[f.txt#{}]\nPUT 2.=2:\n+BBB", compute_file_hash(content, 4));
        let sections = parse_patch(&patch).unwrap();
        let (after, first) = apply_edits(content, &sections[0].edits).unwrap();
        assert_eq!(after, "aaa\nBBB\nccc");
        assert_eq!(first, Some(2));
    }

    #[test]
    fn apply_inserts_and_cut() {
        let content = "aaa\nbbb\nccc";
        let edits = parse_patch(&format!(
            "[f.txt#{}]\nPUT <2:\n+before b\nPUT >2:\n+after b\nPUT <1:\n+top\nPUT >$:\n+tail",
            compute_file_hash(content, 4)
        ))
        .unwrap()
        .remove(0)
        .edits;
        let (after, _) = apply_edits(content, &edits).unwrap();
        assert_eq!(after, "top\naaa\nbefore b\nbbb\nafter b\nccc\ntail");

        let cut = parse_patch(&format!(
            "[f.txt#{}]\nCUT 2.=2",
            compute_file_hash(content, 4)
        ))
        .unwrap()
        .remove(0)
        .edits;
        let (after, _) = apply_edits(content, &cut).unwrap();
        assert_eq!(after, "aaa\nccc");
    }

    #[test]
    fn rejects_stale_tag() {
        use std::cell::RefCell;
        let files = RefCell::new(std::collections::HashMap::from([(
            "a.txt".to_string(),
            "alpha\n".to_string(),
        )]));
        let sections = parse_patch("[a.txt#ZZZZ]\nPUT 1.=1:\n+BETA\n").unwrap();
        let err = apply_patch_sections(
            &sections,
            4,
            true,
            |p| {
                files
                    .borrow()
                    .get(p)
                    .cloned()
                    .ok_or_else(|| "missing".into())
            },
            |p, c| {
                files.borrow_mut().insert(p.to_string(), c.to_string());
                Ok(())
            },
            |p| {
                files.borrow_mut().remove(p);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(err.contains("stale hashline tag"), "{err}");
    }

    #[test]
    fn multi_section_atomic_preflight() {
        use std::cell::RefCell;
        let files = RefCell::new(std::collections::HashMap::from([
            ("a.txt".to_string(), "aaa\n".to_string()),
            ("b.txt".to_string(), "bbb\n".to_string()),
        ]));
        let a_tag = compute_file_hash("aaa\n", 4);
        let patch = format!("[a.txt#{a_tag}]\nPUT 1.=1:\n+AAA\n[b.txt#DEAD]\nPUT 1.=1:\n+BBB");
        let sections = parse_patch(&patch).unwrap();
        let err = apply_patch_sections(
            &sections,
            4,
            true,
            |p| {
                files
                    .borrow()
                    .get(p)
                    .cloned()
                    .ok_or_else(|| format!("missing {p}"))
            },
            |p, c| {
                files.borrow_mut().insert(p.to_string(), c.to_string());
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(err.contains("stale") || err.contains("DEAD"), "{err}");
        // a.txt must not have been written.
        assert_eq!(
            files.borrow().get("a.txt").map(String::as_str),
            Some("aaa\n")
        );
    }

    #[test]
    fn format_header_includes_path_and_tag() {
        let content = "one\ntwo";
        let tag = compute_file_hash(content, 4);
        let header = format_header("src/a.rs", &tag);
        assert_eq!(header, format!("[src/a.rs#{tag}]"));
        assert!(tag.bytes().all(|b| HASH_ALPHABET.contains(&b)));
    }

    #[test]
    fn hash_alphabet_is_digits_and_uppercase_only() {
        // Sample several inputs across lengths; every digit must be in 0-9A-Z.
        let long = "x".repeat(4096);
        let samples: Vec<&str> = vec!["", "a", "a\n", "hello world\n", "unicode café\n", &long];
        for len in HashlineConfig::HASH_LENGTH_MIN..=HashlineConfig::HASH_LENGTH_MAX {
            for sample in &samples {
                let tag = compute_file_hash(sample, len);
                assert_eq!(tag.len(), len as usize, "len={len} sample={sample:?}");
                assert!(
                    tag.bytes()
                        .all(|b: u8| b.is_ascii_digit() || b.is_ascii_uppercase()),
                    "non base-36 char in {tag}"
                );
            }
        }
    }

    #[test]
    fn different_content_usually_differs() {
        let a = compute_file_hash("alpha\n", 8);
        let b = compute_file_hash("beta\n", 8);
        assert_ne!(a, b);
    }

    #[test]
    fn parse_rejects_empty_patch() {
        assert!(parse_patch("").is_err());
        assert!(parse_patch("   \n\n").is_err());
    }

    #[test]
    fn parse_rejects_malformed_header() {
        assert!(parse_patch("not a header\nPUT 1.=1:\n+x").is_err());
        assert!(parse_patch("[#ABCD]\nPUT 1.=1:\n+x").is_err());
        assert!(parse_patch("[path#ab]\nPUT 1.=1:\n+x").is_err()); // tag too short
        assert!(parse_patch("[path#GGGGGGGGGGG]\nPUT 1.=1:\n+x").is_err()); // too long
        assert!(parse_patch("[path#abcd]\nPUT 1.=1:\n+x").is_ok()); // lower-case accepted → upper
    }

    #[test]
    fn parse_normalizes_windows_paths_and_quotes() {
        // One backslash in the path (Windows-style) becomes a forward slash.
        let sections = parse_patch("[\"src\\foo.rs\"#A1B2]\nPUT 1.=1:\n+x").unwrap();
        assert_eq!(sections[0].path, "src/foo.rs");
        assert_eq!(sections[0].file_hash.as_deref(), Some("A1B2"));
    }

    #[test]
    fn parse_accepts_legacy_range_separators() {
        for sep in [".=", "..", "-"] {
            let patch = format!("[f#A1B2]\nPUT 1{sep}2:\n+a\n+b");
            let sections = parse_patch(&patch).unwrap();
            assert_eq!(sections[0].edits.len(), 4, "sep={sep}"); // 2 deletes + 2 replaces
        }
    }

    #[test]
    fn parse_rejects_empty_put_body() {
        let err = parse_patch("[f#A1B2]\nPUT 1.=1:\n").unwrap_err();
        assert!(err.contains("CUT") || err.contains("empty"), "{err}");
    }

    #[test]
    fn parse_rejects_block_ops_and_mv() {
        let err = parse_patch("[f#A1B2]\nPUT 1*:\n+x").unwrap_err();
        assert!(err.contains("block"), "{err}");
        let err = parse_patch("[f#A1B2]\nMV other.rs").unwrap_err();
        assert!(err.contains("MV"), "{err}");
    }

    #[test]
    fn parse_rejects_colonless_register_put() {
        let err = parse_patch("[f#A1B2]\nPUT >1 @reg").unwrap_err();
        assert!(
            err.contains("register") || err.contains("not supported"),
            "{err}"
        );
    }

    #[test]
    fn parse_rem_section() {
        let sections = parse_patch("[doomed.txt#A1B2]\nREM").unwrap();
        assert_eq!(sections[0].file_op, FileOp::Remove);
    }

    #[test]
    fn parse_multi_section_split() {
        let patch = "[a.ts#A1B2]\nPUT 1.=1:\n+A\n[b.ts#C3D4]\nCUT 1.=1\n";
        let sections = parse_patch(patch).unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].path, "a.ts");
        assert_eq!(sections[1].path, "b.ts");
    }

    #[test]
    fn apply_range_replace_multi_line() {
        let content = "a\nb\nc\nd";
        let edits = parse_patch("[f#A1B2]\nPUT 2.=3:\n+B\n+C")
            .unwrap()
            .remove(0)
            .edits;
        let (after, first) = apply_edits(content, &edits).unwrap();
        assert_eq!(after, "a\nB\nC\nd");
        assert_eq!(first, Some(2));
    }

    #[test]
    fn apply_cut_range() {
        let content = "a\nb\nc\nd";
        let edits = parse_patch("[f#A1B2]\nCUT 2.=3").unwrap().remove(0).edits;
        let (after, _) = apply_edits(content, &edits).unwrap();
        assert_eq!(after, "a\nd");
    }

    #[test]
    fn apply_out_of_bounds_line_errors() {
        let content = "only\n";
        let edits = parse_patch("[f#A1B2]\nCUT 5.=5").unwrap().remove(0).edits;
        let err = apply_edits(content, &edits).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn apply_preserves_payload_whitespace_and_sigils() {
        let content = "x";
        let payload = "\tconst x = 1;";
        let edits = parse_patch(&format!("[f#A1B2]\nPUT 1.=1:\n+{payload}"))
            .unwrap()
            .remove(0)
            .edits;
        let (after, _) = apply_edits(content, &edits).unwrap();
        assert_eq!(after, payload);
    }

    #[test]
    fn apply_markdown_bullet_body_rows() {
        // Body `+- item` must land as `- item`.
        let content = "title\n";
        let edits = parse_patch("[f#A1B2]\nPUT >1:\n+- item\n+  - nested")
            .unwrap()
            .remove(0)
            .edits;
        let (after, _) = apply_edits(content, &edits).unwrap();
        // Source ended with `\n`, so the trailing newline (split phantom) is preserved.
        assert_eq!(after, "title\n- item\n  - nested\n");
    }

    #[test]
    fn missing_tag_rejected_when_required() {
        use std::cell::RefCell;
        let files = RefCell::new(std::collections::HashMap::from([(
            "a.txt".to_string(),
            "alpha\n".to_string(),
        )]));
        // Header without #TAG
        let sections = parse_patch("[a.txt]\nPUT 1.=1:\n+BETA\n").unwrap();
        assert!(sections[0].file_hash.is_none());
        let err = apply_patch_sections(
            &sections,
            4,
            true,
            |p| {
                files
                    .borrow()
                    .get(p)
                    .cloned()
                    .ok_or_else(|| "missing".into())
            },
            |_, _| Ok(()),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(err.contains("missing #TAG"), "{err}");
    }

    #[test]
    fn rem_deletes_file_when_tag_matches() {
        use std::cell::RefCell;
        let content = "bye\n";
        let tag = compute_file_hash(content, 4);
        let files = RefCell::new(std::collections::HashMap::from([(
            "gone.txt".to_string(),
            content.to_string(),
        )]));
        let sections = parse_patch(&format!("[gone.txt#{tag}]\nREM")).unwrap();
        let reports = apply_patch_sections(
            &sections,
            4,
            true,
            |p| {
                files
                    .borrow()
                    .get(p)
                    .cloned()
                    .ok_or_else(|| "missing".into())
            },
            |_, _| Ok(()),
            |p| {
                files.borrow_mut().remove(p);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(reports[0].op, "delete");
        assert!(files.borrow().get("gone.txt").is_none());
    }

    #[test]
    fn successful_multi_section_commit() {
        use std::cell::RefCell;
        let a = "aaa\n";
        let b = "bbb\n";
        let a_tag = compute_file_hash(a, 4);
        let b_tag = compute_file_hash(b, 4);
        let files = RefCell::new(std::collections::HashMap::from([
            ("a.txt".to_string(), a.to_string()),
            ("b.txt".to_string(), b.to_string()),
        ]));
        let patch = format!("[a.txt#{a_tag}]\nPUT 1.=1:\n+AAA\n[b.txt#{b_tag}]\nPUT 1.=1:\n+BBB");
        let sections = parse_patch(&patch).unwrap();
        let reports = apply_patch_sections(
            &sections,
            4,
            true,
            |p| {
                files
                    .borrow()
                    .get(p)
                    .cloned()
                    .ok_or_else(|| format!("missing {p}"))
            },
            |p, c| {
                files.borrow_mut().insert(p.to_string(), c.to_string());
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(reports.len(), 2);
        // Trailing newline preserved from the original files.
        assert_eq!(
            files.borrow().get("a.txt").map(String::as_str),
            Some("AAA\n")
        );
        assert_eq!(
            files.borrow().get("b.txt").map(String::as_str),
            Some("BBB\n")
        );
        // New tags are content hashes of post-edit text.
        assert_eq!(
            reports[0].file_hash.as_deref(),
            Some(compute_file_hash("AAA\n", 4).as_str())
        );
    }

    #[test]
    fn tag_length_mismatch_still_validates_when_content_matches() {
        // Tag computed at length 8; applier configured for 4 — should recompute at expected len.
        use std::cell::RefCell;
        let content = "same\n";
        let tag8 = compute_file_hash(content, 8);
        let files = RefCell::new(std::collections::HashMap::from([(
            "a.txt".to_string(),
            content.to_string(),
        )]));
        let sections = parse_patch(&format!("[a.txt#{tag8}]\nPUT 1.=1:\n+new\n")).unwrap();
        let reports = apply_patch_sections(
            &sections,
            4,
            true,
            |p| {
                files
                    .borrow()
                    .get(p)
                    .cloned()
                    .ok_or_else(|| "missing".into())
            },
            |p, c| {
                files.borrow_mut().insert(p.to_string(), c.to_string());
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(reports[0].op, "update");
    }

    #[test]
    fn prompt_guidance_mentions_length_and_ops() {
        let g = prompt_guidance(7);
        assert!(g.contains('7'));
        assert!(g.contains("hashline_edit"));
        assert!(g.contains("PUT"));
        assert!(g.contains("0-9A-Z") || g.contains("base-36"));
    }

    #[test]
    fn normalize_strips_bom() {
        let with_bom = "\u{FEFF}line\n";
        let without = "line\n";
        assert_eq!(
            compute_file_hash(with_bom, 6),
            compute_file_hash(without, 6)
        );
    }
}
