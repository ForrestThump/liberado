//! Function line spans and their intersection with a diff.
//!
//! cargo-crap reports the line where a function starts. The end line is the matching closing
//! brace of the body, or the semicolon when the item has no body. Strings, comments, character
//! literals, and lifetimes are not braces.

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
    if let Some(next) = skip_prefixed_string(bytes, index) {
        return Some(next);
    }
    None
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

fn skip_block_comment(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index + 2;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'*' && bytes[cursor + 1] == b'/' {
            return cursor + 2;
        }
        cursor += 1;
    }
    bytes.len()
}

fn skip_char_or_lifetime(bytes: &[u8], index: usize) -> usize {
    let Some(next) = bytes.get(index + 1).copied() else {
        return bytes.len();
    };
    if next.is_ascii_alphabetic() || next == b'_' {
        return skip_ident(bytes, index + 1);
    }
    let mut cursor = index + 1;
    if next == b'\\' {
        cursor += 2;
    } else {
        cursor += 1;
    }
    if bytes.get(cursor) == Some(&b'\'') {
        cursor += 1;
    }
    cursor
}

fn skip_ident(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index;
    while cursor < bytes.len() && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_') {
        cursor += 1;
    }
    cursor
}

fn is_raw_string(bytes: &[u8], index: usize) -> bool {
    let rest = &bytes[index..];
    let rest = rest.strip_prefix(b"b").unwrap_or(rest);
    let rest = rest.strip_prefix(b"c").unwrap_or(rest);
    rest.starts_with(b"r\"") || rest.starts_with(b"r#")
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
}
