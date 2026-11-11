//! Session minting, lookup, revocation, and cookie parsing.

use super::*;

#[test]
fn a_new_session_is_looked_up_by_its_id_and_yields_its_csrf_token() {
    let store = SessionStore::new();
    let (id, csrf) = store.create();
    assert_eq!(store.touch(&id).as_deref(), Some(csrf.as_str()));
}

#[test]
fn the_session_id_and_the_csrf_token_are_different_values() {
    let store = SessionStore::new();
    let (id, csrf) = store.create();
    assert_ne!(
        id, csrf,
        "the readable CSRF token must not be the value that authenticates"
    );
}

#[test]
fn two_sessions_never_share_an_id() {
    let store = SessionStore::new();
    let (first, _) = store.create();
    let (second, _) = store.create();
    assert_ne!(first, second);
    assert_eq!(first.len(), 64, "256 bits, hex-encoded");
}

#[test]
fn an_unknown_session_id_is_not_a_session() {
    let store = SessionStore::new();
    store.create();
    assert_eq!(store.touch("not-a-session"), None);
}

#[test]
fn revoking_stops_the_cookie_working_not_only_the_browser_holding_it() {
    let store = SessionStore::new();
    let (id, _) = store.create();
    store.revoke(&id);
    assert_eq!(store.touch(&id), None);
}

#[test]
fn a_cookie_header_yields_the_named_value_and_only_that_one() {
    let header = "theme=dark; liberado_session=abc123; liberado_csrf=def456";
    assert_eq!(cookie_value(header, SESSION_COOKIE), Some("abc123"));
    assert_eq!(cookie_value(header, CSRF_COOKIE), Some("def456"));
    assert_eq!(cookie_value(header, "absent"), None);
}

#[test]
fn a_cookie_value_may_contain_an_equals_sign() {
    assert_eq!(
        cookie_value("liberado_session=YWJj=ZA==", SESSION_COOKIE),
        Some("YWJj=ZA==")
    );
}

/// `liberado_session_other=x` must not answer a lookup for `liberado_session`.
#[test]
fn a_cookie_name_is_matched_whole() {
    assert_eq!(
        cookie_value("liberado_session_other=x", SESSION_COOKIE),
        None
    );
}
