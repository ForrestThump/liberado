//! Consumer-side hash-join attribution — the loop-breaking primitive (Decision 5 §6).
//!
//! A filesystem change event is provenance-blind: identical whether Turbovault, Obsidian, or git
//! wrote the file. We attribute by **content identity**, not timing: hash the current file and
//! match it against the `after_hash` of a recent audit entry for that path. A match whose
//! provenance says "an agent did this" → suppress (it's ours). No match → the content was
//! produced by something outside our write path (a human in Obsidian) → react.
//!
//! Matching on `hash == after_hash` (rather than "the single most recent write") is what makes
//! this robust to event coalescing and the human-edits-after-agent case: whichever agent write
//! produced the *current* bytes is the one that explains them.
//!
//! Recency, cross-hook correlation de-looping, and reaction-depth limits are layered by the daemon
//! on top of this primitive (it owns the event timestamp and the cascade state); this module is
//! the pure content-identity join.

use std::path::Path;

use turbovault_audit::AuditFilter;

use crate::error::VaultResult;
use crate::{Vault, WriteProvenance};

/// How many recent audit entries to scan when looking for the one that explains current content.
/// Attribution runs right after a change, so the explaining write is among the most recent.
const SCAN_LIMIT: usize = 256;

/// The result of attributing an observed change to a writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attribution {
    /// A recognized agent authored the current content. Do **not** react (it's our own write).
    Agent(WriteProvenance),
    /// No agent write produced the current content — an external/human edit. **React.**
    External,
    /// The path could not be read (e.g. deleted). The caller decides (a delete is usually its own
    /// event); this module does not guess.
    Missing,
}

impl Vault {
    /// Attribute the current state of `rel_path` to a writer (the §6 hash-join).
    pub async fn attribute(&self, rel_path: impl AsRef<Path>) -> VaultResult<Attribution> {
        let rel_path = rel_path.as_ref();

        let content = match self.read(rel_path).await {
            Ok(c) => c,
            Err(_) => return Ok(Attribution::Missing),
        };
        let hash = Vault::content_hash(&content);
        let target = normalize(&rel_path.to_string_lossy());

        // Scan recent entries and match in Rust. We deliberately do NOT narrow with
        // `AuditFilter::with_path`: it matches only an entry's `path`, so it would miss a Move
        // entry when attributing the move's *destination* (whose `path` field is the source).
        // The post-filter below checks both `path` and `new_path`.
        let entries = self
            .audit()
            .query(&AuditFilter::new().with_limit(SCAN_LIMIT))
            .await?;

        // Entries come newest-first. Find the most recent one whose recorded `after_hash` equals
        // the current content hash — i.e. the write that produced what's on disk now.
        //
        // Match against the entry's *resulting* path: a Move records `after_hash` for the content
        // at its destination (`new_path`), so it explains the destination, not the source. Matching
        // a move on its source path would let a later external recreation of that source — with
        // content that happens to hash-match the move — be falsely suppressed (a missed human edit,
        // the one mistake loop-breaking must never make). Create/Update/Delete have no `new_path`,
        // so they match on their own `path`.
        for entry in entries.iter() {
            let resulting_path = entry.new_path.as_deref().unwrap_or(entry.path.as_str());
            if normalize(resulting_path) != target {
                continue;
            }
            if entry.after_hash.as_deref() != Some(hash.as_str()) {
                continue;
            }

            // This audit entry explains the current bytes. Attribute by its provenance.
            return Ok(
                match WriteProvenance::from_audit_metadata(&entry.metadata) {
                    // An agent we recognize (not a human-sourced write) → suppress.
                    Some(prov) if !prov.is_human() => Attribution::Agent(prov),
                    // Explicitly human-sourced. Believe it; front matter must not override an
                    // audit entry that names a human.
                    Some(_) => Attribution::External,
                    // No provenance on the entry. Not necessarily external: a write that reached
                    // the vault through an MCP *tool* rather than this adapter lands here, because
                    // the tool layer does not forward our `_meta` into the audit entry. Fall back
                    // to the note's own front matter. See `frontmatter_provenance`.
                    None => match frontmatter_provenance(&content) {
                        Some(prov) if !prov.is_human() => Attribution::Agent(prov),
                        _ => Attribution::External,
                    },
                },
            );
        }

        // No agent write produced these exact bytes → external/human edit.
        Ok(Attribution::External)
    }

    /// Convenience predicate over [`attribute`](Self::attribute): should a reactive consumer act
    /// on a change to `rel_path`? `true` for external/human edits, `false` for our own writes and
    /// for missing paths (a deletion is delivered as its own event, not inferred here).
    pub async fn should_react(&self, rel_path: impl AsRef<Path>) -> VaultResult<bool> {
        Ok(matches!(
            self.attribute(rel_path).await?,
            Attribution::External
        ))
    }
}

/// Normalize path separators so attribution is cross-platform (the audit log spells relative
/// paths with the OS separator; events/callers may use `/`).
fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

/// Front-matter keys the orchestrator's report sink stamps into a delivered note
/// (`vault_note_body` in `liberado-orchestrator`).
const FM_SOURCE_KEY: &str = "liberado_source";
const FM_CORRELATION_KEY: &str = "liberado_correlation";

/// Recover provenance from a note's YAML front matter.
///
/// **This is a fallback, and only legitimate because of where it is called from.** The module-level
/// rule (see `liberado_common::provenance`) is that provenance rides the audit log and *not* front
/// matter, because front matter is last-writer-only state that goes stale the instant a human edits
/// the note in Obsidian. That objection is precisely what the call site neutralises: this runs only
/// after an audit entry's `after_hash` was found equal to the bytes currently on disk. A human's
/// Obsidian save produces no such entry, so a stale `liberado_source:` left in a note a human then
/// edited is never consulted — the hash no longer matches and attribution has already returned
/// `External`. What reaches here is a write that *did* go through the vault's audit path but arrived
/// without metadata.
///
/// That gap is real: an MCP tool call carries our [`WriteProvenance`] in the request's `_meta`, but
/// Turbovault's tool layer does not forward it into the audit entry, so the entry lands with
/// `metadata: {}`. Writes through this adapter are unaffected — they attach provenance directly.
/// The concrete failure this closes: the daemon delivered a generated morning brief through the
/// report sink, saw the resulting change, could not attribute it, and dispatched an agent to react
/// to the system's own output (three times over, on 2026-07-26).
///
/// Deliberately a two-key scan, not a YAML parse: only the sink's own keys are recognised, so this
/// cannot be widened by whatever else a note happens to carry.
fn frontmatter_provenance(content: &str) -> Option<WriteProvenance> {
    // Front matter must open on the very first line, or there is none.
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;

    let mut source = None;
    let mut correlation = None;
    let mut closed = false;

    for line in rest.lines() {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            FM_SOURCE_KEY => source = Some(value.to_string()),
            FM_CORRELATION_KEY => correlation = Some(value.to_string()),
            _ => {}
        }
    }

    // An unterminated block is not front matter — refuse to read provenance out of the body.
    if !closed {
        return None;
    }

    Some(WriteProvenance {
        source: source.filter(|s| !s.is_empty())?,
        correlation_id: correlation.filter(|c| !c.is_empty()),
        zone: None,
        note: None,
    })
}

#[cfg(test)]
#[path = "attribution_tests.rs"]
mod tests;
