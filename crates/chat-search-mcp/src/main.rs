//! Liberado chat-search MCP server.
//!
//! Exposes one tool, `search_conversations`, backed by `liberado-chat-search` (the same search
//! implementation `liberado-server`'s `GET /api/conversations/search` uses) — so the dispatcher
//! can search chat history mid-reasoning, not just the human via the webui's sidebar search box.
//!
//! Registered in `topology.toml` as a plain `stdio` MCP (NOT a managed MCP — managed MCPs are
//! `cargo install --git`'d from an external repo; this crate lives in the workspace and is built
//! locally: `cargo build --bin liberado-chat-search-mcp`).

use liberado_chat_search::ParsedQuery;
use turbomcp::prelude::*;

#[derive(Clone)]
struct ChatSearchServer;

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
    #[tool(
        description = "Search conversation history. Literal mode (default): all whitespace-separated terms must match within the SAME message (quoted phrases count as one term) — this is per-message, not per-conversation, so terms split across different messages won't match. Regex mode: query is a single case-insensitive Rust regex, also matched per-message. Returns matching conversations with message snippets, as JSON."
    )]
    async fn search_conversations(
        &self,
        query: String,
        regex: bool,
        limit: i32,
    ) -> McpResult<String> {
        // The **converged** session store (S5′/D7) — the same directory `liberado-server`'s own
        // `/api/conversations/search` reads. This binary used to `.join("conversations")` itself,
        // and kept doing so after chat moved: it searched a directory nothing had written to since
        // convergence, quietly returning a frozen archive and none of what the human had said since.
        let root = liberado_config::sessions_dir();

        let parsed = if regex {
            ParsedQuery::parse_regex(&query)
        } else {
            ParsedQuery::parse_literal(&query)
        }
        .map_err(|e| McpError::invalid_params(e.to_string()))?;

        let limit = limit.clamp(1, 50) as usize;
        let results = liberado_chat_search::search(&root, &parsed, limit)
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
    ChatSearchServer.run_stdio().await?;
    Ok(())
}
