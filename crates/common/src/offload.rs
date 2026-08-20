//! Shared tool result offload utilities.
//!
//! Provides a unified way to handle oversized tool results: when output exceeds a byte
//! threshold, write the full body to a spill directory and return a head+tail preview
//! that tells the model where to read the rest.
//!
//! Two entry points:
//! - [`spill_text`] — for callers that already have a decoded UTF-8 string (e.g., executor).
//! - [`spill_bytes`] — for callers with raw command output bytes that may need UTF-16
//!   decoding (e.g., coder-sandbox).

use std::path::{Path, PathBuf};

/// Configuration for offload behavior.
#[derive(Debug, Clone)]
pub struct OffloadConfig {
    /// Maximum bytes before offloading. Results under this pass through unchanged.
    pub max_bytes: usize,
    /// Directory to write spill files. If `None`, truncation degrades to head-only.
    pub spill_dir: Option<PathBuf>,
    /// Prefix for spill file names.
    pub file_prefix: String,
}

impl Default for OffloadConfig {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024,
            spill_dir: None,
            file_prefix: "tool-spill".to_string(),
        }
    }
}

/// Result of an offload operation.
#[derive(Debug, Clone)]
pub struct OffloadResult {
    /// The text to return to the model (either full text or head+tail preview).
    pub text: String,
    /// Path to the spill file, if offload occurred.
    pub spill_path: Option<PathBuf>,
}

/// Sanitize a label for use in a spill filename.
pub fn sanitize_label(label: &str) -> String {
    let mut out: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if out.is_empty() {
        out.push_str("call");
    }
    out
}

/// Find the nearest valid UTF-8 character boundary at or before `idx`.
fn char_boundary_at_or_before(text: &str, mut idx: usize) -> usize {
    idx = idx.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Truncate `text` to at most `max` bytes, respecting UTF-8 char boundaries.
pub fn truncate_head(text: &str, max: usize) -> String {
    let end = char_boundary_at_or_before(text, max);
    text[..end].to_string()
}

/// Generate a head+tail preview of `text` totaling approximately `max_bytes`.
///
/// The preview shows the first half and last half of the content with a truncation
/// marker in between. If the text is too short to split, falls back to head-only.
pub fn head_tail_preview(text: &str, max_bytes: usize) -> String {
    let head = max_bytes / 2;
    let tail = max_bytes - head;
    let head_end = char_boundary_at_or_before(text, head);
    let tail_start = char_boundary_at_or_before(text, text.len().saturating_sub(tail));
    if head_end >= tail_start {
        return truncate_head(text, max_bytes);
    }
    format!(
        "{}\n\n… [output truncated to {max_bytes} bytes of {}; middle omitted] …\n\n{}",
        &text[..head_end],
        text.len(),
        &text[tail_start..],
    )
}

/// Decode command output bytes, handling UTF-16 LE (with or without BOM) and UTF-8.
///
/// Windows PowerShell and some cmd builtins emit UTF-16 LE. This function detects
/// and decodes it correctly, stripping NUL bytes that would otherwise corrupt
/// the model's transcript.
pub fn decode_command_bytes(buf: &[u8]) -> String {
    if buf.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16_units(&buf[2..], u16::from_le_bytes);
    }
    if buf.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16_units(&buf[2..], u16::from_be_bytes);
    }
    if looks_like_utf16_le(buf) {
        return decode_utf16_units(buf, u16::from_le_bytes);
    }
    String::from_utf8_lossy(buf).into_owned()
}

fn looks_like_utf16_le(buf: &[u8]) -> bool {
    if buf.len() < 4 || !buf.len().is_multiple_of(2) {
        return false;
    }
    let pairs = buf.len() / 2;
    let high_nul = buf.chunks_exact(2).filter(|c| c[1] == 0).count();
    high_nul * 2 >= pairs
}

fn decode_utf16_units(buf: &[u8], from_bytes: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| from_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Spill an oversized **pre-decoded text** result.
///
/// If `text.len() <= config.max_bytes`, returns it unchanged with no spill path.
/// Otherwise, writes the full text to a file in `config.spill_dir` (if set) and
/// returns a head+tail preview. If the spill dir is not set or write fails,
/// degrades to head-only truncation.
pub fn spill_text(text: &str, config: &OffloadConfig) -> OffloadResult {
    if text.len() <= config.max_bytes {
        return OffloadResult {
            text: text.to_string(),
            spill_path: None,
        };
    }
    let Some(spill_dir) = &config.spill_dir else {
        return OffloadResult {
            text: truncate_head(text, config.max_bytes),
            spill_path: None,
        };
    };
    let file_name = format!("{}-{}.txt", config.file_prefix, sanitize_label("result"));
    let path = spill_dir.join(&file_name);
    if std::fs::create_dir_all(spill_dir).is_err() || std::fs::write(&path, text).is_err() {
        return OffloadResult {
            text: truncate_head(text, config.max_bytes),
            spill_path: None,
        };
    }
    OffloadResult {
        text: head_tail_preview(text, config.max_bytes),
        spill_path: Some(path),
    }
}

/// Spill an oversized **raw bytes** result (e.g., command stdout/stderr).
///
/// Decodes the bytes (handling UTF-16 LE from Windows PowerShell), then applies
/// the same logic as [`spill_text`]. The `label` is used to generate a unique
/// filename for this specific tool call.
pub fn spill_bytes(buf: &[u8], config: &OffloadConfig, label: &str) -> OffloadResult {
    let text = decode_command_bytes(buf);
    if text.len() <= config.max_bytes {
        return OffloadResult {
            text,
            spill_path: None,
        };
    }
    let Some(spill_dir) = &config.spill_dir else {
        return OffloadResult {
            text: truncate_head(&text, config.max_bytes),
            spill_path: None,
        };
    };
    let file_name = format!("{}-{}.txt", config.file_prefix, sanitize_label(label));
    let path = spill_dir.join(&file_name);
    if std::fs::create_dir_all(spill_dir).is_err() || std::fs::write(&path, &text).is_err() {
        return OffloadResult {
            text: truncate_head(&text, config.max_bytes),
            spill_path: None,
        };
    }
    OffloadResult {
        text: head_tail_preview(&text, config.max_bytes),
        spill_path: Some(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn spill_text_under_threshold_passes_through() {
        let config = OffloadConfig {
            max_bytes: 1024,
            spill_dir: None,
            ..Default::default()
        };
        let result = spill_text("hello", &config);
        assert_eq!(result.text, "hello");
        assert!(result.spill_path.is_none());
    }

    #[test]
    fn spill_text_over_threshold_without_dir_truncates() {
        let config = OffloadConfig {
            max_bytes: 10,
            spill_dir: None,
            ..Default::default()
        };
        let result = spill_text("hello world", &config);
        assert_eq!(result.text.len(), 10);
        assert!(result.spill_path.is_none());
    }

    #[test]
    fn spill_text_writes_file_and_returns_preview() {
        let dir = tempdir().unwrap();
        let config = OffloadConfig {
            max_bytes: 20,
            spill_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let long = "0123456789abcdefghijklmnop"; // 26 chars > 20 bytes
        let result = spill_text(long, &config);
        assert!(result.spill_path.is_some());
        let written = std::fs::read_to_string(result.spill_path.unwrap()).unwrap();
        assert_eq!(written, long);
        assert!(result.text.contains("truncated"));
        assert!(result.text.starts_with("01234"));
        assert!(result.text.ends_with("klmnop"));
    }

    #[test]
    fn spill_bytes_decodes_utf16_le() {
        let mut utf16 = Vec::new();
        for unit in "Windows PowerShell".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        let config = OffloadConfig {
            max_bytes: 1024,
            spill_dir: None,
            ..Default::default()
        };
        let result = spill_bytes(&utf16, &config, "test");
        assert_eq!(result.text, "Windows PowerShell");
        assert!(!result.text.contains('\0'));
    }

    #[test]
    fn spill_bytes_decodes_utf16_le_bom() {
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in "hi".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        let config = OffloadConfig {
            max_bytes: 1024,
            spill_dir: None,
            ..Default::default()
        };
        let result = spill_bytes(&utf16, &config, "test");
        assert_eq!(result.text, "hi");
    }

    #[test]
    fn spill_bytes_keeps_utf8() {
        let config = OffloadConfig {
            max_bytes: 1024,
            spill_dir: None,
            ..Default::default()
        };
        let result = spill_bytes("café".as_bytes(), &config, "test");
        assert_eq!(result.text, "café");
    }

    #[test]
    fn sanitize_label_replaces_special_chars() {
        assert_eq!(sanitize_label("hello world"), "hello_world");
        assert_eq!(sanitize_label("path/to/file"), "path_to_file");
        assert_eq!(sanitize_label(""), "call");
    }

    #[test]
    fn head_tail_preview_short_text_is_head_only() {
        let text = "short";
        let preview = head_tail_preview(text, 100);
        assert_eq!(preview, "short");
    }

    #[test]
    fn head_tail_preview_splits_long_text() {
        let text = "0123456789abcdefghijklmnopqrstuvwxyz";
        let preview = head_tail_preview(text, 20);
        assert!(preview.contains("truncated"));
        assert!(preview.starts_with("01234"));
        assert!(preview.ends_with("uvwxyz"));
    }

    #[test]
    fn truncate_head_respects_utf8_boundaries() {
        // "café" = 'c','a','f','é' where 'é' is 2 bytes = 5 bytes total
        let text = "café";
        assert_eq!(truncate_head(text, 3), "caf"); // 3 bytes = 'c','a','f'
        assert_eq!(truncate_head(text, 4), "caf"); // 4 bytes = still 'c','a','f' (é is 2 bytes)
        assert_eq!(truncate_head(text, 5), "café"); // 5 bytes = full
    }
}