//! The persisted sticky Telegram session id: the one conversation a cron brief folds into and a
//! typed reply continues. It lived only in memory, so every container restart reset it — an implicit
//! `/new` that dropped the running thread. Persisting the id to a small file on the data volume lets
//! the same conversation survive a restart. Design: `docs/future-work/ideas/cron-delivery-timing-idea.md`.
//!
//! One type owns both the in-memory cell and its write-through to disk, so the two cannot drift: the
//! chat bridge, the approval bot, and the cron-delivery notifier all hold clones of the *same*
//! `StickySession`, and every mutation persists. That "one place, cannot half-happen" shape is the
//! same lesson `sessions_dir()` was extracted for.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use liberado_conversation_store::Ulid;
use tokio::sync::Mutex;

/// The sticky Telegram conversation id, shared (cheap `Clone`) and persisted. `path` is `None` for an
/// in-memory-only handle (no data dir, and tests); otherwise every change writes through to it.
#[derive(Clone)]
pub struct StickySession {
    inner: Arc<Mutex<Option<Ulid>>>,
    path: Option<Arc<PathBuf>>,
}

impl StickySession {
    /// In-memory only — no persistence. Used when chat is disabled (no sticky to keep) and in tests.
    pub fn ephemeral() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            path: None,
        }
    }

    /// Load the persisted id from `path`, adopting it only if `is_valid` confirms the conversation
    /// still exists. A pointer to a conversation that's gone (store wiped, or a stale file) is
    /// discarded rather than adopted, so we never append a brief into a ghost session — the next
    /// message just lazily creates a fresh one. A missing/empty/garbage file loads as `None`.
    pub async fn load<F, Fut>(path: PathBuf, is_valid: F) -> Self
    where
        F: FnOnce(Ulid) -> Fut,
        Fut: Future<Output = bool>,
    {
        let initial = match tokio::fs::read_to_string(&path).await {
            Ok(contents) => Self::restore_persisted_id(&path, &contents, is_valid).await,
            Err(_) => None, // no file yet — first run
        };
        Self {
            inner: Arc::new(Mutex::new(initial)),
            path: Some(Arc::new(path)),
        }
    }

    /// Parse the persisted sticky id and drop it when it fails validation. A stale or
    /// unparsable file is a fresh start, not an error.
    async fn restore_persisted_id<F, Fut>(path: &Path, contents: &str, is_valid: F) -> Option<Ulid>
    where
        F: FnOnce(Ulid) -> Fut,
        Fut: Future<Output = bool>,
    {
        match contents.trim().parse::<Ulid>() {
            Ok(id) if is_valid(id).await => {
                tracing::info!(%id, "restored sticky Telegram session from disk");
                Some(id)
            }
            Ok(id) => {
                tracing::info!(
                    %id,
                    "persisted sticky Telegram session no longer exists; starting fresh"
                );
                None
            }
            Err(_) => {
                tracing::warn!(
                    path = %path.display(),
                    "unparsable sticky session file; ignoring"
                );
                None
            }
        }
    }

    /// The current sticky id, if any.
    pub async fn get(&self) -> Option<Ulid> {
        *self.inner.lock().await
    }

    /// Set or clear the id, writing through to disk. `None` (a `/new`) removes the file so a restart
    /// doesn't resurrect a cleared session. A no-op change skips the write.
    pub async fn set(&self, id: Option<Ulid>) {
        let mut guard = self.inner.lock().await;
        if *guard == id {
            return;
        }
        *guard = id;
        self.persist(id).await;
    }

    /// Return the current id, or run `create` to make one — storing and persisting it. The lock is
    /// held across `create` so two concurrent callers (a cron brief and a chat message racing to open
    /// the first Telegram conversation) can't each create one; the loser sees the winner's id.
    pub async fn get_or_create<F, Fut, E>(&self, create: F) -> Result<Ulid, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Ulid, E>>,
    {
        let mut guard = self.inner.lock().await;
        if let Some(id) = *guard {
            return Ok(id);
        }
        let id = create().await?;
        *guard = Some(id);
        self.persist(Some(id)).await;
        Ok(id)
    }

    /// Write-through, called while the in-memory lock is held so disk can't disagree with memory.
    /// Best-effort: a failed write is logged, not fatal (the in-memory value still holds for this
    /// run; only cross-restart survival is lost).
    async fn persist(&self, id: Option<Ulid>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let result = match id {
            Some(id) => {
                if let Some(parent) = path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                tokio::fs::write(path.as_path(), id.to_string()).await
            }
            None => match tokio::fs::remove_file(path.as_path()).await {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, path = %path.display(), "could not persist sticky Telegram session id");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("liberado-sticky-test-{tag}-{}", Ulid::new()))
    }

    #[tokio::test]
    async fn set_then_load_roundtrips() {
        let path = temp_path("roundtrip");
        let id = Ulid::new();
        {
            let s = StickySession::load(path.clone(), |_| async { true }).await;
            assert_eq!(s.get().await, None); // nothing on disk yet
            s.set(Some(id)).await;
        }
        // A fresh load from the same path adopts the persisted id (validator says it still exists).
        let reloaded = StickySession::load(path.clone(), |_| async { true }).await;
        assert_eq!(reloaded.get().await, Some(id));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn stale_id_is_discarded_when_invalid() {
        let path = temp_path("stale");
        let id = Ulid::new();
        StickySession::load(path.clone(), |_| async { true })
            .await
            .set(Some(id))
            .await;
        // The conversation no longer exists → validator returns false → not adopted.
        let reloaded = StickySession::load(path.clone(), |_| async { false }).await;
        assert_eq!(reloaded.get().await, None);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn clearing_removes_the_file() {
        let path = temp_path("clear");
        let s = StickySession::load(path.clone(), |_| async { true }).await;
        s.set(Some(Ulid::new())).await;
        assert!(path.exists());
        s.set(None).await;
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn get_or_create_persists_and_reuses() {
        let path = temp_path("create");
        let s = StickySession::load(path.clone(), |_| async { true }).await;
        let made = Ulid::new();
        let id = s
            .get_or_create::<_, _, std::convert::Infallible>(|| async { Ok(made) })
            .await
            .unwrap();
        assert_eq!(id, made);
        // Second call must NOT create again — it returns the stored id and never runs the closure.
        let again = s
            .get_or_create::<_, _, std::convert::Infallible>(|| async {
                panic!("must not create a second session");
            })
            .await
            .unwrap();
        assert_eq!(again, made);
        // And it's on disk.
        let reloaded = StickySession::load(path.clone(), |_| async { true }).await;
        assert_eq!(reloaded.get().await, Some(made));
        let _ = std::fs::remove_file(&path);
    }
}
