//! Process-wide serialization for git worktree registry mutations.
//!
//! Git updates `.git/worktrees/` through several commands, and those updates are not atomic.
//! Every async sandbox path that adds, removes, or prunes worktrees must use this guard. The
//! original failures were an add racing a prune on Windows and a remove racing an add on Ubuntu.
//!
//! A single global lock is intentional. Registry commands take milliseconds, while the coding
//! work happens after the guard is released. A keyed lock would add state without useful
//! throughput.

static WORKTREE_REGISTRY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn lock() -> tokio::sync::MutexGuard<'static, ()> {
    WORKTREE_REGISTRY.lock().await
}

/// Test probe kept separate from [`lock`] so deleting a guard is an observable mutation.
#[cfg(test)]
pub(crate) fn enter_probe() -> ConcurrencyProbe {
    ConcurrencyProbe::enter()
}

#[cfg(test)]
pub(crate) static PEAK_IN_REGISTRY: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
static CONCURRENT_IN_REGISTRY: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Counts entries into registry-mutating sections, including an entry whose lock was removed.
#[cfg(test)]
pub(crate) struct ConcurrencyProbe;

#[cfg(test)]
impl ConcurrencyProbe {
    fn enter() -> Self {
        let now = CONCURRENT_IN_REGISTRY.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        PEAK_IN_REGISTRY.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

#[cfg(test)]
impl Drop for ConcurrencyProbe {
    fn drop(&mut self) {
        CONCURRENT_IN_REGISTRY.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
