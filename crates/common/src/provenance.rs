//! Write provenance (Decision 5, `liberado-vault-concurrency-spec.md` §4).
//!
//! Provenance rides on the **Turbovault audit log**, not note frontmatter (frontmatter is
//! last-writer-only state and goes stale the moment a human edits in Obsidian). Every agent
//! write attaches a [`WriteProvenance`] to the audit entry's `metadata` field via the
//! `write_*_with_metadata` SDK methods, under the reserved key [`PROVENANCE_KEY`]. Reactive
//! consumers read it back to attribute changes and break loops (hash-join, spec §6).
//!
//! Attribution is **best-effort, never a security boundary**: a missing or unrecognized
//! provenance always means "treat as external/unknown," never "trusted." Security is the
//! capability/zone model ([`crate::capability`]).

use serde::{Deserialize, Serialize};

/// Reserved key under which provenance is stored on `AuditEntry.metadata`.
///
/// Kept as a single constant so migrating to a typed upstream field — or a vendor-neutral
/// `_provenance` key — is a one-line change (concurrency spec §10 Q1).
pub const PROVENANCE_KEY: &str = "_liberado_provenance";

/// The `source` value marking a write as human-authored (vs one of our agents). Loop-breaking
/// suppresses only non-human sources; an unrecognized source is treated as external (reacted to).
pub const HUMAN_SOURCE: &str = "human";

/// Attribution recorded on every agent-originated vault write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteProvenance {
    /// Who/what performed the write, e.g. `"human"`, `"liberado-dispatcher"`, `"tasks-mcp"`,
    /// `"daily-review-agent"`. Free-form string for v1.
    pub source: String,

    /// Links this write to the task/decision/event that caused it. **Required** for any
    /// agent write — it is the root of the loop-breaking + idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,

    /// The zone the write targeted (lets consumers filter without re-deriving from path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,

    /// Optional free-form reason; also carries `parent_correlation_id` for cascade tracing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl WriteProvenance {
    /// A provenance for an agent write. `source` and `correlation_id` are the mandatory pair
    /// the daemon refuses to write without (spec §4.3).
    pub fn agent(source: impl Into<String>, correlation_id: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            correlation_id: Some(correlation_id.into()),
            zone: None,
            note: None,
        }
    }

    /// A provenance for a human-authored write (e.g. a Telegram approve/reject/revise action).
    /// Rides through the audit log like an agent write, but [`is_human`](Self::is_human) is what
    /// keeps it from being suppressed by loop-breaking — see this module's doc comment.
    pub fn human() -> Self {
        Self {
            source: HUMAN_SOURCE.into(),
            correlation_id: None,
            zone: None,
            note: None,
        }
    }

    /// Builder: set the target zone.
    pub fn with_zone(mut self, zone: impl Into<String>) -> Self {
        self.zone = Some(zone.into());
        self
    }

    /// Builder: set a free-form note.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Render as the `metadata` JSON to pass to Turbovault's `write_*_with_metadata`:
    /// `{ "_liberado_provenance": { ... } }`.
    pub fn to_audit_metadata(&self) -> serde_json::Value {
        serde_json::json!({ PROVENANCE_KEY: self })
    }

    /// Extract provenance from an audit entry's `metadata` JSON, if present and well-formed.
    /// A `None` result means "no recognizable provenance" → treat the change as external.
    pub fn from_audit_metadata(metadata: &serde_json::Value) -> Option<Self> {
        let inner = metadata.get(PROVENANCE_KEY)?;
        serde_json::from_value(inner.clone()).ok()
    }

    /// Whether this write was made by a human (vs one of our agents). Used by loop-breaking:
    /// a human-attributed (or unattributed) change is reacted to; an agent one is suppressed.
    pub fn is_human(&self) -> bool {
        self.source.eq_ignore_ascii_case(HUMAN_SOURCE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_audit_metadata() {
        let prov = WriteProvenance::agent("daily-review-agent", "review-2026-06-21")
            .with_zone("reviews")
            .with_note("nightly sweep");

        let meta = prov.to_audit_metadata();
        assert!(meta.get(PROVENANCE_KEY).is_some());

        let recovered = WriteProvenance::from_audit_metadata(&meta).expect("recover provenance");
        assert_eq!(recovered, prov);
        assert!(!recovered.is_human());
    }

    #[test]
    fn missing_provenance_is_none() {
        let meta = serde_json::json!({ "something_else": 1 });
        assert!(WriteProvenance::from_audit_metadata(&meta).is_none());
    }
}
