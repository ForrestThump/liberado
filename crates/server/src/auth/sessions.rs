//! Browser sessions for the owner: persisted in `liberado.db`, hashed at rest.

use std::sync::Arc;

use chrono::Utc;
use liberado_outbox::Outbox;

/// Cookie the browser sends back. `HttpOnly`, so page script cannot read it.
pub const SESSION_COOKIE: &str = "liberado_session";

/// Header the WebUI echoes the CSRF token in on a mutating request.
pub const CSRF_HEADER: &str = "x-liberado-csrf";

/// Readable cookie carrying the CSRF token, so a page reload can recover it without another
/// round trip. It authenticates nothing on its own.
pub const CSRF_COOKIE: &str = "liberado_csrf";

/// Short-lived cookie binding a WebAuthn ceremony to this browser tab.
pub const CEREMONY_COOKIE: &str = "liberado_webauthn_ceremony";

/// A live session after idle and absolute expiry checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSession {
    pub csrf: String,
    pub account_id: i64,
    pub method: String,
    pub public_id: String,
    pub expires_at: String,
}

/// Owner browser sessions backed by SQLite.
pub struct SessionStore {
    outbox: Arc<Outbox>,
}

impl SessionStore {
    pub fn new(outbox: Arc<Outbox>) -> Self {
        Self { outbox }
    }

    /// Mint a session. Returns `(session id, csrf token)` for cookies.
    pub fn create(
        &self,
        account_id: i64,
        method: &str,
        user_agent: Option<&str>,
    ) -> Result<(String, String), liberado_outbox::Error> {
        self.outbox
            .create_auth_session(account_id, method, user_agent, Utc::now())
    }

    /// Refresh idle clock and return session metadata. `None` if unknown or expired.
    pub fn touch(&self, raw_id: &str) -> Option<LiveSession> {
        self.outbox.touch_auth_session(raw_id, Utc::now()).map(|r| LiveSession {
            csrf: r.csrf,
            account_id: r.account_id,
            method: r.method,
            public_id: r.public_id,
            expires_at: r.expires_at,
        })
    }

    pub fn revoke(&self, raw_id: &str) {
        let _ = self.outbox.revoke_auth_session(raw_id);
    }

    pub fn outbox(&self) -> &Arc<Outbox> {
        &self.outbox
    }
}

/// The value of `name` in a `Cookie:` header, or `None`.
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then_some(value.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SessionStore {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Arc::new(Outbox::open(&dir.path().join("liberado.db")).unwrap());
        SessionStore::new(outbox)
    }

    #[test]
    fn a_new_session_is_looked_up_by_its_id_and_yields_its_csrf_token() {
        let store = store();
        let (id, csrf) = store.create(1, "token", None).unwrap();
        assert_eq!(store.touch(&id).as_ref().map(|s| s.csrf.as_str()), Some(csrf.as_str()));
    }

    #[test]
    fn the_session_id_and_the_csrf_token_are_different_values() {
        let store = store();
        let (id, csrf) = store.create(1, "token", None).unwrap();
        assert_ne!(id, csrf);
    }

    #[test]
    fn revoking_stops_the_cookie_working() {
        let store = store();
        let (id, _) = store.create(1, "token", None).unwrap();
        store.revoke(&id);
        assert!(store.touch(&id).is_none());
    }

    #[test]
    fn a_cookie_header_yields_the_named_value() {
        let header = "theme=dark; liberado_session=abc123; liberado_csrf=def456";
        assert_eq!(cookie_value(header, SESSION_COOKIE), Some("abc123"));
    }
}
