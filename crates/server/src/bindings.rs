//! Persisted map from a chat peer to a Liberado session.
//!
//! One file, `<data_dir>/channel-bindings.json`, replaces the single
//! `<data_dir>/telegram-sticky-session` id. Telegram is one binding:
//! `(telegram, chat id, bot id) -> session`. On first load, a legacy sticky file is adopted
//! when that Telegram peer has no binding yet, so the chat keeps the same conversation.
//!
//! The registry is the only writer. Chat, cron delivery, and approval routing hold clones and
//! read the same map. A synchronous snapshot is published for the risk gate, which cannot await.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use liberado_common::{ChannelKind, SessionChannel};
use liberado_conversation_store::Ulid;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const FILE_VERSION: u32 = 1;

/// `(channel, external room or chat id, bot identity)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BindingKey {
    pub channel: ChannelKind,
    pub peer: String,
    pub bot: String,
}

impl BindingKey {
    pub fn telegram(peer: impl Into<String>, bot: impl Into<String>) -> Self {
        Self {
            channel: ChannelKind::Telegram,
            peer: peer.into(),
            bot: bot.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredBinding {
    channel: ChannelKind,
    peer: String,
    bot: String,
    session_id: String,
}

impl StoredBinding {
    // Inverse of a stored row. Tests look a session up; production routing reads the snapshot.
    #[allow(dead_code)]
    fn key(&self) -> BindingKey {
        BindingKey {
            channel: self.channel,
            peer: self.peer.clone(),
            bot: self.bot.clone(),
        }
    }

    fn matches(&self, key: &BindingKey) -> bool {
        self.channel == key.channel && self.peer == key.peer && self.bot == key.bot
    }

    fn session(&self) -> Option<Ulid> {
        self.session_id.parse().ok()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BindingFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    bindings: Vec<StoredBinding>,
}

struct BindingState {
    bindings: Vec<StoredBinding>,
    /// Legacy sticky id kept in the snapshot when Telegram env is unset, so that session still
    /// routes as the channel chat. Not written into the bindings file.
    restored_legacy: Option<Ulid>,
}

/// Legacy sticky file kept in step with one Telegram key so a cleared session is not re-adopted.
struct LegacySticky {
    path: PathBuf,
    key: BindingKey,
}

type SnapshotSlot = Arc<std::sync::Mutex<Vec<SessionChannel>>>;

/// Shared binding map. `Clone` is cheap. `path` is `None` for the in-memory handle tests use.
#[derive(Clone)]
pub struct ChannelBindings {
    inner: Arc<Mutex<BindingState>>,
    path: Option<Arc<PathBuf>>,
    legacy: Option<Arc<LegacySticky>>,
    mirror: Arc<std::sync::Mutex<Option<SnapshotSlot>>>,
}

/// Where to load the map from, and which Telegram peer may adopt the legacy sticky file.
pub struct BindingLoad {
    pub path: PathBuf,
    pub legacy_path: PathBuf,
    pub adopt_key: Option<BindingKey>,
}

impl ChannelBindings {
    /// In-memory only. Chat disabled, and tests.
    pub fn ephemeral() -> Self {
        Self::from_parts(BindingState::empty(), None, None)
    }

    /// Load `path`. A missing or unreadable file starts empty.
    ///
    /// When `adopt_key` is set and that peer has no binding, a valid id in `legacy_path` becomes
    /// the binding. When `adopt_key` is unset, a valid legacy id is only published (Telegram env
    /// is off, so there is no peer to store) and the legacy file is left in place.
    pub async fn load<F, Fut>(load: BindingLoad, is_valid: F) -> Self
    where
        F: Fn(Ulid) -> Fut,
        Fut: Future<Output = bool>,
    {
        let mut state = read_state(&load.path).await;
        let read_count = state.bindings.len();
        state.bindings = retain_valid(state.bindings, &is_valid).await;
        let dropped = state.bindings.len() != read_count;
        let legacy = load.adopt_key.clone().map(|key| {
            Arc::new(LegacySticky {
                path: load.legacy_path.clone(),
                key,
            })
        });
        let adopted = adopt_legacy(&mut state, &load, &is_valid).await;
        let bindings = Self::from_parts(state, Some(load.path), legacy);
        if adopted || dropped {
            bindings.persist_current().await;
        }
        bindings
    }

    fn from_parts(
        state: BindingState,
        path: Option<PathBuf>,
        legacy: Option<Arc<LegacySticky>>,
    ) -> Self {
        let snapshot = state.snapshot();
        let bindings = Self {
            inner: Arc::new(Mutex::new(state)),
            path: path.map(Arc::new),
            legacy,
            mirror: Arc::new(std::sync::Mutex::new(None)),
        };
        bindings.publish(&snapshot);
        bindings
    }

    /// Session for this peer, if one is stored.
    pub async fn get(&self, key: &BindingKey) -> Option<Ulid> {
        self.inner
            .lock()
            .await
            .bindings
            .iter()
            .find(|binding| binding.matches(key))
            .and_then(StoredBinding::session)
    }

    /// Peer bound to this session, if the session was stored under a key.
    ///
    /// The risk gate reads the published snapshot. This lookup is the other direction, used by
    /// tests and by a later channel that needs the peer for a session id.
    #[allow(dead_code)]
    pub async fn key_for_session(&self, session: Ulid) -> Option<BindingKey> {
        let label = session.to_string();
        self.inner
            .lock()
            .await
            .bindings
            .iter()
            .find(|binding| binding.session_id == label)
            .map(StoredBinding::key)
    }

    /// Channel that owns `session`, including a legacy id published without a peer key.
    ///
    /// Same reason as [`Self::key_for_session`]: the snapshot is what the risk gate reads.
    #[allow(dead_code)]
    pub async fn channel_of(&self, session: Ulid) -> Option<ChannelKind> {
        let state = self.inner.lock().await;
        let label = session.to_string();
        if let Some(binding) = state
            .bindings
            .iter()
            .find(|binding| binding.session_id == label)
        {
            return Some(binding.channel);
        }
        state
            .restored_legacy
            .filter(|id| *id == session)
            .map(|_| ChannelKind::Telegram)
    }

    /// Session that also shows background cards. The first Telegram binding, else the restored
    /// legacy id. One Telegram peer is the only binding this process writes today.
    pub async fn background_session(&self) -> Option<Ulid> {
        let state = self.inner.lock().await;
        let stored = state
            .bindings
            .iter()
            .find(|binding| binding.channel == ChannelKind::Telegram)
            .and_then(StoredBinding::session);
        stored.or(state.restored_legacy)
    }

    /// Set or clear the session for `key`. `None` (a `/new`) drops the binding.
    pub async fn set(&self, key: &BindingKey, id: Option<Ulid>) {
        let snapshot = {
            let mut state = self.inner.lock().await;
            if !state.assign(key, id) {
                return;
            }
            let snapshot = state.snapshot();
            self.persist(&state).await;
            self.mirror_legacy(key, id).await;
            snapshot
        };
        self.publish(&snapshot);
    }

    /// Return the stored id, or run `create` once and store it. The lock is held across `create`
    /// so a cron brief and a chat message cannot each open a session.
    pub async fn get_or_create<F, Fut, E>(&self, key: &BindingKey, create: F) -> Result<Ulid, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Ulid, E>>,
    {
        let mut state = self.inner.lock().await;
        if let Some(id) = state
            .bindings
            .iter()
            .find(|binding| binding.matches(key))
            .and_then(StoredBinding::session)
        {
            return Ok(id);
        }
        let id = create().await?;
        state.assign(key, Some(id));
        let snapshot = state.snapshot();
        self.persist(&state).await;
        self.mirror_legacy(key, Some(id)).await;
        drop(state);
        self.publish(&snapshot);
        Ok(id)
    }

    /// Point chat's synchronous slot at this map and copy the current snapshot into it.
    pub async fn publish_to(&self, slot: SnapshotSlot) {
        let snapshot = self.inner.lock().await.snapshot();
        {
            let mut guard = self
                .mirror
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = Some(Arc::clone(&slot));
        }
        if let Ok(mut dest) = slot.lock() {
            *dest = snapshot;
        }
    }

    fn publish(&self, snapshot: &[SessionChannel]) {
        let dest = self
            .mirror
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let Some(dest) = dest else {
            return;
        };
        if let Ok(mut guard) = dest.lock() {
            *guard = snapshot.to_vec();
        }
    }

    async fn persist_current(&self) {
        let state = self.inner.lock().await;
        self.persist(&state).await;
    }

    async fn persist(&self, state: &BindingState) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let file = BindingFile {
            version: FILE_VERSION,
            bindings: state.bindings.clone(),
        };
        let body = match serde_json::to_string_pretty(&file) {
            Ok(body) => body,
            Err(error) => {
                tracing::warn!(%error, "could not encode channel bindings");
                return;
            }
        };
        if let Err(error) = atomic_write(path.as_path(), &body).await {
            tracing::warn!(%error, path = %path.display(), "could not persist channel bindings");
        }
    }

    async fn mirror_legacy(&self, key: &BindingKey, id: Option<Ulid>) {
        let Some(legacy) = &self.legacy else {
            return;
        };
        if legacy.key != *key {
            return;
        }
        let result = match id {
            Some(id) => tokio::fs::write(&legacy.path, id.to_string()).await,
            None => match tokio::fs::remove_file(&legacy.path).await {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        };
        if let Err(error) = result {
            tracing::warn!(
                %error,
                path = %legacy.path.display(),
                "could not update the legacy sticky session file"
            );
        }
    }
}

impl BindingState {
    fn empty() -> Self {
        Self {
            bindings: Vec::new(),
            restored_legacy: None,
        }
    }

    fn snapshot(&self) -> Vec<SessionChannel> {
        let mut rows: Vec<SessionChannel> = self
            .bindings
            .iter()
            .map(|binding| SessionChannel {
                session_id: binding.session_id.clone(),
                channel: binding.channel,
            })
            .collect();
        if let Some(id) = self.restored_legacy {
            let session_id = id.to_string();
            if !rows.iter().any(|row| row.session_id == session_id) {
                rows.push(SessionChannel {
                    session_id,
                    channel: ChannelKind::Telegram,
                });
            }
        }
        rows
    }

    /// Returns whether the map changed.
    fn assign(&mut self, key: &BindingKey, id: Option<Ulid>) -> bool {
        let position = self
            .bindings
            .iter()
            .position(|binding| binding.matches(key));
        match (position, id) {
            (Some(index), Some(id)) if self.bindings[index].session_id == id.to_string() => false,
            (None, None) => false,
            (Some(index), None) => {
                self.bindings.remove(index);
                true
            }
            (Some(index), Some(id)) => {
                self.bindings[index].session_id = id.to_string();
                true
            }
            (None, Some(id)) => {
                self.bindings.push(StoredBinding {
                    channel: key.channel,
                    peer: key.peer.clone(),
                    bot: key.bot.clone(),
                    session_id: id.to_string(),
                });
                true
            }
        }
    }
}

async fn read_state(path: &Path) -> BindingState {
    let Ok(text) = tokio::fs::read_to_string(path).await else {
        return BindingState::empty();
    };
    match serde_json::from_str::<BindingFile>(&text) {
        Ok(file) => BindingState {
            bindings: file.bindings,
            restored_legacy: None,
        },
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "unreadable channel bindings; starting empty");
            BindingState::empty()
        }
    }
}

async fn retain_valid<F, Fut>(bindings: Vec<StoredBinding>, is_valid: &F) -> Vec<StoredBinding>
where
    F: Fn(Ulid) -> Fut,
    Fut: Future<Output = bool>,
{
    let mut kept = Vec::new();
    for binding in bindings {
        if let Some(id) = binding.session()
            && is_valid(id).await
        {
            kept.push(binding);
        } else {
            tracing::info!(
                session = %binding.session_id,
                channel = binding.channel.as_str(),
                "dropping channel binding whose session is gone"
            );
        }
    }
    kept
}

async fn adopt_legacy<F, Fut>(state: &mut BindingState, load: &BindingLoad, is_valid: &F) -> bool
where
    F: Fn(Ulid) -> Fut,
    Fut: Future<Output = bool>,
{
    let Some(id) = read_legacy_id(&load.legacy_path, is_valid).await else {
        return false;
    };
    if let Some(key) = &load.adopt_key {
        if state.bindings.iter().any(|binding| binding.matches(key)) {
            return false;
        }
        tracing::info!(%id, "adopted legacy telegram sticky session into channel bindings");
        state.assign(key, Some(id));
        return true;
    }
    state.restored_legacy = Some(id);
    tracing::info!(%id, "restored legacy telegram sticky session for routing");
    false
}

async fn read_legacy_id<F, Fut>(path: &Path, is_valid: &F) -> Option<Ulid>
where
    F: Fn(Ulid) -> Fut,
    Fut: Future<Output = bool>,
{
    let text = tokio::fs::read_to_string(path).await.ok()?;
    let id = text.trim().parse::<Ulid>().ok()?;
    if is_valid(id).await { Some(id) } else { None }
}

async fn atomic_write(path: &Path, body: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = tmp_path(path);
    tokio::fs::write(&tmp, body).await?;
    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(error) => replace_existing(&tmp, path, error).await,
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

async fn replace_existing(tmp: &Path, path: &Path, first: std::io::Error) -> std::io::Result<()> {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        let _ = tokio::fs::remove_file(tmp).await;
        return Err(first);
    }
    tokio::fs::rename(tmp, path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("liberado-bindings-{tag}-{}", Ulid::new()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn load_in(dir: &Path, adopt: Option<BindingKey>) -> BindingLoad {
        BindingLoad {
            path: dir.join("channel-bindings.json"),
            legacy_path: dir.join("telegram-sticky-session"),
            adopt_key: adopt,
        }
    }

    async fn always_valid(_id: Ulid) -> bool {
        true
    }

    #[tokio::test]
    async fn set_then_load_roundtrips_and_looks_up_both_ways() {
        let dir = temp_dir("roundtrip");
        let key = BindingKey::telegram("chat-1", "bot-9");
        let id = Ulid::new();
        {
            let bindings =
                ChannelBindings::load(load_in(&dir, Some(key.clone())), always_valid).await;
            assert_eq!(bindings.get(&key).await, None);
            bindings.set(&key, Some(id)).await;
        }
        let reloaded = ChannelBindings::load(load_in(&dir, Some(key.clone())), always_valid).await;
        assert_eq!(reloaded.get(&key).await, Some(id));
        assert_eq!(reloaded.key_for_session(id).await, Some(key));
        assert_eq!(reloaded.channel_of(id).await, Some(ChannelKind::Telegram));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_stale_binding_is_dropped_on_load() {
        let dir = temp_dir("stale");
        let key = BindingKey::telegram("chat-1", "bot-9");
        let id = Ulid::new();
        ChannelBindings::load(load_in(&dir, Some(key.clone())), always_valid)
            .await
            .set(&key, Some(id))
            .await;
        let reloaded =
            ChannelBindings::load(load_in(&dir, Some(key.clone())), |_| async { false }).await;
        assert_eq!(reloaded.get(&key).await, None);
        assert_eq!(reloaded.key_for_session(id).await, None);
        let text = std::fs::read_to_string(dir.join("channel-bindings.json")).unwrap();
        assert!(
            !text.contains(&id.to_string()),
            "a dropped binding must be removed from the file, got {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn legacy_sticky_file_is_adopted_once() {
        let dir = temp_dir("legacy");
        let key = BindingKey::telegram("chat-7", "42");
        let id = Ulid::new();
        std::fs::write(dir.join("telegram-sticky-session"), id.to_string()).unwrap();
        let loaded = ChannelBindings::load(load_in(&dir, Some(key.clone())), always_valid).await;
        assert_eq!(loaded.get(&key).await, Some(id));
        assert_eq!(loaded.background_session().await, Some(id));

        let other = Ulid::new();
        std::fs::write(dir.join("telegram-sticky-session"), other.to_string()).unwrap();
        let again = ChannelBindings::load(load_in(&dir, Some(key.clone())), always_valid).await;
        assert_eq!(again.get(&key).await, Some(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn clearing_a_binding_removes_the_legacy_file() {
        let dir = temp_dir("clear");
        let key = BindingKey::telegram("chat-7", "42");
        let id = Ulid::new();
        std::fs::write(dir.join("telegram-sticky-session"), id.to_string()).unwrap();
        let bindings = ChannelBindings::load(load_in(&dir, Some(key.clone())), always_valid).await;
        bindings.set(&key, None).await;
        assert_eq!(bindings.get(&key).await, None);
        assert!(!dir.join("telegram-sticky-session").exists());
        let reloaded = ChannelBindings::load(load_in(&dir, Some(key.clone())), always_valid).await;
        assert_eq!(reloaded.get(&key).await, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_invalid_legacy_id_is_not_adopted() {
        let dir = temp_dir("bad-legacy");
        let key = BindingKey::telegram("chat-7", "42");
        let id = Ulid::new();
        std::fs::write(dir.join("telegram-sticky-session"), id.to_string()).unwrap();
        let loaded =
            ChannelBindings::load(load_in(&dir, Some(key.clone())), |_| async { false }).await;
        assert_eq!(loaded.get(&key).await, None);
        assert!(dir.join("telegram-sticky-session").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn without_a_peer_the_legacy_id_still_routes() {
        let dir = temp_dir("no-peer");
        let id = Ulid::new();
        std::fs::write(dir.join("telegram-sticky-session"), id.to_string()).unwrap();
        let loaded = ChannelBindings::load(load_in(&dir, None), always_valid).await;
        assert_eq!(loaded.channel_of(id).await, Some(ChannelKind::Telegram));
        assert_eq!(loaded.background_session().await, Some(id));
        assert_eq!(loaded.key_for_session(id).await, None);
        assert!(!dir.join("channel-bindings.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn get_or_create_persists_and_reuses() {
        let dir = temp_dir("create");
        let key = BindingKey::telegram("chat-1", "bot-9");
        let bindings = ChannelBindings::load(load_in(&dir, Some(key.clone())), always_valid).await;
        let made = Ulid::new();
        let id = bindings
            .get_or_create::<_, _, std::convert::Infallible>(&key, || async { Ok(made) })
            .await
            .unwrap();
        assert_eq!(id, made);
        let again = bindings
            .get_or_create::<_, _, std::convert::Infallible>(&key, || async {
                panic!("must not create a second session");
            })
            .await
            .unwrap();
        assert_eq!(again, made);
        let reloaded = ChannelBindings::load(load_in(&dir, Some(key.clone())), always_valid).await;
        assert_eq!(reloaded.get(&key).await, Some(made));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn peers_do_not_share_a_session() {
        let bindings = ChannelBindings::ephemeral();
        let first = BindingKey::telegram("chat-1", "bot-9");
        let second = BindingKey::telegram("chat-2", "bot-9");
        let a = Ulid::new();
        let b = Ulid::new();
        bindings.set(&first, Some(a)).await;
        bindings.set(&second, Some(b)).await;
        assert_eq!(bindings.get(&first).await, Some(a));
        assert_eq!(bindings.get(&second).await, Some(b));
        assert_eq!(bindings.key_for_session(b).await.unwrap().peer, "chat-2");
    }
}
