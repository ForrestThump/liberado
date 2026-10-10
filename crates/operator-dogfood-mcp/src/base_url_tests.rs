use super::*;

#[test]
fn unset_or_blank_uses_loopback_without_a_trailing_slash() {
    assert_eq!(resolve_api_base(None).unwrap(), "http://127.0.0.1:4201");
    assert_eq!(
        resolve_api_base(Some("  ")).unwrap(),
        "http://127.0.0.1:4201"
    );
    assert_eq!(
        resolve_api_base(Some("http://127.0.0.1:4201/")).unwrap(),
        "http://127.0.0.1:4201"
    );
}

#[test]
fn https_origin_and_ipv6_loopback_are_kept() {
    assert_eq!(
        resolve_api_base(Some("https://liberado.tailnet.ts.net:4201")).unwrap(),
        "https://liberado.tailnet.ts.net:4201"
    );
    assert_eq!(
        resolve_api_base(Some("http://[::1]:4201")).unwrap(),
        "http://[::1]:4201"
    );
}

#[test]
fn non_http_userinfo_path_and_query_are_rejected() {
    let file = resolve_api_base(Some("file:///tmp/daemon")).unwrap_err();
    assert!(file.to_string().contains("http or https"), "{file}");
    let user = resolve_api_base(Some("http://operator:secret@127.0.0.1:4201")).unwrap_err();
    assert!(user.to_string().contains("userinfo"), "{user}");
    let path = resolve_api_base(Some("http://127.0.0.1:4201/api")).unwrap_err();
    assert!(path.to_string().contains("origin"), "{path}");
    let query = resolve_api_base(Some("http://127.0.0.1:4201?token=1")).unwrap_err();
    assert!(query.to_string().contains("query"), "{query}");
}

#[test]
fn timeout_blank_is_the_default_and_zero_is_refused() {
    assert_eq!(timeout_secs(None).unwrap(), DEFAULT_TIMEOUT_SECS);
    assert_eq!(timeout_secs(Some("  ")).unwrap(), DEFAULT_TIMEOUT_SECS);
    assert_eq!(timeout_secs(Some("30")).unwrap(), 30);
    let zero = timeout_secs(Some("0")).unwrap_err();
    assert!(zero.to_string().contains(TIMEOUT_ENV), "{zero}");
    let text = timeout_secs(Some("soon")).unwrap_err();
    assert!(text.to_string().contains("whole number"), "{text}");
}

#[test]
fn connect_timeout_is_capped_at_ten_seconds() {
    assert_eq!(
        connect_timeout(std::time::Duration::from_secs(600)),
        std::time::Duration::from_secs(10)
    );
    assert_eq!(
        connect_timeout(std::time::Duration::from_secs(3)),
        std::time::Duration::from_secs(3)
    );
}
