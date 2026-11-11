//! In-memory WebAuthn ceremony state (single-use, five-minute TTL).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use webauthn_rs::prelude::{PasskeyAuthentication, PasskeyRegistration};

const TTL: Duration = Duration::from_secs(5 * 60);

pub enum CeremonyKind {
    Register(PasskeyRegistration),
    Login(PasskeyAuthentication),
}

struct Entry {
    kind: CeremonyKind,
    expires: Instant,
}

pub struct ChallengeStore {
    entries: Mutex<HashMap<String, Entry>>,
    now: fn() -> Instant,
}

impl Default for ChallengeStore {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            now: Instant::now,
        }
    }
}

impl ChallengeStore {
    pub fn insert(&self, ceremony_id: String, kind: CeremonyKind) {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut map, (self.now)());
        map.insert(
            ceremony_id,
            Entry {
                kind,
                expires: (self.now)() + TTL,
            },
        );
    }

    pub fn take_register(&self, ceremony_id: &str) -> Option<PasskeyRegistration> {
        self.take(ceremony_id, |kind| match kind {
            CeremonyKind::Register(r) => Some(r),
            CeremonyKind::Login(_) => None,
        })
    }

    pub fn take_login(&self, ceremony_id: &str) -> Option<PasskeyAuthentication> {
        self.take(ceremony_id, |kind| match kind {
            CeremonyKind::Login(a) => Some(a),
            CeremonyKind::Register(_) => None,
        })
    }

    fn take<T, F>(&self, ceremony_id: &str, pick: F) -> Option<T>
    where
        F: FnOnce(CeremonyKind) -> Option<T>,
    {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut map, (self.now)());
        let entry = map.remove(ceremony_id)?;
        if entry.expires <= (self.now)() {
            return None;
        }
        pick(entry.kind)
    }
}

fn prune(map: &mut HashMap<String, Entry>, now: Instant) {
    map.retain(|_, e| e.expires > now);
}
