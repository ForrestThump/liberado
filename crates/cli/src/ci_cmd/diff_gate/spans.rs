//! Function line spans and their intersection with a diff.
//!
//! cargo-crap reports the line where a function starts. The end line is the matching closing
//! brace of the body, or the semicolon when the item has no body. Strings (including raw
//! strings), comments (including nested block comments), character literals, lifetimes, and
//! raw identifiers are not braces.

use std::collections::BTreeSet;

pub fn intersects(start: u32, end: u32, lines: &BTreeSet<u32>) -> bool {
    let end = end.max(start);
    lines.range(start..=end).next().is_some()
}

pub fn function_end_line(source: &str, start_line: u32) -> u32 {
    let bytes = source.as_bytes();
    let mut index = skip_to_line(bytes, start_line);
    let mut line = start_line.max(1);
    let mut depth = 0i32;
    let mut opened = false;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            line = line.saturating_add(1);
            index += 1;
            continue;
        }
        if bytes[index] == b'\r' {
            index += 1;
            continue;
        }
        if let Some(next) = skip_trivia(bytes, index) {
            line = line_after(bytes, index, next, line);
            index = next;
            continue;
        }
        match bytes[index] {
            b'{' => {
                depth += 1;
                opened = true;
            }
            b'}' if opened => {
                depth -= 1;
                if depth == 0 {
                    return line;
                }
            }
            b';' if !opened && depth == 0 => return line,
            _ => {}
        }
        index += 1;
    }
    line
}

fn line_after(bytes: &[u8], from: usize, to: usize, mut line: u32) -> u32 {
    let end = to.min(bytes.len());
    let mut cursor = from;
    while cursor < end {
        if bytes[cursor] == b'\n' {
            line = line.saturating_add(1);
        }
        cursor += 1;
    }
    line
}

fn skip_to_line(bytes: &[u8], start_line: u32) -> usize {
    let mut line = 1u32;
    let mut index = 0usize;
    while index < bytes.len() && line < start_line {
        if bytes[index] == b'\n' {
            line += 1;
        }
        index += 1;
    }
    index
}

fn skip_trivia(bytes: &[u8], index: usize) -> Option<usize> {
    let byte = bytes[index];
    let next = bytes.get(index + 1).copied();
    if byte == b'/' && next == Some(b'/') {
        return Some(skip_line_comment(bytes, index));
    }
    if byte == b'/' && next == Some(b'*') {
        return Some(skip_block_comment(bytes, index));
    }
    if byte == b'\'' {
        return Some(skip_char_or_lifetime(bytes, index));
    }
    if let Some(end) = skip_prefixed_string(bytes, index) {
        return Some(end);
    }
    skip_raw_ident(bytes, index)
}

fn skip_prefixed_string(bytes: &[u8], index: usize) -> Option<usize> {
    if is_raw_string(bytes, index) {
        return Some(skip_raw_string(bytes, index));
    }
    let byte = bytes[index];
    if byte == b'"' {
        return Some(skip_string(bytes, index + 1));
    }
    if matches!(byte, b'b' | b'c') && bytes.get(index + 1) == Some(&b'"') {
        return Some(skip_string(bytes, index + 2));
    }
    None
}

fn skip_line_comment(bytes: &[u8], index: usize) -> usize {
    bytes
        .iter()
        .skip(index)
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| index + offset)
}

pub(crate) fn skip_block_comment(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index + 2;
    let mut depth = 1i32;
    while cursor + 1 < bytes.len() {
        if depth == 0 {
            return cursor;
        }
        cursor = step_block(bytes, cursor, &mut depth);
    }
    if depth == 0 { cursor } else { bytes.len() }
}

fn step_block(bytes: &[u8], cursor: usize, depth: &mut i32) -> usize {
    match block_mark(bytes, cursor) {
        BlockMark::Open => {
            *depth += 1;
            cursor + 2
        }
        BlockMark::Close => {
            *depth -= 1;
            cursor + 2
        }
        BlockMark::Other => cursor + 1,
    }
}

enum BlockMark {
    Open,
    Close,
    Other,
}

fn block_mark(bytes: &[u8], cursor: usize) -> BlockMark {
    let pair = [bytes[cursor], bytes.get(cursor + 1).copied().unwrap_or(0)];
    match pair {
        [b'/', b'*'] => BlockMark::Open,
        [b'*', b'/'] => BlockMark::Close,
        _ => BlockMark::Other,
    }
}

fn skip_char_or_lifetime(bytes: &[u8], index: usize) -> usize {
    match classify_tick(bytes, index) {
        Tick::End => bytes.len(),
        Tick::Lifetime => skip_ident(bytes, index + 1),
        Tick::Char => skip_char_body(bytes, index + 1),
    }
}

enum Tick {
    End,
    Lifetime,
    Char,
}

fn classify_tick(bytes: &[u8], index: usize) -> Tick {
    let Some(next) = bytes.get(index + 1).copied() else {
        return Tick::End;
    };
    if is_lifetime(bytes, index, next) {
        Tick::Lifetime
    } else {
        Tick::Char
    }
}

fn is_lifetime(bytes: &[u8], index: usize, next: u8) -> bool {
    next != b'\\' && ident_start(next) && bytes.get(index + 2) != Some(&b'\'')
}

fn ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn skip_char_body(bytes: &[u8], index: usize) -> usize {
    let cursor = if bytes.get(index) == Some(&b'\\') {
        skip_escape(bytes, index + 1)
    } else {
        index + 1
    };
    close_quote(bytes, cursor)
}

fn close_quote(bytes: &[u8], cursor: usize) -> usize {
    if bytes.get(cursor) == Some(&b'\'') {
        cursor + 1
    } else {
        cursor
    }
}

fn skip_escape(bytes: &[u8], index: usize) -> usize {
    if bytes.get(index) == Some(&b'u') {
        return skip_unicode(bytes, index + 1);
    }
    index + 1
}

fn skip_unicode(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index;
    if bytes.get(cursor) != Some(&b'{') {
        return cursor;
    }
    cursor += 1;
    while cursor < bytes.len() && bytes[cursor] != b'}' {
        cursor += 1;
    }
    if cursor < bytes.len() {
        cursor + 1
    } else {
        cursor
    }
}

fn skip_ident(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index;
    while cursor < bytes.len() && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_') {
        cursor += 1;
    }
    cursor
}

fn is_raw_string(bytes: &[u8], index: usize) -> bool {
    raw_string_body(strip_string_prefix(&bytes[index..]))
}

fn strip_string_prefix(rest: &[u8]) -> &[u8] {
    let rest = rest.strip_prefix(b"b").unwrap_or(rest);
    rest.strip_prefix(b"c").unwrap_or(rest)
}

fn raw_string_body(rest: &[u8]) -> bool {
    let Some(after_r) = rest.strip_prefix(b"r") else {
        return false;
    };
    let hashes = after_r.iter().take_while(|byte| **byte == b'#').count();
    after_r.get(hashes) == Some(&b'"')
}

fn skip_raw_ident(bytes: &[u8], index: usize) -> Option<usize> {
    if !bytes.get(index..).unwrap_or(&[]).starts_with(b"r#") {
        return None;
    }
    let start = bytes.get(index + 2).copied()?;
    ident_start(start).then_some(skip_ident(bytes, index + 2))
}

fn skip_raw_string(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index;
    if matches!(bytes.get(cursor), Some(b'b' | b'c')) {
        cursor += 1;
    }
    if bytes.get(cursor) == Some(&b'r') {
        cursor += 1;
    }
    let mut hashes = 0usize;
    while bytes.get(cursor) == Some(&b'#') {
        hashes += 1;
        cursor += 1;
    }
    if bytes.get(cursor) == Some(&b'"') {
        cursor += 1;
    }
    let closer = format!("\"{}", "#".repeat(hashes));
    let closer = closer.as_bytes();
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(closer) {
            return cursor + closer.len();
        }
        cursor += 1;
    }
    bytes.len()
}

fn skip_string(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor += 2,
            b'"' | b'\n' => return cursor + 1,
            _ => cursor += 1,
        }
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{function_end_line, intersects};

    #[test]
    fn a_span_hits_an_interior_line_and_misses_the_outside() {
        let lines = BTreeSet::from([4, 9]);
        assert!(intersects(3, 8, &lines));
        assert!(!intersects(10, 12, &lines));
        assert!(intersects(9, 9, &lines));
    }

    #[test]
    fn the_end_line_follows_braces_and_ignores_strings_comments_and_crlf() {
        let source = "fn outer() {\r\n    let s = \"{ not a brace\";\r\n    // {\r\n    if true { 1 } else { 2 }\r\n}\r\nfn next() {}\r\n";
        assert_eq!(function_end_line(source, 1), 5);
        assert_eq!(function_end_line(source, 6), 6);
    }

    #[test]
    fn a_signature_without_a_body_ends_at_the_semicolon() {
        let source = "trait T {\n    fn only(&self);\n}\n";
        assert_eq!(function_end_line(source, 2), 2);
    }

    #[test]
    fn raw_strings_lifetimes_raw_idents_and_nested_comments_are_not_braces() {
        let source = r###"
fn demo() {
    let a = r" { ";
    let b = r#" { "#;
    let c = br##" { "##;
    let e = 'a';
    let f = '\'';
    let g = '\u{1F600}';
    let h = Item::<'a, 'static>(r#type);
    /* { */
    /* nested /* { */ still { */
    1
}
"###;
        let start = line_no(source, "fn demo() {");
        let end = line_no(source, "}");
        assert_eq!(function_end_line(source, start), end);
    }

    #[test]
    fn an_unterminated_raw_string_and_block_comment_run_to_the_end() {
        let raw = "fn demo() {\n    let _ = r\"unterminated {\n}\nfn next() {}\n";
        assert_eq!(function_end_line(raw, 1), 5);
        let comment = b"/* nested /* { */ still {";
        assert_eq!(super::skip_block_comment(comment, 0), comment.len());
        let hashes = b"let _ = r#\"no end";
        let at = hashes.windows(2).position(|pair| pair == b"r#").unwrap();
        assert_eq!(super::skip_raw_string(hashes, at), hashes.len());
        assert_eq!(super::skip_char_or_lifetime(b"'", 0), 1);
        assert!(super::skip_raw_ident(b"r#type {", 0).is_some());
        assert!(super::skip_raw_ident(b"r#\"x\"#", 0).is_none());
    }

    fn line_no(source: &str, text: &str) -> u32 {
        source.lines().position(|line| line.trim() == text).unwrap() as u32 + 1
    }
}
