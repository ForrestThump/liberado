//! Serde default for `unmapped_bucket`.
//!
//! `weekly_spending.rs` is already past the function review boundary, so this
//! helper lives in its own file.

pub(super) fn unmapped_bucket() -> Option<String> {
    Some("Everything else".to_string())
}
