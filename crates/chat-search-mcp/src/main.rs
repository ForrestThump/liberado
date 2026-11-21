//! Liberado chat-search MCP server.
//!
//! Exposes one tool, `search_conversations`, backed by `liberado-chat-search` (the same search
//! implementation `liberado-server`'s `GET /api/conversations/search` uses) — so the dispatcher
//! can search chat history mid-reasoning, not just the human via the webui's sidebar search box.
//!
//! Registered in `topology.toml` as a plain `stdio` MCP (NOT a managed MCP — managed MCPs are
//! `cargo install --git`'d from an external repo; this crate lives in the workspace and is built
//! locally: `cargo build --bin liberado-chat-search-mcp`).

use std::path::PathBuf;

use liberado_chat_search::ParsedQuery;
use turbomcp::prelude::*;

#[derive(Clone)]
struct ChatSearchServer {
    /// The **converged** session store (S5′/D7) — the same directory `liberado-server`'s own
    /// `/api/conversations/search` reads. This binary used to `.join("conversations")` itself,
    /// and kept doing so after chat moved: it searched a directory nothing had written to since
    /// convergence, quietly returning a frozen archive and none of what the human had said since.
    /// Injected as a field rather than read from [`liberado_config::sessions_dir`] inside the
    /// tool, so tests can point a server at a scratch directory without mutating
    /// `LIBERADO_DATA_DIR` (process-global state that races under parallel test execution).
    sessions_root: PathBuf,
}

impl ChatSearchServer {
    fn new() -> Self {
        Self {
            sessions_root: liberado_config::sessions_dir(),
        }
    }
}

#[turbomcp::server(
    name = "chat-search",
    version = "0.1.0",
    description = "Search Liberado conversation history by keyword or regex"
)]
impl ChatSearchServer {
    /// Search conversation history for messages matching a query.
    ///
    /// Literal mode (regex=false, the default): the query is split on whitespace; "quoted
    /// phrases" count as one term; ALL terms must appear in **the same message** (case-insensitive
    /// AND), not just anywhere in the conversation — good for narrowing toward a topic from a few
    /// half-remembered keywords, but a query like "auth token" won't match a conversation where
    /// those two words appear in different messages. Regex mode (regex=true): the query is a
    /// single Rust regex pattern, matched case-insensitively (also per-message).
    ///
    /// Returns up to `limit` conversations (newest first), each with the matching messages'
    /// snippets, as JSON.
    ///
    /// `project` limits matches to that project id or topology name. `scope` of `project`
    /// uses the current chat's project (`conversation` is that chat's id). Omit both to
    /// search every chat.
    #[tool(
        description = "Search chats. Literal mode (default): every term must occur in the same message; quotes keep a phrase. Regex mode: one case-insensitive pattern. Pass project to limit results to that project id or topology name. Pass scope \"project\" to limit results to the current chat's project. Omit both to search every chat."
    )]
    async fn search_conversations(
        &self,
        query: String,
        regex: bool,
        limit: i32,
        project: Option<String>,
        scope: Option<String>,
        conversation: Option<String>,
    ) -> McpResult<String> {
        // The **converged** session store (S5′/D7) — resolved once at construction so a server
        // built for tests can point elsewhere (see `sessions_root`).
        let root = &self.sessions_root;

        let parsed = if regex {
            ParsedQuery::parse_regex(&query)
        } else {
            ParsedQuery::parse_literal(&query)
        }
        .map_err(|e| McpError::invalid_params(e.to_string()))?;

        let current = conversation
            .as_deref()
            .map(|id| liberado_chat_search::stored_project(root, id));
        let project = liberado_chat_search::resolve_project_filter(
            project.as_deref(),
            scope.as_deref(),
            current.as_ref(),
        )
        .map_err(McpError::invalid_params)?;

        let limit = limit.clamp(1, 50) as usize;
        let results = liberado_chat_search::search_in(root, &parsed, limit, project.as_deref())
            .await
            .map_err(|e| McpError::internal(e.to_string()))?;

        serde_json::to_string_pretty(&results).map_err(|e| McpError::internal(e.to_string()))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // MUST write to stderr, never stdout — stdout carries the MCP JSON-RPC protocol stream.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("liberado-chat-search-mcp starting");
    ChatSearchServer::new().run_stdio().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use turbomcp::prelude::McpTestClient;

    // ── JSONL session-store fixtures ─────────────────────────────────────────────
    //
    // Raw JSON-Lines in the converged (D7) session-store shape, so these tests stay off the
    // `liberado-session-store` writer API and mirror the search crate's own fixtures.

    fn header(id: &str, title: Option<&str>, created_at: &str) -> String {
        let title_json = title
            .map(|t| format!("\"{t}\""))
            .unwrap_or_else(|| "null".to_string());
        format!(
            r#"{{"kind":"header","id":"{id}","title":{title_json},"parent_conversation":null,"spawned_by":null,"created_at":"{created_at}"}}"#
        )
    }

    fn node(id: &str, conv_id: &str, author: &str, content: &str, created_at: &str) -> String {
        format!(
            r#"{{"kind":"node","id":"{id}","parent_id":null,"conversation_id":"{conv_id}","author":"{author}","created_at":"{created_at}","message":{{"role":"{author}","content":"{content}","tool_calls":[],"tool_call_id":null}}}}"#
        )
    }

    fn write_fixture(dir: &std::path::Path, filename: &str, lines: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        let mut f = std::fs::File::create(dir.join(filename)).unwrap();
        for line in lines {
            use std::io::Write;
            writeln!(f, "{line}").unwrap();
        }
    }

    /// A server rooted at a scratch directory instead of `sessions_dir()`.
    fn server(root: &std::path::Path) -> ChatSearchServer {
        ChatSearchServer {
            sessions_root: root.to_path_buf(),
        }
    }

    fn conv_a() -> (&'static str, &'static str) {
        ("01JVAAAAAAAAAAAAAAAAAAAAAA", "01JVAAAAAAAAAAAAAAAAAAAAB1")
    }

    // ── tool logic (direct calls) ────────────────────────────────────────────────

    #[tokio::test]
    async fn literal_query_finds_a_matching_message_snippet() {
        let dir = tempfile::tempdir().unwrap();
        let (conv, n1) = conv_a();
        write_fixture(
            dir.path(),
            &format!("{conv}.jsonl"),
            &[
                &header(conv, Some("Warmup"), "2026-01-01T00:00:00Z"),
                &node(
                    n1,
                    conv,
                    "assistant",
                    "The user prefers dark mode.",
                    "2026-01-02T00:00:00Z",
                ),
            ],
        );

        let out = server(dir.path())
            .search_conversations("dark mode".into(), false, 10, None, None, None)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["total_found"], 1);
        let m = &v["matches"][0];
        assert_eq!(m["title"], "Warmup");
        assert!(
            m["matches"][0]["content_snippet"]
                .as_str()
                .unwrap()
                .contains("dark mode")
        );
    }

    #[tokio::test]
    async fn absent_store_returns_empty_results_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no/such/dir");

        let out = server(&missing)
            .search_conversations("anything".into(), false, 10, None, None, None)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["matches"], json!([]));
        assert_eq!(v["total_found"], 0);
    }

    #[tokio::test]
    async fn literal_terms_across_different_messages_do_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let (conv, n1) = conv_a();
        write_fixture(
            dir.path(),
            &format!("{conv}.jsonl"),
            &[
                &header(conv, None, "2026-01-01T00:00:00Z"),
                &node(n1, conv, "user", "hello", "2026-01-02T00:00:00Z"),
                &node(
                    "01JVAAAAAAAAAAAAAAAAAAAAB2",
                    conv,
                    "assistant",
                    "world ",
                    "2026-01-03T00:00:00Z",
                ),
            ],
        );

        let out = server(dir.path())
            .search_conversations("hello world".into(), false, 10, None, None, None)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["total_found"], 0,
            "terms split across messages must not match"
        );
    }

    #[tokio::test]
    async fn quoted_phrase_counts_as_one_literal_term() {
        let dir = tempfile::tempdir().unwrap();
        let (conv, n1) = conv_a();
        write_fixture(
            dir.path(),
            &format!("{conv}.jsonl"),
            &[
                &header(conv, None, "2026-01-01T00:00:00Z"),
                &node(
                    n1,
                    conv,
                    "user",
                    "I love dark mode themes.",
                    "2026-01-02T00:00:00Z",
                ),
            ],
        );

        // "dark mode" as one quoted phrase matches a message containing the whole phrase.
        let out = server(dir.path())
            .search_conversations("\"dark mode\" themes".into(), false, 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap()["total_found"],
            1
        );
    }

    #[tokio::test]
    async fn regex_mode_matches_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let (conv, n1) = conv_a();
        write_fixture(
            dir.path(),
            &format!("{conv}.jsonl"),
            &[
                &header(conv, None, "2026-01-01T00:00:00Z"),
                &node(
                    n1,
                    conv,
                    "assistant",
                    "Look for DARK here.",
                    "2026-01-02T00:00:00Z",
                ),
            ],
        );

        let out = server(dir.path())
            .search_conversations("dark".into(), true, 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap()["total_found"],
            1
        );
    }

    #[tokio::test]
    async fn invalid_regex_is_rejected_as_invalid_params() {
        let dir = tempfile::tempdir().unwrap();
        let err = server(dir.path())
            .search_conversations("([".into(), true, 10, None, None, None)
            .await
            .unwrap_err();
        // `kind` is a field; compare kinds without naming `ErrorKind` (which turbomcp's umbrella
        // does not re-export).
        assert_eq!(
            err.kind,
            McpError::invalid_params("").kind,
            "a bad regex must surface as invalid_params, got {err}"
        );
    }

    #[tokio::test]
    async fn limit_is_clamped_to_at_least_one() {
        let dir = tempfile::tempdir().unwrap();
        let (conv, n1) = conv_a();
        let (conv2, n2) = ("01JVAAAAAAAAAAAAAAAAAAAABB", "01JVAAAAAAAAAAAAAAAAAAAAB2");
        write_fixture(
            dir.path(),
            &format!("{conv}.jsonl"),
            &[
                &header(conv, None, "2026-01-01T00:00:00Z"),
                &node(n1, conv, "user", "alpha beta", "2026-01-02T00:00:00Z"),
            ],
        );
        write_fixture(
            dir.path(),
            &format!("{conv2}.jsonl"),
            &[
                &header(conv2, None, "2026-01-01T00:00:00Z"),
                &node(n2, conv2, "user", "alpha gamma", "2026-01-02T00:00:00Z"),
            ],
        );

        // limit 0 (and negative) clamp up to 1; both conversations match "alpha".
        let out = server(dir.path())
            .search_conversations("alpha".into(), false, 0, None, None, None)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["matches"].as_array().unwrap().len(), 1);
        assert_eq!(v["total_found"], 2, "total_found reflects pre-truncation");
    }

    #[tokio::test]
    async fn newest_conversation_comes_first() {
        let dir = tempfile::tempdir().unwrap();
        let (conv, n1) = conv_a();
        let (conv2, n2) = ("01JVAAAAAAAAAAAAAAAAAAAABB", "01JVAAAAAAAAAAAAAAAAAAAAB2");
        write_fixture(
            dir.path(),
            &format!("{conv}.jsonl"),
            &[
                &header(conv, Some("Older"), "2026-01-01T00:00:00Z"),
                &node(n1, conv, "user", "old topic x", "2026-01-02T00:00:00Z"),
            ],
        );
        write_fixture(
            dir.path(),
            &format!("{conv2}.jsonl"),
            &[
                &header(conv2, Some("Newer"), "2026-02-01T00:00:00Z"),
                &node(n2, conv2, "user", "new topic x", "2026-02-02T00:00:00Z"),
            ],
        );

        let out = server(dir.path())
            .search_conversations("x".into(), false, 10, None, None, None)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["matches"].as_array().unwrap().len(), 2);
        assert_eq!(v["matches"][0]["title"], "Newer");
        assert_eq!(v["matches"][1]["title"], "Older");
    }

    #[tokio::test]
    async fn limit_is_capped_at_fifty() {
        let dir = tempfile::tempdir().unwrap();
        // 55 conversations all matching "alpha" — more than the 50 cap.
        // 55 conversations all matching "alpha" — more than the 50 cap. ULID ids must be
        // exactly 26 Crockford chars or the session-store line parser skips them.
        for i in 0..55u32 {
            let conv = format!("01JV{}{:04}", "A".repeat(18), i);
            let node_id = format!("01JV{}{:04}", "B".repeat(18), i);
            write_fixture(
                dir.path(),
                &format!("{conv}.jsonl"),
                &[
                    &header(&conv, None, "2026-01-01T00:00:00Z"),
                    &node(
                        &node_id,
                        &conv,
                        "user",
                        "alpha topic",
                        "2026-01-02T00:00:00Z",
                    ),
                ],
            );
        }

        let out = server(dir.path())
            .search_conversations("alpha".into(), false, 999, None, None, None)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["matches"].as_array().unwrap().len(),
            50,
            "limit is clamped to the 50-result ceiling"
        );
        assert_eq!(v["total_found"], 55, "total_found reflects pre-truncation");
    }

    #[tokio::test]
    async fn corrupt_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (conv, n1) = conv_a();
        write_fixture(
            dir.path(),
            &format!("{conv}.jsonl"),
            &[
                &header(conv, None, "2026-01-01T00:00:00Z"),
                "this is not json at all {{{{ [",
                &node(n1, conv, "user", "intact message", "2026-01-02T00:00:00Z"),
            ],
        );

        let out = server(dir.path())
            .search_conversations("intact".into(), false, 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap()["total_found"],
            1
        );
    }

    #[tokio::test]
    async fn a_file_without_a_header_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (conv, n1) = conv_a();
        write_fixture(
            dir.path(),
            &format!("{conv}.jsonl"),
            &[&node(
                n1,
                conv,
                "user",
                "headless message",
                "2026-01-02T00:00:00Z",
            )],
        );

        let out = server(dir.path())
            .search_conversations("headless".into(), false, 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap()["total_found"],
            0
        );
    }

    // ── MCP layer (in-process client) ────────────────────────────────────────────

    #[tokio::test]
    async fn mcp_client_advertises_the_tool() {
        let dir = tempfile::tempdir().unwrap();
        let client = McpTestClient::new(server(dir.path()));
        assert_eq!(client.server_info().name, "chat-search");
        client.assert_tool_exists("search_conversations");
    }

    #[tokio::test]
    async fn mcp_client_calls_the_tool_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let (conv, n1) = conv_a();
        write_fixture(
            dir.path(),
            &format!("{conv}.jsonl"),
            &[
                &header(conv, Some("Greetings"), "2026-01-01T00:00:00Z"),
                &node(n1, conv, "user", "hello dark mode", "2026-01-02T00:00:00Z"),
            ],
        );

        let client = McpTestClient::new(server(dir.path()));
        let result = client
            .call_tool(
                "search_conversations",
                json!({"query": "dark mode", "regex": false, "limit": 10}),
            )
            .await
            .unwrap();
        let json_out = result
            .first_text()
            .map(|t| t.to_string())
            .unwrap_or_default();
        let v: Value = serde_json::from_str(&json_out).unwrap();
        assert_eq!(v["total_found"], 1);
        assert_eq!(v["matches"][0]["title"], "Greetings");
    }

    #[tokio::test]
    async fn project_and_scope_limit_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = "01JVAAAAAAAAAAAAAAAAAAAAAA";
        let beta = "01JVAAAAAAAAAAAAAAAAAAAABB";
        let node_a = "01JVAAAAAAAAAAAAAAAAAAAAB1";
        let node_b = "01JVAAAAAAAAAAAAAAAAAAAAB2";
        write_fixture(
            dir.path(),
            &format!("{alpha}.jsonl"),
            &[
                &format!(
                    r#"{{"kind":"header","id":"{alpha}","title":"Alpha","parent_conversation":null,"spawned_by":null,"created_at":"2026-01-01T00:00:00Z","project":"proj-alpha"}}"#
                ),
                &node(
                    node_a,
                    alpha,
                    "user",
                    "shared token",
                    "2026-01-02T00:00:00Z",
                ),
            ],
        );
        write_fixture(
            dir.path(),
            &format!("{beta}.jsonl"),
            &[
                &format!(
                    r#"{{"kind":"header","id":"{beta}","title":"Beta","parent_conversation":null,"spawned_by":null,"created_at":"2026-01-01T00:00:00Z","project":"proj-beta"}}"#
                ),
                &node(node_b, beta, "user", "shared token", "2026-01-02T00:00:00Z"),
            ],
        );
        let scoped = server(dir.path());
        let by_project = scoped
            .search_conversations(
                "shared token".into(),
                false,
                10,
                Some("proj-alpha".into()),
                None,
                None,
            )
            .await
            .unwrap();
        let by_project: Value = serde_json::from_str(&by_project).unwrap();
        assert_eq!(by_project["total_found"], 1);
        assert_eq!(by_project["matches"][0]["title"], "Alpha");

        let by_scope = scoped
            .search_conversations(
                "shared token".into(),
                false,
                10,
                None,
                Some("project".into()),
                Some(alpha.into()),
            )
            .await
            .unwrap();
        let by_scope: Value = serde_json::from_str(&by_scope).unwrap();
        assert_eq!(by_scope["total_found"], 1);
        assert_eq!(by_scope["matches"][0]["conversation_id"], alpha);

        let missing = scoped
            .search_conversations(
                "shared token".into(),
                false,
                10,
                None,
                Some("project".into()),
                None,
            )
            .await
            .unwrap_err();
        assert!(missing.to_string().contains("current conversation"));
    }
}
