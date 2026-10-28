//! First-connect wait, and the hook the server uses to log the face tool count.
//!
//! The wait ends when every registered peer has a status row, or when the bound elapses.
//! A peer that is still inside `connect` keeps that attempt. The hook runs only after the
//! server arms it, and only when the MCP tool count changes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::futures::Notified;

use crate::connector::McpConnector;

use super::PeerSupervisor;

pub(super) type SurfaceHook = Arc<dyn Fn() + Send + Sync>;

impl PeerSupervisor {
    /// Block until each current peer has finished its first attempt, or until `bound` elapses.
    pub(crate) async fn wait_first_wave(&self, bound: Duration) {
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            if !self.wave_pending(deadline).await {
                return;
            }
        }
    }

    /// Remember the current MCP tool count and start delivering later changes to `hook`.
    pub(crate) fn arm_surface_hook(&self, hook: SurfaceHook) {
        let count = self.tool_count();
        *lock_last(&self.last_surface_count) = Some(count);
        *lock_hook(&self.surface_hook) = Some(hook);
    }

    pub(super) fn surface_change(&self, count: usize) -> Option<SurfaceHook> {
        let mut last = lock_last(&self.last_surface_count);
        if *last == Some(count) {
            return None;
        }
        *last = Some(count);
        drop(last);
        lock_hook(&self.surface_hook).clone()
    }

    pub(super) fn wake_first_wave(&self) {
        self.wave.notify_waiters();
    }

    async fn wave_pending(&self, deadline: tokio::time::Instant) -> bool {
        let notified = self.wave.notified();
        if self.first_wave_settled() {
            return false;
        }
        wait_or_deadline(deadline, notified).await
    }

    fn first_wave_settled(&self) -> bool {
        let Some(wanted) = self.connectors() else {
            return true;
        };
        self.each_peer_settled(&wanted)
    }

    fn each_peer_settled(&self, wanted: &HashMap<String, Arc<dyn McpConnector>>) -> bool {
        let data = self.lock();
        wanted.keys().all(|name| data.status.contains_key(name))
    }
}

pub(super) fn emit(hook: Option<SurfaceHook>) {
    if let Some(hook) = hook {
        hook();
    }
}

fn lock_last(slot: &Mutex<Option<usize>>) -> MutexGuard<'_, Option<usize>> {
    slot.lock().unwrap_or_else(|err| err.into_inner())
}

fn lock_hook(slot: &Mutex<Option<SurfaceHook>>) -> MutexGuard<'_, Option<SurfaceHook>> {
    slot.lock().unwrap_or_else(|err| err.into_inner())
}

async fn wait_or_deadline(deadline: tokio::time::Instant, notified: Notified<'_>) -> bool {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return false;
    }
    notified_before(remaining, notified).await
}

async fn notified_before(remaining: Duration, notified: Notified<'_>) -> bool {
    tokio::select! {
        biased;
        _ = notified => true,
        _ = tokio::time::sleep(remaining) => false,
    }
}
