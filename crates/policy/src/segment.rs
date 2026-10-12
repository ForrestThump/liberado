//! Shared path-segment checks.
//!
//! Vault paths and URLs both refuse a parent segment, a drive letter, a
//! control character, and a zone whose case does not match the declaration.

/// ASCII control characters, including NUL and DEL.
pub(crate) fn has_ascii_control(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_control())
}

/// A segment that walks to the parent directory. Surrounding spaces count.
pub(crate) fn is_parent_segment(segment: &str) -> bool {
    segment.trim() == ".."
}

/// A leading `/` or `\` makes a vault path absolute.
pub(crate) fn path_is_absolute(raw: &str) -> bool {
    raw.starts_with('/') || raw.starts_with('\\')
}

/// A `:` inside a segment is a drive letter (`C:` or `C:\`).
pub(crate) fn segment_has_drive(segment: &str) -> bool {
    segment.contains(':')
}

/// Any vault-path segment contains a drive letter.
pub(crate) fn path_has_drive(raw: &str) -> bool {
    raw.split(['/', '\\']).any(segment_has_drive)
}

/// `C:` at the start of a string. Used for URL inputs that are file paths.
pub(crate) fn looks_like_drive_prefix(raw: &str) -> bool {
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let Some(second) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic() && second == ':'
}

/// How a zone name compares with the declared list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ZoneClass {
    /// No declared zones. Any well-formed zone is accepted. Case is kept.
    Open,
    /// The zone matches a declaration exactly.
    Exact,
    /// The zone matches a declaration only when ASCII case is ignored.
    CaseMismatch { expected: String },
    /// The zone is not in the declaration.
    NotDeclared,
}

/// Compare `found` with `declared`. An empty list is open.
pub(crate) fn classify_zone(found: &str, declared: &[String]) -> ZoneClass {
    if declared.is_empty() {
        return ZoneClass::Open;
    }
    if declared.iter().any(|zone| zone == found) {
        return ZoneClass::Exact;
    }
    if let Some(expected) = declared
        .iter()
        .find(|zone| zone.eq_ignore_ascii_case(found))
    {
        return ZoneClass::CaseMismatch {
            expected: expected.clone(),
        };
    }
    ZoneClass::NotDeclared
}
