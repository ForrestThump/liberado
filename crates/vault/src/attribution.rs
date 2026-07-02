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
                    // Matched an entry, but it was human-sourced or carried no provenance → react.
                    _ => Attribution::External,
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

#[cfg(test)]
mod tests {
    use crate::{Attribution, Vault, WriteProvenance};
    use tempfile::TempDir;

    async fn temp_vault() -> (Vault, TempDir) {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        (vault, dir)
    }

    #[tokio::test]
    async fn agent_write_is_attributed_and_suppressed() {
        let (vault, _dir) = temp_vault().await;
        let prov = WriteProvenance::agent("daily-review-agent", "review-1").with_zone("reviews");

        vault
            .write("reviews/2026-06-21.md", "# Review\nbody", None, &prov)
            .await
            .unwrap();

        match vault.attribute("reviews/2026-06-21.md").await.unwrap() {
            Attribution::Agent(p) => assert_eq!(p.correlation_id.as_deref(), Some("review-1")),
            other => panic!("expected Agent attribution, got {other:?}"),
        }
        assert!(!vault.should_react("reviews/2026-06-21.md").await.unwrap());
    }

    #[tokio::test]
    async fn human_edit_after_agent_write_is_reacted_to() {
        let (vault, dir) = temp_vault().await;
        let prov = WriteProvenance::agent("tasks-mcp", "task-1");

        vault
            .write("tasks/today.md", "- [ ] original", None, &prov)
            .await
            .unwrap();
        // Agent write is suppressed...
        assert!(!vault.should_react("tasks/today.md").await.unwrap());

        // ...then a human edits the note directly in Obsidian (a raw file write, not through the
        // adapter, so no audit entry with a matching after_hash exists).
        std::fs::write(dir.path().join("tasks/today.md"), "- [x] done by hand").unwrap();

        assert_eq!(
            vault.attribute("tasks/today.md").await.unwrap(),
            Attribution::External
        );
        assert!(vault.should_react("tasks/today.md").await.unwrap());
    }

    #[tokio::test]
    async fn latest_matching_write_wins() {
        let (vault, _dir) = temp_vault().await;

        let h1 = Vault::content_hash("v1");
        vault
            .write("notes/n.md", "v1", None, &WriteProvenance::agent("a", "c1"))
            .await
            .unwrap();
        // Second agent overwrites; current content is now "v2", attributed to the second write.
        vault
            .write(
                "notes/n.md",
                "v2",
                Some(&h1),
                &WriteProvenance::agent("b", "c2"),
            )
            .await
            .unwrap();

        match vault.attribute("notes/n.md").await.unwrap() {
            Attribution::Agent(p) => {
                assert_eq!(p.source, "b");
                assert_eq!(p.correlation_id.as_deref(), Some("c2"));
            }
            other => panic!("expected Agent(b), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn external_recreation_of_a_moved_source_is_not_falsely_suppressed() {
        // Regression: a Move records `after_hash` under its *source* path but for the
        // *destination's* content. If attribution matched the source path, a later human
        // recreation of that source with the moved content would be wrongly suppressed.
        let (vault, dir) = temp_vault().await;

        // A human-authored note (no audit entry of its own)...
        std::fs::write(dir.path().join("orig.md"), "shared content").unwrap();
        // ...is filed away by an agent (the only audit entry mentioning `orig.md` is this move).
        vault
            .move_note(
                "orig.md",
                "filed.md",
                None,
                &WriteProvenance::agent("organizer", "mv-1"),
            )
            .await
            .unwrap();

        // The human recreates the original path with that same content.
        std::fs::write(dir.path().join("orig.md"), "shared content").unwrap();

        // It must be treated as an external edit (react), not the agent's move.
        assert_eq!(
            vault.attribute("orig.md").await.unwrap(),
            Attribution::External
        );
        // ...while the destination is still correctly attributed to the agent.
        assert!(matches!(
            vault.attribute("filed.md").await.unwrap(),
            Attribution::Agent(_)
        ));
    }

    #[tokio::test]
    async fn human_sourced_write_is_not_suppressed() {
        let (vault, _dir) = temp_vault().await;
        // Even a write that went through the audit log but is tagged source=human must be reacted
        // to — attribution suppresses only *our agents'* writes.
        let human = WriteProvenance {
            source: "human".into(),
            correlation_id: None,
            zone: None,
            note: None,
        };
        vault
            .write("journal/today.md", "dear diary", None, &human)
            .await
            .unwrap();

        assert_eq!(
            vault.attribute("journal/today.md").await.unwrap(),
            Attribution::External
        );
    }

    #[tokio::test]
    async fn missing_path_is_missing() {
        let (vault, _dir) = temp_vault().await;
        assert_eq!(
            vault.attribute("does/not/exist.md").await.unwrap(),
            Attribution::Missing
        );
    }
}
