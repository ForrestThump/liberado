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

    /// Reproduce the shape an MCP tool write leaves behind: the bytes are on disk and the audit log
    /// has an entry whose `after_hash` matches them, but `metadata` is `{}` because the tool layer
    /// dropped our `_meta`. This is what `turbovault:write_note` actually produced for the morning
    /// brief on 2026-07-27 (verified against the live audit log).
    async fn write_like_an_mcp_tool(vault: &Vault, dir: &TempDir, rel: &str, content: &str) {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();

        let hash = Vault::content_hash(content);
        let entry = turbovault_audit::AuditEntry::new(turbovault_audit::OperationType::Create, rel)
            .with_after(hash.clone(), hash);
        assert_eq!(
            entry.metadata,
            serde_json::json!({}),
            "the bug depends on this entry carrying no provenance"
        );
        vault.audit().record(&entry).await.unwrap();
    }

    /// The regression: a delivered report is suppressed even though the audit entry lost its
    /// provenance, because the note's own front matter still names the agent that wrote it.
    #[tokio::test]
    async fn mcp_tool_write_is_attributed_from_front_matter() {
        let (vault, dir) = temp_vault().await;
        let note = "---\nliberado_source: liberado-executor\n\
                    liberado_correlation: sub:6dba0afc21695ad7\n\
                    generated: 2026-07-27T11:55:38Z\n---\n\n# Morning Brief\n\nbody\n";

        write_like_an_mcp_tool(&vault, &dir, "wakeups/2026-07-27-morning-brief.md", note).await;

        match vault
            .attribute("wakeups/2026-07-27-morning-brief.md")
            .await
            .unwrap()
        {
            Attribution::Agent(p) => {
                assert_eq!(p.source, "liberado-executor");
                assert_eq!(p.correlation_id.as_deref(), Some("sub:6dba0afc21695ad7"));
            }
            other => panic!("expected the delivered report to be suppressed, got {other:?}"),
        }
        assert!(
            !vault
                .should_react("wakeups/2026-07-27-morning-brief.md")
                .await
                .unwrap()
        );
    }

    /// The guarantee the fallback must not break. A human edits a note that still carries agent
    /// front matter; their bytes match no audit entry, so the stale front matter is never read and
    /// the edit is still reacted to. This is the one mistake loop-breaking may never make.
    #[tokio::test]
    async fn human_edit_of_a_note_with_agent_front_matter_still_reacts() {
        let (vault, dir) = temp_vault().await;
        let note =
            "---\nliberado_source: liberado-executor\nliberado_correlation: c1\n---\n\nbody\n";

        write_like_an_mcp_tool(&vault, &dir, "wakeups/brief.md", note).await;
        assert!(!vault.should_react("wakeups/brief.md").await.unwrap());

        // The human appends a line in Obsidian. Front matter still says liberado-executor.
        std::fs::write(
            dir.path().join("wakeups/brief.md"),
            format!("{note}\nmy own note\n"),
        )
        .unwrap();

        assert_eq!(
            vault.attribute("wakeups/brief.md").await.unwrap(),
            Attribution::External,
            "a human edit must be reacted to even when stale agent front matter remains"
        );
    }

    /// Front matter must not override an audit entry that explicitly names a human.
    #[tokio::test]
    async fn audit_human_provenance_beats_agent_front_matter() {
        let (vault, _dir) = temp_vault().await;
        let note = "---\nliberado_source: liberado-executor\n---\n\napproved by hand\n";

        vault
            .write("proposals/p.md", note, None, &WriteProvenance::human())
            .await
            .unwrap();

        assert_eq!(
            vault.attribute("proposals/p.md").await.unwrap(),
            Attribution::External
        );
    }

    /// A note whose front matter names no liberado source is still external, even via the MCP path.
    #[tokio::test]
    async fn mcp_tool_write_without_liberado_front_matter_is_external() {
        let (vault, dir) = temp_vault().await;

        write_like_an_mcp_tool(&vault, &dir, "notes/n.md", "---\ntags: [x]\n---\n\nbody\n").await;
        assert_eq!(
            vault.attribute("notes/n.md").await.unwrap(),
            Attribution::External
        );

        // ...and so is a note with no front matter at all.
        write_like_an_mcp_tool(&vault, &dir, "notes/plain.md", "just text\n").await;
        assert_eq!(
            vault.attribute("notes/plain.md").await.unwrap(),
            Attribution::External
        );
    }

    /// Human-sourced front matter must not be attributed as Agent via the MCP fallback path.
    /// The `!prov.is_human()` guard on the front matter branch must reject human-sourced entries,
    /// not just the audit entry branch. This is the regression test for the mutation that replaces
    /// the guard with `true`.
    #[tokio::test]
    async fn mcp_tool_write_with_human_front_matter_is_external_not_agent() {
        let (vault, dir) = temp_vault().await;
        let note = "---\nliberado_source: human\n---\n\napproved by hand\n";

        write_like_an_mcp_tool(&vault, &dir, "proposals/approved.md", note).await;

        // The front matter says `human`, so even though the audit entry has no metadata
        // (MCP path), the `!prov.is_human()` guard must reject it and fall through to External.
        assert_eq!(
            vault.attribute("proposals/approved.md").await.unwrap(),
            Attribution::External,
            "human-sourced front matter via MCP path must be External, not Agent"
        );
    }

    #[test]
    fn front_matter_parsing_edge_cases() {
        // Unterminated block: refuse to mine the body for provenance.
        assert!(
            super::frontmatter_provenance("---\nliberado_source: liberado-executor\n\nbody\n")
                .is_none()
        );
        // Must open on the first line.
        assert!(
            super::frontmatter_provenance("\n---\nliberado_source: liberado-executor\n---\n")
                .is_none()
        );
        // Quotes are stripped; a missing correlation id is allowed (source is what decides).
        let p = super::frontmatter_provenance("---\nliberado_source: \"agent-x\"\n---\n").unwrap();
        assert_eq!(p.source, "agent-x");
        assert_eq!(p.correlation_id, None);
        // An empty source is not provenance.
        assert!(super::frontmatter_provenance("---\nliberado_source:\n---\n").is_none());
        // A human-sourced note is parsed, and `is_human` is what suppresses suppression.
        let h = super::frontmatter_provenance("---\nliberado_source: human\n---\n").unwrap();
        assert!(h.is_human());
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
