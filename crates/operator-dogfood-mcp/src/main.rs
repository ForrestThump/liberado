//! Operator dogfood MCP.
//!
//! Stdio server for an external operator agent. It calls the daemon HTTP API
//! that the WebUI already uses. It does not add a route and it does not add
//! an authentication scheme.
//!
//! # Auth
//!
//! The API reference records this HTTP API as unauthenticated. The server
//! binds all interfaces on `LIBERADO_PORT` (default 4201). The WebUI is the
//! static fallback on that same listener. Homelab Compose publishes that port
//! and its healthcheck calls the status route with no credential. ADR-0010's
//! shared-secret header applies to hook webhooks, not to `/api`. Remote
//! access is the network boundary (Tailscale), the same boundary the WebUI
//! uses. This process sends no `Authorization` header and rejects a base URL
//! that carries userinfo.
//!
//! Set `LIBERADO_SERVER` to the origin the operator already uses for the
//! WebUI. When unset, the base is loopback port 4201.
//!
//! # Sessions
//!
//! A chat is a multi-turn conversation id. Pass that id as `session` on every
//! later turn.
//!
//! An agent is one long-running Agents-shelf conversation per profile and
//! title. `create_agent` reuses it. Continuing posts to that same `session`.
//! It does not open a new conversation.
//!
//! # Human turns
//!
//! `send_human_message` and `continue_session` are the only tools that post a
//! user message. Both require the caller to pass `message`. No tool reads an
//! assistant reply and posts it back.

mod base_url;
mod client;
mod error;
mod ops;
mod shelf;

use client::DaemonClient;
use error::DogfoodError;
use turbomcp::prelude::*;

#[derive(Clone)]
struct DogfoodServer {
    client: DaemonClient,
}

impl DogfoodServer {
    fn from_client(client: DaemonClient) -> Self {
        Self { client }
    }

    /// Both human-turn tools are this call. The message is the tool argument.
    async fn human_turn(&self, session: &str, message: &str) -> McpResult<String> {
        let value = ops::post_human_turn(&self.client, session, message)
            .await
            .map_err(mcp_err)?;
        to_json(&value)
    }
}

#[turbomcp::server(
    name = "liberado-operator-dogfood-mcp",
    version = "0.1.0",
    description = "Operator dogfood tools over the Liberado HTTP API. A human reply is posted only by send_human_message or continue_session, and only with the message argument the caller passes."
)]
impl DogfoodServer {
    #[tool(
        description = "Find or create one Agents-shelf session for a profile and title. Checks GET /api/profiles and refuses a name that is not agent_eligible. Reuses the oldest matching row from GET /api/conversations (same profile and title, surface_mode agent, not the Agent Creator singleton). Otherwise POST /api/conversations with that profile and title, which stamps surface_mode agent for an agent-eligible profile. A blank title uses the profile name. Does not send a chat turn and does not call the face create_agent tool. Returns conversation_id, profile, title, surface_mode, and reused."
    )]
    async fn create_agent(&self, profile: String, title: Option<String>) -> McpResult<String> {
        let value = ops::create_agent(&self.client, &profile, title.as_deref())
            .await
            .map_err(mcp_err)?;
        to_json(&value)
    }

    #[tool(
        description = "Open a normal chat with POST /api/conversations. Omit profile for a default-grant chat. A profile that is not agent_eligible is sent through. An agent-eligible profile is refused: use create_agent so the same profile and title reuse one Agents-shelf session. Does not send a message."
    )]
    async fn start_chat(
        &self,
        profile: Option<String>,
        title: Option<String>,
    ) -> McpResult<String> {
        let value = ops::start_chat(&self.client, profile.as_deref(), title.as_deref())
            .await
            .map_err(mcp_err)?;
        to_json(&value)
    }

    #[tool(
        description = "Post one explicit human turn with POST /api/chat and body {session, message}. This is the only way a human reply enters a chat session or an agent session. Do not pass text the assistant just produced. The call blocks until the daemon returns {reply, session}. Pass the session id from create_agent or start_chat."
    )]
    async fn send_human_message(&self, session: String, message: String) -> McpResult<String> {
        self.human_turn(&session, &message).await
    }

    #[tool(
        description = "Continue an existing session with POST /api/chat and body {session, message}. Chat sessions are multi-turn: pass the same session id on every turn. Agent sessions are one long-running Agents-shelf chat per profile and title: pass that same session id. Do not open a new conversation. The message argument is the human's next line. Do not invent it from the assistant reply. Does not create a session."
    )]
    async fn continue_session(&self, session: String, message: String) -> McpResult<String> {
        self.human_turn(&session, &message).await
    }

    #[tool(
        description = "GET /api/conversations/{id} and return the latest assistant message, plus turn_running, turn_unanswered, surface_mode, and profile. Does not post a message. If turn_running is true, a turn is already in flight; read again later instead of sending."
    )]
    async fn read_replies(&self, session: String) -> McpResult<String> {
        let value = ops::read_replies(&self.client, &session)
            .await
            .map_err(mcp_err)?;
        to_json(&value)
    }

    #[tool(
        description = "GET /api/conversations/{id} and return the transcript the daemon returned. Does not post a message."
    )]
    async fn get_history(&self, session: String) -> McpResult<String> {
        let value = ops::get_history(&self.client, &session)
            .await
            .map_err(mcp_err)?;
        to_json(&value)
    }

    #[tool(
        description = "GET /api/conversations and return Agents-shelf rows (surface_mode agent, excluding the Agent Creator singleton): id, title, profile, created_at. Keeps the daemon's list order."
    )]
    async fn list_agents(&self) -> McpResult<String> {
        let value = ops::list_agents(&self.client).await.map_err(mcp_err)?;
        to_json(&value)
    }

    #[tool(
        description = "GET /api/sessions and return a short row for every session, chats and goal sessions: id, title, surface_mode, profile, status, has_goal, agent_creator."
    )]
    async fn list_sessions(&self) -> McpResult<String> {
        let value = ops::list_sessions(&self.client).await.map_err(mcp_err)?;
        to_json(&value)
    }

    #[tool(description = "GET /api/status. Daemon health. No credentials are sent.")]
    async fn status(&self) -> McpResult<String> {
        let value = ops::status(&self.client).await.map_err(mcp_err)?;
        to_json(&value)
    }

    #[tool(description = "GET /api/catalog. MCP names and tool names the daemon reports.")]
    async fn catalog(&self) -> McpResult<String> {
        let value = ops::catalog(&self.client).await.map_err(mcp_err)?;
        to_json(&value)
    }

    #[tool(
        description = "Read-only check of GET /api/status chat_tool_names for workspace_list, workspace_read, workspace_write, workspace_delete, and workspace_download. Does not write a file and does not add an HTTP route. Missing names mean the boot catalog did not list those tools."
    )]
    async fn workspace_smoke(&self) -> McpResult<String> {
        let value = ops::workspace_smoke(&self.client).await.map_err(mcp_err)?;
        to_json(&value)
    }
}

fn mcp_err(err: DogfoodError) -> McpError {
    match err {
        DogfoodError::Invalid(message) => McpError::invalid_params(message),
        DogfoodError::Daemon(message) => McpError::internal(message),
    }
}

fn to_json(value: &serde_json::Value) -> McpResult<String> {
    serde_json::to_string_pretty(value).map_err(|err| McpError::internal(err.to_string()))
}

fn init_tracing() {
    // stderr only: stdout carries the MCP JSON-RPC protocol stream.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
        )
        .init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    let server = DogfoodServer::from_client(DaemonClient::from_process_env()?);
    tracing::info!(base = %server.client.base(), "liberado-operator-dogfood-mcp starting");
    server.run_stdio().await?;
    Ok(())
}
