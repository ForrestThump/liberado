//! URL normalizer for tool declarations and calls.
//!
//! Only `http` and `https` are accepted. The host is stored in lower case.
//! The path drops `.` segments, refuses `..` (including a single percent-decode
//! of `%2e%2e`), refuses a drive-letter segment, and compares the first path
//! segment with the declared zones.

use crate::segment::{
    ZoneClass, classify_zone, has_ascii_control, is_parent_segment, looks_like_drive_prefix,
    segment_has_drive,
};

/// A normalized `http` or `https` URL. The path starts with `/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolUrl {
    scheme: String,
    host: String,
    port: Option<u16>,
    path: String,
}

impl ToolUrl {
    /// `http` or `https`.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Host in lower case.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Explicit port. A missing port stays missing.
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Absolute URL path. `/` is the root.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// `self` is an allowlist entry that covers `call`.
    pub fn covers(&self, call: &ToolUrl) -> bool {
        self.scheme == call.scheme
            && self.host == call.host
            && self.port == call.port
            && path_covers(&self.path, &call.path)
    }
}

/// Why a URL was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    /// The input is empty.
    #[error("url is empty")]
    Empty,
    /// The input contains an ASCII control character.
    #[error("url contains a control character")]
    ControlCharacter,
    /// The input is a filesystem absolute path.
    #[error("url is an absolute filesystem path")]
    Absolute,
    /// The input or a path segment contains a drive letter.
    #[error("url contains a drive letter")]
    DriveLetter,
    /// The scheme is not `http` or `https`.
    #[error("url scheme is not http or https")]
    UnsupportedScheme,
    /// The authority contains user info.
    #[error("url contains user info")]
    Userinfo,
    /// The host or port cannot be read.
    #[error("url authority is invalid")]
    BadAuthority,
    /// A percent-escape is incomplete or not UTF-8.
    #[error("url path encoding is invalid")]
    BadEncoding,
    /// A path segment is `..` or hides a separator.
    #[error("url path contains a parent segment")]
    ParentSegment,
    /// The path has no leading zone and the declaration requires one.
    #[error("url path names no zone")]
    ZoneMissing,
    /// The first path segment matches a zone only by ASCII case.
    #[error("url zone {found:?} does not match declared zone {expected:?}")]
    ZoneCaseMismatch { found: String, expected: String },
    /// The first path segment is outside the declared zones.
    #[error("url zone {found:?} is not a declared zone")]
    ZoneNotDeclared { found: String },
}

/// Normalize `raw` against `declared_zones`.
pub fn normalize_url(raw: &str, declared_zones: &[String]) -> Result<ToolUrl, UrlError> {
    if raw.is_empty() {
        return Err(UrlError::Empty);
    }
    if has_ascii_control(raw) {
        return Err(UrlError::ControlCharacter);
    }
    if let Some(error) = filesystem_reason(raw) {
        return Err(error);
    }
    let (scheme, rest) = split_scheme(raw)?;
    let (authority, path_and_query) = split_authority(rest)?;
    refuse_userinfo(authority)?;
    let (host, port) = split_host_port(authority)?;
    let path = normalize_url_path(&path_before_query(path_and_query), declared_zones)?;
    Ok(ToolUrl {
        scheme,
        host,
        port,
        path,
    })
}

fn filesystem_reason(raw: &str) -> Option<UrlError> {
    if looks_like_drive_prefix(raw) {
        return Some(UrlError::DriveLetter);
    }
    if raw.starts_with('/') || raw.starts_with('\\') || raw.contains('\\') {
        return Some(UrlError::Absolute);
    }
    None
}

fn split_scheme(raw: &str) -> Result<(String, &str), UrlError> {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return Err(UrlError::UnsupportedScheme);
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme == "http" || scheme == "https" {
        return Ok((scheme, rest));
    }
    Err(UrlError::UnsupportedScheme)
}

fn split_authority(rest: &str) -> Result<(&str, &str), UrlError> {
    match rest.split_once('/') {
        Some((authority, path)) => Ok((authority, path)),
        None => Ok((rest, "")),
    }
}

fn refuse_userinfo(authority: &str) -> Result<(), UrlError> {
    if authority.contains('@') {
        return Err(UrlError::Userinfo);
    }
    Ok(())
}

fn split_host_port(authority: &str) -> Result<(String, Option<u16>), UrlError> {
    if authority.is_empty() || authority.contains('[') {
        return Err(UrlError::BadAuthority);
    }
    let Some((host, port_text)) = authority.rsplit_once(':') else {
        return Ok((authority.to_ascii_lowercase(), None));
    };
    if host.is_empty() || port_text.is_empty() || host.contains(':') {
        return Err(UrlError::BadAuthority);
    }
    let port = port_text
        .parse::<u16>()
        .map_err(|_| UrlError::BadAuthority)?;
    Ok((host.to_ascii_lowercase(), Some(port)))
}

fn path_before_query(path_and_query: &str) -> String {
    let without_fragment = path_and_query
        .split_once('#')
        .unwrap_or((path_and_query, ""))
        .0;
    let path = without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""))
        .0;
    if path.is_empty() {
        "/".to_owned()
    } else {
        format!("/{path}")
    }
}

fn normalize_url_path(path: &str, zones: &[String]) -> Result<String, UrlError> {
    let mut out = String::from("/");
    let mut saw_segment = false;
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        let decoded = decode_segment(segment)?;
        reject_decoded(&decoded)?;
        if !saw_segment {
            confirm_url_zone(&decoded, zones)?;
            saw_segment = true;
        }
        push_segment(&mut out, &decoded);
    }
    if !saw_segment {
        confirm_absent_zone(zones)?;
    }
    Ok(out)
}

fn reject_decoded(decoded: &str) -> Result<(), UrlError> {
    if decoded.contains('/') || decoded.contains('\\') || is_parent_segment(decoded) {
        return Err(UrlError::ParentSegment);
    }
    if segment_has_drive(decoded) {
        return Err(UrlError::DriveLetter);
    }
    Ok(())
}

fn confirm_url_zone(zone: &str, declared: &[String]) -> Result<(), UrlError> {
    match classify_zone(zone, declared) {
        ZoneClass::Open | ZoneClass::Exact => Ok(()),
        ZoneClass::CaseMismatch { expected } => Err(UrlError::ZoneCaseMismatch {
            found: zone.to_owned(),
            expected,
        }),
        ZoneClass::NotDeclared => Err(UrlError::ZoneNotDeclared {
            found: zone.to_owned(),
        }),
    }
}

fn confirm_absent_zone(declared: &[String]) -> Result<(), UrlError> {
    if declared.is_empty() {
        Ok(())
    } else {
        Err(UrlError::ZoneMissing)
    }
}

fn push_segment(out: &mut String, decoded: &str) {
    if out != "/" {
        out.push('/');
    }
    out.push_str(decoded);
}

fn decode_segment(segment: &str) -> Result<String, UrlError> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        index = push_one(bytes, index, &mut out)?;
    }
    String::from_utf8(out).map_err(|_| UrlError::BadEncoding)
}

fn push_one(bytes: &[u8], index: usize, out: &mut Vec<u8>) -> Result<usize, UrlError> {
    if bytes[index] != b'%' {
        out.push(bytes[index]);
        return Ok(index + 1);
    }
    let value = hex_byte(bytes, index)?;
    if value.is_ascii_control() {
        return Err(UrlError::ControlCharacter);
    }
    out.push(value);
    Ok(index + 3)
}

fn hex_byte(bytes: &[u8], index: usize) -> Result<u8, UrlError> {
    let Some(pair) = bytes.get(index + 1..index + 3) else {
        return Err(UrlError::BadEncoding);
    };
    let text = std::str::from_utf8(pair).map_err(|_| UrlError::BadEncoding)?;
    u8::from_str_radix(text, 16).map_err(|_| UrlError::BadEncoding)
}

fn path_covers(allow: &str, call: &str) -> bool {
    if allow == "/" {
        return true;
    }
    let prefix = allow.strip_suffix('/').unwrap_or(allow);
    call == prefix || (call.starts_with(prefix) && call[prefix.len()..].starts_with('/'))
}

#[cfg(test)]
#[path = "url_tests.rs"]
mod url_tests;
