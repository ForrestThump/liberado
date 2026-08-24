//! Directory scan + per-file JSONL parse + match + snippet extraction.

use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use liberado_conversation_store::Author;
use liberado_session_store::{Record, SessionHeader};

use crate::query::ParsedQuery;

/// One matching message within a conversation.
#[derive(Debug, Clone, Serialize)]
pub struct MessageMatch {
    pub node_id: String,
    pub author: String,
    pub content_snippet: String,
    pub created_at: String,
}

/// One conversation containing at least one matching message.
#[derive(Debug, Clone, Serialize)]
pub struct ConversationMatch {
    pub conversation_id: String,
    pub title: Option<String>,
    pub created_at: String,
    pub matches: Vec<MessageMatch>,
}

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("I/O error scanning conversations directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("query parse error: {0}")]
    Query(#[from] crate::query::QueryParseError),
}

/// The result of a search: `matches` is already truncated to the caller's requested `limit`;
/// `total_found` is the count *before* truncation, so a caller can honestly report "N of M" —
/// truncating first and then taking `matches.len()` would just echo `limit` back, which is why
/// this is a dedicated field rather than something derived from `matches` alone.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResults {
    pub matches: Vec<ConversationMatch>,
    pub total_found: usize,
}

const SNIPPET_RADIUS: usize = 120; // chars either side of the first match

fn snippet(content: &str, query: &ParsedQuery) -> String {
    let center = query.find_start(content).unwrap_or(0);
    let start = content.floor_char_boundary(center.saturating_sub(SNIPPET_RADIUS));
    let end = content.ceil_char_boundary((center + SNIPPET_RADIUS).min(content.len()));
    let mut s = String::new();
    if start > 0 {
        s.push('\u{2026}');
    }
    s.push_str(&content[start..end]);
    if end < content.len() {
        s.push('\u{2026}');
    }
    s
}

fn author_label(author: &Author) -> String {
    match author {
        Author::System => "system".to_string(),
        Author::User => "user".to_string(),
        Author::Assistant => "assistant".to_string(),
        Author::Tool => "tool".to_string(),
        Author::Named(name) => name.clone(),
    }
}

/// Search all conversations under `root` for messages matching `query`.
///
/// Scans every `.jsonl` file fully (skipping empty-content messages — tool-call-only nodes — and
/// any line that fails to parse, since this is a best-effort search path, not the authoritative
/// store), sorts newest-conversation-first, then truncates to `limit` — but `total_found` on the
/// returned [`SearchResults`] reflects the count *before* that truncation, so a caller can
/// honestly report "N of M" rather than a number that trivially always equals `limit`. The scan
/// itself is never cut short mid-walk; personal-scale corpora make a full scan per query cheap.
pub async fn search(
    root: &Path,
    query: &ParsedQuery,
    limit: usize,
) -> Result<SearchResults, SearchError> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let read_dir = match std::fs::read_dir(root) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SearchResults {
                matches: Vec::new(),
                total_found: 0,
            });
        }
        Err(e) => return Err(e.into()),
    };
    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            paths.push(path);
        }
    }

    let mut matches = Vec::new();
    for path in &paths {
        if let Some(conv_match) = scan_file(path, query)? {
            matches.push(conv_match);
        }
    }

    matches.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let total_found = matches.len();
    matches.truncate(limit);
    Ok(SearchResults {
        matches,
        total_found,
    })
}

fn scan_file(path: &Path, query: &ParsedQuery) -> Result<Option<ConversationMatch>, SearchError> {
    let contents = std::fs::read_to_string(path)?;
    let mut header: Option<SessionHeader> = None;
    let mut message_matches: Vec<MessageMatch> = Vec::new();

    for line in contents.split('\n') {
        if line.is_empty() {
            continue;
        }
        let record: Record = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue, // corrupt line: skip, this is a best-effort search path
        };
        match record {
            Record::Header(h) => header = Some(*h),
            Record::Node(node) => {
                let content = &node.message.content;
                if content.is_empty() {
                    continue; // tool-call-only node, nothing to search
                }
                if node.author.is_compaction_tail_copy() {
                    // A compaction's re-appended tail copy — the same text already appears
                    // earlier in this file as the original. Indexing both would report one
                    // message as two hits.
                    continue;
                }
                if query.matches(content) {
                    message_matches.push(MessageMatch {
                        node_id: node.id.to_string(),
                        author: author_label(&node.author),
                        content_snippet: snippet(content, query),
                        created_at: node.created_at.to_rfc3339(),
                    });
                }
            }
            // A goal session records its transcript as *events*, which carry no searchable message
            // text — so scanning one converged store means skipping these. (A pack that recorded
            // its turns as message nodes instead would become searchable here for free.)
            Record::Event(_) | Record::Status { .. } | Record::Finish { .. } => {}
        }
    }

    if message_matches.is_empty() {
        return Ok(None);
    }
    let Some(header) = header else {
        return Ok(None); // malformed file (no header line): skip in this best-effort path
    };
    // A goal session has no title of its own; its goal reads as one. Search spans every session
    // now, not just chats, so the label has to work for both.
    let title = header
        .title
        .clone()
        .or_else(|| header.goal.as_ref().map(|g| g.description.clone()));
    Ok(Some(ConversationMatch {
        conversation_id: header.id.to_string(),
        title,
        created_at: header.created_at.to_rfc3339(),
        matches: message_matches,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_fixture(dir: &TempDir, filename: &str, lines: &[&str]) {
        let path = dir.path().join(filename);
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    fn header(id: &str, title: Option<&str>, created_at: &str) -> String {
        let title_json = match title {
            Some(t) => format!("\"{t}\""),
            None => "null".to_string(),
        };
        format!(
            r#"{{"kind":"header","id":"{id}","title":{title_json},"parent_conversation":null,"spawned_by":null,"created_at":"{created_at}"}}"#
        )
    }

    fn node(id: &str, conv_id: &str, author: &str, content: &str, created_at: &str) -> String {
        format!(
            r#"{{"kind":"node","id":"{id}","parent_id":null,"conversation_id":"{conv_id}","author":"{author}","created_at":"{created_at}","message":{{"role":"{author}","content":"{content}","tool_calls":[],"tool_call_id":null}}}}"#
        )
    }

    #[tokio::test]
    async fn literal_single_term_matches() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            &dir,
            "01JVAAAAAAAAAAAAAAAAAAAAAA.jsonl",
            &[
                &header(
                    "01JVAAAAAAAAAAAAAAAAAAAAAA",
                    Some("Test"),
                    "2026-01-01T00:00:00Z",
                ),
                &node(
                    "01JVAAAAAAAAAAAAAAAAAAAAB1",
                    "01JVAAAAAAAAAAAAAAAAAAAAAA",
                    "assistant",
                    "hello world",
                    "2026-01-02T00:00:00Z",
                ),
            ],
        );
        let q = ParsedQuery::parse_literal("hello").unwrap();
        let sr = search(dir.path(), &q, 10).await.unwrap();
        assert_eq!(sr.total_found, 1);
        assert_eq!(sr.matches.len(), 1);
        assert_eq!(sr.matches[0].matches.len(), 1);
        assert_eq!(sr.matches[0].matches[0].author, "assistant");
    }

    #[tokio::test]
    async fn literal_and_requires_all_terms_in_one_message() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            &dir,
            "01JVAAAAAAAAAAAAAAAAAAAAAA.jsonl",
            &[
                &header("01JVAAAAAAAAAAAAAAAAAAAAAA", None, "2026-01-01T00:00:00Z"),
                &node(
                    "01JVAAAAAAAAAAAAAAAAAAAAB1",
                    "01JVAAAAAAAAAAAAAAAAAAAAAA",
                    "user",
                    "hello world",
                    "2026-01-02T00:00:00Z",
                ),
                &node(
                    "01JVAAAAAAAAAAAAAAAAAAAAB2",
                    "01JVAAAAAAAAAAAAAAAAAAAAAA",
                    "user",
                    "goodbye world",
                    "2026-01-02T00:01:00Z",
                ),
            ],
        );
        let q = ParsedQuery::parse_literal("hello goodbye").unwrap();
        let sr = search(dir.path(), &q, 10).await.unwrap();
        // Neither individual message contains both terms.
        assert_eq!(sr.matches.len(), 0);
        assert_eq!(sr.total_found, 0);
    }

    #[tokio::test]
    async fn quoted_phrase_matches_as_one_term() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            &dir,
            "01JVAAAAAAAAAAAAAAAAAAAAAA.jsonl",
            &[
                &header("01JVAAAAAAAAAAAAAAAAAAAAAA", None, "2026-01-01T00:00:00Z"),
                &node(
                    "01JVAAAAAAAAAAAAAAAAAAAAB1",
                    "01JVAAAAAAAAAAAAAAAAAAAAAA",
                    "user",
                    "the quick brown fox",
                    "2026-01-02T00:00:00Z",
                ),
            ],
        );
        let q = ParsedQuery::parse_literal("\"quick brown\"").unwrap();
        let sr = search(dir.path(), &q, 10).await.unwrap();
        assert_eq!(sr.matches.len(), 1);
    }

    #[tokio::test]
    async fn regex_mode_matches() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            &dir,
            "01JVAAAAAAAAAAAAAAAAAAAAAA.jsonl",
            &[
                &header("01JVAAAAAAAAAAAAAAAAAAAAAA", None, "2026-01-01T00:00:00Z"),
                &node(
                    "01JVAAAAAAAAAAAAAAAAAAAAB1",
                    "01JVAAAAAAAAAAAAAAAAAAAAAA",
                    "assistant",
                    "Error: connection refused",
                    "2026-01-02T00:00:00Z",
                ),
            ],
        );
        let q = ParsedQuery::parse_regex("error: .+refused").unwrap();
        let sr = search(dir.path(), &q, 10).await.unwrap();
        assert_eq!(sr.matches.len(), 1);
    }

    #[tokio::test]
    async fn empty_content_messages_are_skipped() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            &dir,
            "01JVAAAAAAAAAAAAAAAAAAAAAA.jsonl",
            &[
                &header("01JVAAAAAAAAAAAAAAAAAAAAAA", None, "2026-01-01T00:00:00Z"),
                &node(
                    "01JVAAAAAAAAAAAAAAAAAAAAB1",
                    "01JVAAAAAAAAAAAAAAAAAAAAAA",
                    "assistant",
                    "",
                    "2026-01-02T00:00:00Z",
                ),
            ],
        );
        let q = ParsedQuery::parse_literal("anything").unwrap();
        let sr = search(dir.path(), &q, 10).await.unwrap();
        assert_eq!(sr.matches.len(), 0);
    }

    #[tokio::test]
    async fn limit_truncates_after_a_full_scan() {
        let dir = TempDir::new().unwrap();
        for i in 0u8..3 {
            let cid = format!("01JVAAAAAAAAAAAAAAAAAAAA{i:02}"); // 26 chars total
            let nid = format!("01JVAAAAAAAAAAAAAAAAAAB1{i:02}"); // 26 chars total
            write_fixture(
                &dir,
                &format!("{cid}.jsonl"),
                &[
                    &header(&cid, None, "2026-01-01T00:00:00Z"),
                    &node(&nid, &cid, "user", "hello", "2026-01-02T00:00:00Z"),
                ],
            );
        }
        let q = ParsedQuery::parse_literal("hello").unwrap();
        let sr = search(dir.path(), &q, 2).await.unwrap();
        // All 3 conversations matched, but the response is capped at 2 — and total_found still
        // honestly reports 3, the whole point of tracking it separately from the truncated Vec.
        assert_eq!(sr.matches.len(), 2);
        assert_eq!(sr.total_found, 3);
    }

    #[tokio::test]
    async fn missing_directory_returns_empty_not_error() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        let q = ParsedQuery::parse_literal("anything").unwrap();
        let sr = search(&missing, &q, 10).await.unwrap();
        assert!(sr.matches.is_empty());
        assert_eq!(sr.total_found, 0);
    }

    /// Tail copies (`Author::Named("compaction-tail")`) duplicate text that is
    /// already earlier in the log as the original. The scanner must skip them or
    /// one message would report as two search hits.
    #[tokio::test]
    async fn scanner_skips_compaction_tail_copies() {
        let dir = TempDir::new().unwrap();
        write_fixture(
            &dir,
            "01JVAAAAAAAAAAAAAAAAAAAAAA.jsonl",
            &[
                &header("01JVAAAAAAAAAAAAAAAAAAAAAA", None, "2026-01-01T00:00:00Z"),
                // Original — should match
                &node(
                    "01JVAAAAAAAAAAAAAAAAAAAAB1",
                    "01JVAAAAAAAAAAAAAAAAAAAAAA",
                    "user",
                    "hello world",
                    "2026-01-02T00:00:00Z",
                ),
                // Tail copy of the same content — must NOT produce a second hit
                r#"{"kind":"node","id":"01JVAAAAAAAAAAAAAAAAAAAAB2","parent_id":"01JVAAAAAAAAAAAAAAAAAAAAB1","conversation_id":"01JVAAAAAAAAAAAAAAAAAAAAAA","author":{"named":"compaction-tail"},"created_at":"2026-01-03T00:00:00Z","message":{"role":"assistant","content":"hello world","tool_calls":[],"tool_call_id":null}}"#,
                // Another original with different content — should match
                &node(
                    "01JVAAAAAAAAAAAAAAAAAAAAB3",
                    "01JVAAAAAAAAAAAAAAAAAAAAAA",
                    "assistant",
                    "goodbye moon",
                    "2026-01-04T00:00:00Z",
                ),
                // Tail copy of that too — must NOT produce a third hit
                r#"{"kind":"node","id":"01JVAAAAAAAAAAAAAAAAAAAAB4","parent_id":"01JVAAAAAAAAAAAAAAAAAAAAB3","conversation_id":"01JVAAAAAAAAAAAAAAAAAAAAAA","author":{"named":"compaction-tail"},"created_at":"2026-01-05T00:00:00Z","message":{"role":"assistant","content":"goodbye moon","tool_calls":[],"tool_call_id":null}}"#,
            ],
        );
        let q = ParsedQuery::parse_literal("hello").unwrap();
        let sr = search(dir.path(), &q, 10).await.unwrap();
        // Only the original "hello world" should match; tail copy is skipped.
        assert_eq!(sr.matches.len(), 1);
        assert_eq!(sr.matches[0].matches.len(), 1);
        assert!(sr.matches[0].matches[0].content_snippet.contains("hello"));

        // "goodbye" search should also return 1 hit (original only, not the tail copy)
        let q2 = ParsedQuery::parse_literal("goodbye").unwrap();
        let sr2 = search(dir.path(), &q2, 10).await.unwrap();
        assert_eq!(sr2.matches.len(), 1);
        assert_eq!(sr2.matches[0].matches.len(), 1);
    }

    #[test]
    fn snippet_ellipsis_depends_on_content_around_the_match() {
        let q = ParsedQuery::parse_literal("target").unwrap();

        // Match with text on both sides: both ellipses appear.
        let long = format!("{} target {}", "x".repeat(200), "y".repeat(200));
        let s = snippet(&long, &q);
        assert!(s.starts_with('\u{2026}'), "text before the match: {s}");
        assert!(s.ends_with('\u{2026}'), "text after the match: {s}");

        // Match at the very start of content: no leading ellipsis.
        let at_start = format!("target {}", "y".repeat(300));
        let s = snippet(&at_start, &q);
        assert!(!s.starts_with('\u{2026}'), "no text before the match: {s}");
        assert!(s.ends_with('\u{2026}'));

        // Match at the very end: no trailing ellipsis.
        let at_end = format!("{} target", "x".repeat(300));
        let s = snippet(&at_end, &q);
        assert!(s.starts_with('\u{2026}'));
        assert!(!s.ends_with('\u{2026}'), "no text after the match: {s}");
    }

    /// A non-existent root returns empty results rather than an error — the scan
    /// is a best-effort search over whatever conversations exist.
    #[tokio::test]
    async fn search_nonexistent_root_returns_empty_results_not_an_error() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        let q = ParsedQuery::parse_literal("anything").unwrap();
        let results = search(missing.as_path(), &q, 10).await.unwrap();
        assert_eq!(results.total_found, 0);
        assert!(results.matches.is_empty());
    }

    /// A non-NotFound error must propagate, not be swallowed by the
    /// NotFound guard. Passing a regular file as root produces
    /// NotADirectory, which is a genuine misconfiguration.
    #[tokio::test]
    async fn search_propagates_non_notfound_errors() {
        let dir = TempDir::new().unwrap();
        let file_root = dir.path().join("regular-file");
        std::fs::write(&file_root, "not a directory").unwrap();
        let q = ParsedQuery::parse_literal("anything").unwrap();
        let result = search(&file_root, &q, 10).await;
        assert!(
            result.is_err(),
            "a non-directory root must produce an error, not empty results"
        );
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn proptest_snippet_never_panics(
            content in "\\PC{0,200}",
            terms in proptest::collection::vec("[a-z]{1,6}", 1..3),
        ) {
            let q = ParsedQuery::parse_literal(&terms.join(" ")).unwrap();
            let _ = snippet(&content, &q);
        }

        #[test]
        fn proptest_find_start_bounds_snippet(
            terms in proptest::collection::vec("[a-z]{1,6}", 1..3),
            haystack in "[a-zA-Z ]{0,200}",
        ) {
            let input = terms.join(" ");
            let Ok(q) = ParsedQuery::parse_literal(&input) else {
                return Ok(());
            };
            if q.matches(&haystack) {
                let center = q.find_start(&haystack).unwrap_or(0);
                prop_assert!(center <= haystack.len());
            }
        }
    }
}
