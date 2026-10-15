use super::*;

fn zones(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn https_url_folds_host_case() {
    let url = normalize_url("HTTPS://API.Example/v1/charges", &[]).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host(), "api.example");
    assert_eq!(url.port(), None);
    assert_eq!(url.path(), "/v1/charges");
}

#[test]
fn url_parent_segment_is_refused() {
    let error = normalize_url("https://example.com/tasks/../secret", &[]).unwrap_err();
    assert_eq!(error, UrlError::ParentSegment);
}

#[test]
fn url_encoded_parent_segment_is_refused() {
    let error = normalize_url("https://example.com/%2e%2e", &[]).unwrap_err();
    assert_eq!(error, UrlError::ParentSegment);
    let upper = normalize_url("https://example.com/%2E%2E", &[]).unwrap_err();
    assert_eq!(upper, UrlError::ParentSegment);
}

#[test]
fn url_encoded_separator_is_refused() {
    let error = normalize_url("https://example.com/tasks%2fsecret", &[]).unwrap_err();
    assert_eq!(error, UrlError::ParentSegment);
}

#[test]
fn url_absolute_filesystem_path_is_refused() {
    assert_eq!(normalize_url("/etc/passwd", &[]), Err(UrlError::Absolute));
    assert_eq!(
        normalize_url("\\\\server\\share\\a", &[]),
        Err(UrlError::Absolute)
    );
}

#[test]
fn url_drive_letter_is_refused() {
    assert_eq!(
        normalize_url("C:\\Windows\\note.md", &[]),
        Err(UrlError::DriveLetter)
    );
    assert_eq!(
        normalize_url("https://example.com/C:/Windows", &[]),
        Err(UrlError::DriveLetter)
    );
}

#[test]
fn url_control_character_is_refused() {
    assert_eq!(
        normalize_url("https://example.com/a\u{0000}", &[]),
        Err(UrlError::ControlCharacter)
    );
}

#[test]
fn url_zone_case_mismatch_is_refused() {
    let error = normalize_url("https://example.com/Tasks/a", &zones(&["tasks"])).unwrap_err();
    assert_eq!(
        error,
        UrlError::ZoneCaseMismatch {
            found: "Tasks".into(),
            expected: "tasks".into(),
        }
    );
}

#[test]
fn url_userinfo_is_refused() {
    let error = normalize_url("https://user:pass@example.com/a", &[]).unwrap_err();
    assert_eq!(error, UrlError::Userinfo);
}

#[test]
fn file_scheme_is_refused() {
    let error = normalize_url("file:///etc/passwd", &[]).unwrap_err();
    assert_eq!(error, UrlError::UnsupportedScheme);
}

#[test]
fn allowlist_covers_a_longer_path_on_a_folded_host() {
    let allow = normalize_url("https://API.Example/v1/", &[]).unwrap();
    let call = normalize_url("https://api.example/v1/charges", &[]).unwrap();
    assert!(allow.covers(&call));
    let other = normalize_url("https://api.example/v2/charges", &[]).unwrap();
    assert!(!allow.covers(&other));
}

#[test]
fn explicit_port_is_part_of_the_match() {
    let allow = normalize_url("https://example.com/v1", &[]).unwrap();
    let call = normalize_url("https://example.com:443/v1", &[]).unwrap();
    assert!(!allow.covers(&call));
}
