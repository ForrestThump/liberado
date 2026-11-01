//! Session and ceremony cookie helpers shared by auth HTTP handlers.

use axum::http::{HeaderMap, header};
use chrono::{DateTime, Utc};

use super::sessions::{CSRF_COOKIE, SESSION_COOKIE};

pub fn session_max_age(expires_at: &str) -> i64 {
    let Ok(exp) = DateTime::parse_from_rfc3339(expires_at) else {
        return liberado_outbox::SESSION_ABSOLUTE.as_secs() as i64;
    };
    let remaining = exp.with_timezone(&Utc) - Utc::now();
    remaining.num_seconds().max(0)
}

pub fn set_session_cookies(headers: &mut HeaderMap, session_id: &str, csrf: &str, max_age: i64) {
    append_cookie(
        headers,
        &format!(
            "{SESSION_COOKIE}={session_id}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={max_age}"
        ),
    );
    append_cookie(
        headers,
        &format!("{CSRF_COOKIE}={csrf}; Secure; SameSite=Strict; Path=/; Max-Age={max_age}"),
    );
}

pub fn clear_cookie(headers: &mut HeaderMap, name: &str) {
    append_cookie(
        headers,
        &format!("{name}=; Secure; SameSite=Strict; Path=/; Max-Age=0"),
    );
    if name == SESSION_COOKIE {
        append_cookie(
            headers,
            &format!("{name}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0"),
        );
    }
}

pub fn append_cookie(headers: &mut HeaderMap, value: &str) {
    if let Ok(parsed) = value.parse() {
        headers.append(header::SET_COOKIE, parsed);
    }
}
