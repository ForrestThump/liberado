//! Comment- and whitespace-normalized token Jaccard for renamed functions.
//!
//! The function's own simple name is removed before the comparison, so renaming `old_name`
//! to `new_name` does not by itself drop the score under the match threshold.

use std::collections::BTreeSet;

pub fn jaccard(left_body: &str, left_name: &str, right_body: &str, right_name: &str) -> f64 {
    let left = tokens(left_body, left_name);
    let right = tokens(right_body, right_name);
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let shared = left.intersection(&right).count() as f64;
    let union = left.union(&right).count() as f64;
    shared / union
}

fn tokens(body: &str, qualified: &str) -> BTreeSet<String> {
    let simple = simple_name(qualified);
    let stripped = strip_comments(body);
    stripped
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .filter(|token| !token.is_empty())
        .filter(|token| *token != simple)
        .map(str::to_string)
        .collect()
}

fn simple_name(qualified: &str) -> &str {
    qualified.rsplit("::").next().unwrap_or(qualified)
}

fn strip_comments(body: &str) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index = skip_until_newline(bytes, index);
            out.push(' ');
            continue;
        }
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index = skip_block(bytes, index);
            out.push(' ');
            continue;
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    out
}

fn skip_until_newline(bytes: &[u8], index: usize) -> usize {
    bytes
        .iter()
        .skip(index)
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| index + offset)
}

fn skip_block(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index + 2;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'*' && bytes[cursor + 1] == b'/' {
            return cursor + 2;
        }
        cursor += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::jaccard;

    #[test]
    fn a_rename_and_comments_still_clear_the_match_threshold() {
        let old = "fn old_name() {\n    // note\n    let value = 1;\n    value + 2\n}\n";
        let renamed = "fn new_name() {\n    let value = 1;\n    value + 2\n}\n";
        let score = jaccard(old, "old_name", renamed, "new_name");
        assert!(score >= 0.8, "similarity {score} should survive a rename");
    }

    #[test]
    fn unrelated_bodies_stay_below_the_match_threshold() {
        let left = "fn left() { alpha beta gamma delta epsilon }";
        let right = "fn right() { one two three four five six }";
        let score = jaccard(left, "left", right, "right");
        assert!(score < 0.8, "similarity {score} should not match");
    }
}
