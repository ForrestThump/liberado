use sha2::{Digest, Sha256};

/// Stable reminder identity. A trailing `^blockid` wins. Otherwise the first 16 hex
/// characters of SHA-256 of the trimmed line. The line number is not part of the key.
pub fn content_id(line: &str) -> String {
    if let Some(block) = block_id(line) {
        return block;
    }
    hex16(&Sha256::digest(line.trim().as_bytes()))
}

pub fn task_key(kind: &str, path: &str, label: &str, when: &str, identity: &str) -> String {
    format!("remind/{kind}/{path}/{label}/{when}/{identity}")
}

pub fn fanout_key(base: &str, channel: &str, target: &str) -> String {
    format!("{base}/{channel}/{target}")
}

fn block_id(line: &str) -> Option<String> {
    let token = line.split_whitespace().last()?;
    let rest = token.strip_prefix('^')?;
    if rest.is_empty() {
        return None;
    }
    if rest
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        Some(rest.to_string())
    } else {
        None
    }
}

fn hex16(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(16);
    for byte in bytes.iter().take(8) {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_inserted_above_does_not_change_the_key() {
        let line = "- [ ] call home 📅 2026-10-29 09:00 ^callhome";
        let key = task_key("task", "Life/a.md", "due", "2026-10-29T09:00", &content_id(line));
        let again = task_key(
            "task",
            "Life/a.md",
            "due",
            "2026-10-29T09:00",
            &content_id(line),
        );
        assert_eq!(key, again);
        assert!(key.ends_with("/callhome"));
        assert!(!key.contains("/line/"));
    }

    #[test]
    fn text_without_a_block_id_hashes_the_line() {
        let first = content_id("- [ ] alpha");
        let second = content_id("- [ ] beta");
        assert_ne!(first, second);
        assert_eq!(first.len(), 16);
    }
}
