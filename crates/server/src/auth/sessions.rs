//! Browser sessions for the owner: the cookie the WebUI gets in exchange for the human token.
//!
//! In memory, not on disk. A restart signs Forrest out, which costs one paste and removes a
//! file that would otherwise hold a replayable owner credential on the same disk as the vault.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rand::RngCore;

/// Cookie the browser sends back. `HttpOnly`, so page script cannot read it.
pub const SESSION_COOKIE: &str = "liberado_session";

/// Header the WebUI echoes the CSRF token in on a mutating request.
pub const CSRF_HEADER: &str = "x-liberado-csrf";

/// Readable cookie carrying the CSRF token, so a page reload can recover it without another
/// round trip. It authenticates nothing on its own.
pub const CSRF_COOKIE: &str = "liberado_csrf";

/// How long a session lives without being used.
const SESSION_IDLE_TTL: Duration = Duration::from_secs(14 * 24 * 60 * 60);

struct Session {
    csrf: String,
    last_seen: Instant,
}

/// Live owner sessions, keyed by the opaque id in the cookie.
pub struct SessionStore {
    sessions: Mutex<HashMap<String, Session>>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Mint a session. Returns `(session id, csrf token)` — the id goes in an `HttpOnly` cookie,
    /// the CSRF token goes in the login response body and a readable cookie.
    ///
    /// Both are 256 bits from the OS. The CSRF token is separate from the id so that the value
    /// page script must be able to read is not the value that authenticates.
    pub fn create(&self) -> (String, String) {
        let id = random_token();
        let csrf = random_token();
        let mut sessions = self.lock();
        prune(&mut sessions);
        sessions.insert(
            id.clone(),
            Session {
                csrf: csrf.clone(),
                last_seen: Instant::now(),
            },
        );
        (id, csrf)
    }

    /// The CSRF token for a live session, refreshing its idle clock. `None` if unknown or
    /// expired.
    pub fn touch(&self, id: &str) -> Option<String> {
        let mut sessions = self.lock();
        prune(&mut sessions);
        let session = sessions.get_mut(id)?;
        session.last_seen = Instant::now();
        Some(session.csrf.clone())
    }

    /// Drop a session. Signing out must stop the cookie working, not only remove it from the
    /// browser — a cookie already copied elsewhere is exactly the case that matters.
    pub fn revoke(&self, id: &str) {
        self.lock().remove(id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        // A poisoned lock here means a panic while holding a `HashMap`; the map is still
        // structurally sound and refusing every login afterwards is the worse outcome.
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn prune(sessions: &mut HashMap<String, Session>) {
    let now = Instant::now();
    sessions.retain(|_, s| now.duration_since(s.last_seen) < SESSION_IDLE_TTL);
}

/// 256 bits from the OS CSPRNG, hex-encoded.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The value of `name` in a `Cookie:` header, or `None`.
///
/// Hand-parsed rather than pulled in as a dependency: the grammar is `name=value` pairs split
/// on `;`, and the only subtlety is that a value may itself contain `=`.
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then_some(value.trim())
    })
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
