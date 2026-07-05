//! The standardized event payload (`life-os-architecture.md` §5, Decision 6).
//!
//! One shape used by **both** trigger paths: (a) vault changes surfaced by the daemon's
//! Turbovault subscription (the daemon fills in `provenance` after central hash-join
//! attribution), and (b) non-vault triggers (systemd timers, git/docker/homelab hooks) that
//! POST this JSON directly to a hook webhook. Keeping `event_type` and `source` as strings is
//! deliberate: any hook-capable system must be able to mint a valid event without linking our
//! crates.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::provenance::WriteProvenance;

/// The always-present dispatcher/executor pool name (Decision 18 checkpoint #3) — every daemon has
/// this one even when no additional pools are configured, matching the single-dispatcher behavior
/// that predates pools exactly. Shared here (rather than duplicated per-crate) since both
/// `liberado-config-loader` (validating pool references) and `liberado-daemon` (routing events to
/// a pool) need the identical literal.
pub const DEFAULT_POOL: &str = "default";

/// Well-known [`Event::source`] values. Free-form by design — these are the ones the system
/// produces or recognizes; external hooks may use others.
pub mod event_source {
    pub const TURBOVAULT_SUBSCRIPTION: &str = "turbovault-subscription";
    pub const SYSTEMD_TIMER: &str = "systemd-timer";
    pub const GIT_HOOK: &str = "git-hook";
    pub const DOCKER_EVENT: &str = "docker-event";
    /// A cron-scheduled trigger (`liberado-cron`'s `CronEventSource`). Individual schedules use
    /// `"cron:{name}"` as the full `Event::source`; this is the shared prefix consumers match on.
    pub const CRON: &str = "cron";
    /// An external webhook trigger (`liberado-server`'s `POST /api/hooks/{name}`). Individual hooks
    /// use `"webhook:{name}"` as the full `Event::source`; this is the shared prefix consumers
    /// match on — mirrors [`CRON`]'s convention exactly (both carry their goal in
    /// `payload.summary`, not a vault path).
    pub const WEBHOOK: &str = "webhook";
}

/// Something that produces [`Event`]s for the daemon to react to (Decision 18/19 — the seam that
/// makes the vault a plugin rather than a hard dependency). Vault-watch and cron both implement
/// this; the daemon fans every attached source into one channel and reacts uniformly, never
/// knowing or caring how a given event was produced.
///
/// A fan-in-to-one-channel shape (not a polled `next_event()`) is deliberate: each source owns its
/// own internal timing/looping and is `tokio::spawn`ed independently against a shared
/// `UnboundedSender<Event>`, which composes cleanly for a heterogeneous, possibly-growing set of
/// sources without a dynamic `select!` over differently-shaped futures.
#[async_trait]
pub trait EventSource: Send + Sync {
    /// A short, human-readable name for logging (e.g. `"vault-watch"`, `"cron"`).
    fn name(&self) -> &str;

    /// Run until this source is closed/exhausted, pushing every reactable event onto `tx`.
    /// Returns when there is nothing left to produce (the caller's `while let Some(event) =
    /// rx.recv().await` loop ends once every attached source's `run` has returned and every
    /// sender clone is dropped).
    async fn run(self: Box<Self>, tx: UnboundedSender<Event>);
}

/// A trigger event delivered to a hook (or routed internally by the daemon).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Domain event name, e.g. `"DecisionLogged"`, `"InboxNoteSettled"`.
    pub event_type: String,

    pub timestamp: DateTime<Utc>,

    /// Origin of the event; see [`event_source`] for well-known values.
    pub source: String,

    /// Idempotency + loop-breaking key. Required on every event (Decision 6 — handlers are
    /// idempotent by construction, keyed on this).
    pub correlation_id: String,

    /// Write attribution for vault-originated events, filled in by the daemon's central
    /// attribution layer. `None` for non-vault triggers (timers/hooks have no provenance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<WriteProvenance>,

    pub payload: EventPayload,
}

/// The body of an [`Event`]. `path`/`summary` cover the common vault-change case; `data`
/// carries anything domain-specific without changing the envelope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventPayload {
    /// Vault path the event concerns, if any (e.g. `"decisions/2026-06-21/example.md"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Short high-signal excerpt or metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    /// Which named dispatcher/executor pool (Decision 18 checkpoint #3) should handle this event
    /// — set by the producer (e.g. a cron `Schedule` or webhook `HookConfig`'s own configured
    /// `pool`). `None` routes to the always-present `"default"` pool — vault-watch events never
    /// set this, so this is a zero-behavior-change addition for anyone not opting into pools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,

    /// Arbitrary structured payload for domain-specific events.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
}

impl Event {
    /// Construct a non-vault trigger event (no provenance), stamped `now`.
    pub fn trigger(
        event_type: impl Into<String>,
        source: impl Into<String>,
        correlation_id: impl Into<String>,
        payload: EventPayload,
    ) -> Self {
        Self {
            event_type: event_type.into(),
            timestamp: Utc::now(),
            source: source.into(),
            correlation_id: correlation_id.into(),
            provenance: None,
            payload,
        }
    }

    /// Whether this event should be reacted to: vault events whose provenance says an agent
    /// authored them are suppressed (loop-breaking); everything else (human/external/no
    /// provenance) is reactable. The hash-join that *establishes* provenance lives in the
    /// daemon; this is the final predicate over an already-attributed event.
    pub fn is_reactable(&self) -> bool {
        match &self.provenance {
            Some(p) => p.is_human(),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_vault_trigger_is_reactable() {
        let ev = Event::trigger(
            "NightlySweep",
            event_source::SYSTEMD_TIMER,
            "sweep-2026-06-21",
            EventPayload::default(),
        );
        assert!(ev.provenance.is_none());
        assert!(ev.is_reactable());
    }

    #[test]
    fn agent_authored_vault_event_is_suppressed() {
        let mut ev = Event::trigger(
            "ReviewWritten",
            event_source::TURBOVAULT_SUBSCRIPTION,
            "review-1",
            EventPayload {
                path: Some("reviews/2026-06-21.md".into()),
                ..Default::default()
            },
        );
        ev.provenance = Some(WriteProvenance::agent("daily-review-agent", "review-1"));
        assert!(!ev.is_reactable());
    }

    #[test]
    fn serializes_to_expected_shape() {
        let ev = Event::trigger(
            "DecisionLogged",
            event_source::TURBOVAULT_SUBSCRIPTION,
            "review-2026-06-21",
            EventPayload {
                path: Some("decisions/2026-06-21/example.md".into()),
                summary: Some("Short excerpt".into()),
                ..Default::default()
            },
        );
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["event_type"], "DecisionLogged");
        assert_eq!(json["source"], "turbovault-subscription");
        assert_eq!(json["payload"]["path"], "decisions/2026-06-21/example.md");
        // provenance + null data are skipped when empty
        assert!(json.get("provenance").is_none());
        assert!(json["payload"].get("data").is_none());
    }
}
