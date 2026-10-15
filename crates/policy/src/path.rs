//! Vault path normalizer.
//!
//! A vault path is a relative `zone/rest` string. Separators become `/`.
//! `.` and empty segments drop. The zone is the first remaining segment.
//! The rest of the path is required.

use crate::segment::{
    ZoneClass, classify_zone, has_ascii_control, is_parent_segment, path_has_drive,
    path_is_absolute,
};

/// A normalized vault path. The zone keeps the caller's case when it matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VaultPath {
    zone: String,
    rest: String,
}

impl VaultPath {
    /// Leading zone segment.
    pub fn zone(&self) -> &str {
        &self.zone
    }

    /// Path under the zone, with `/` separators.
    pub fn rest(&self) -> &str {
        &self.rest
    }

    /// `zone/rest`.
    pub fn as_relative(&self) -> String {
        format!("{}/{}", self.zone, self.rest)
    }
}

/// Why a vault path was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The input is empty.
    #[error("vault path is empty")]
    Empty,
    /// The path starts with `/` or `\`.
    #[error("vault path is absolute")]
    Absolute,
    /// A segment contains `:`.
    #[error("vault path contains a drive letter")]
    DriveLetter,
    /// A segment is `..`.
    #[error("vault path contains a parent segment")]
    ParentSegment,
    /// The path contains an ASCII control character.
    #[error("vault path contains a control character")]
    ControlCharacter,
    /// The path has no `zone/rest` shape.
    #[error("vault path names no zone")]
    NoZone,
    /// The zone matches a declaration only by ASCII case.
    #[error("zone {found:?} does not match declared zone {expected:?}")]
    ZoneCaseMismatch { found: String, expected: String },
    /// The zone is outside the declared list.
    #[error("zone {found:?} is not a declared zone")]
    ZoneNotDeclared { found: String },
    /// A declared zone name is not one segment.
    #[error("zone name is not a single path segment")]
    BadZone,
}

/// Normalize `raw` against `declared_zones`.
///
/// An empty `declared_zones` accepts any well-formed zone and keeps its case.
pub fn normalize_vault_path(raw: &str, declared_zones: &[String]) -> Result<VaultPath, PathError> {
    if raw.is_empty() {
        return Err(PathError::Empty);
    }
    if has_ascii_control(raw) {
        return Err(PathError::ControlCharacter);
    }
    if path_is_absolute(raw) {
        return Err(PathError::Absolute);
    }
    if path_has_drive(raw) {
        return Err(PathError::DriveLetter);
    }
    let segments = collect_segments(raw)?;
    assemble(segments, declared_zones)
}

/// A declared zone is one segment: no slash, drive, dot, or control character.
pub(crate) fn validate_zone_name(name: &str) -> Result<(), PathError> {
    if name.is_empty() {
        return Err(PathError::Empty);
    }
    if has_ascii_control(name) || name_is_bad_segment(name) {
        return Err(PathError::BadZone);
    }
    Ok(())
}

fn name_is_bad_segment(name: &str) -> bool {
    name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.chars().any(|c| c.is_whitespace())
        || name == "."
        || name == ".."
}

fn collect_segments(raw: &str) -> Result<Vec<&str>, PathError> {
    let mut out = Vec::new();
    for segment in raw.split(['/', '\\']) {
        if segment.is_empty() || segment.trim() == "." {
            continue;
        }
        if is_parent_segment(segment) {
            return Err(PathError::ParentSegment);
        }
        out.push(segment);
    }
    Ok(out)
}

fn assemble(segments: Vec<&str>, declared: &[String]) -> Result<VaultPath, PathError> {
    let Some((zone, rest)) = segments.split_first() else {
        return Err(PathError::NoZone);
    };
    if rest.is_empty() {
        return Err(PathError::NoZone);
    }
    confirm_zone(zone, declared)?;
    Ok(VaultPath {
        zone: (*zone).to_owned(),
        rest: rest.join("/"),
    })
}

fn confirm_zone(zone: &str, declared: &[String]) -> Result<(), PathError> {
    match classify_zone(zone, declared) {
        ZoneClass::Open | ZoneClass::Exact => Ok(()),
        ZoneClass::CaseMismatch { expected } => Err(PathError::ZoneCaseMismatch {
            found: zone.to_owned(),
            expected,
        }),
        ZoneClass::NotDeclared => Err(PathError::ZoneNotDeclared {
            found: zone.to_owned(),
        }),
    }
}

#[cfg(test)]
#[path = "path_tests.rs"]
mod path_tests;
