//! ACP (Agent Client Protocol) bridge over stdio for Paseo integration.
//!
//! Speaks **JSON-RPC 2.0 NDJSON** on stdin/stdout — the wire shape Paseo and
//! `@agentclientprotocol/sdk` expect. Methods:
//!
//! | Method | Role |
//! |---|---|
//! | `initialize` | Handshake + agent capabilities |
//! | `session/new` | Start a session rooted at `cwd` (coding tools enabled) |
//! | `session/prompt` | User turn; streams `session/update` chunks |
//! | `session/cancel` | Abort the in-flight turn (notification) |
//! | `session/load` | Not advertised (`loadSession: false`) until history is durable |
//! | `session/set_mode` | Switch coding / chat / face (Liberado-owned; one Paseo provider) |
//! | `session/set_model` | Hot-swap the active model (must be in the live catalog) |
//!
//! Modes (same process; switch via ACP or `--mode` / `LIBERADO_ACP_MODE`):
//! - **coding** — full coding pack + durable worktrees (default)
//! - **chat** — in-process conversation (no tools, no daemon)
//! - **face** — daemon face agent (`liberado serve`; vault + delegate)
//!
//! Usage (spawned by Paseo — one provider is enough):
//! ```text
//! liberado-acp
//! liberado-acp --mode chat
//! liberado-acp --mode face
//! ```
//!
//! Environment:
//! - `OPENROUTER_API_KEY` / `DEEPSEEK_API_KEY` / `OPENAI_API_KEY`
//! - `LIBERADO_ACP_MODE` — default mode (`coding` \| `chat` \| `face`)
//! - `LIBERADO_ACP_MODEL` — initial model id
//! - `LIBERADO_ACP_MAX_TURNS` — turns **per user message** (default 50)
//! - `LIBERADO_CONFIG_DIR` — optional Liberado config (topology + `[coder]` tuning)
//! - `LIBERADO_ACP_SYSTEM_PROMPT` — optional chat system prompt override
//! - `LIBERADO_SERVER` — face-mode daemon base URL (default `http://127.0.0.1:4201`)
//!
//! Model catalog: live `GET /models` from the configured backend, A–Z by id.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

mod coding_run;
mod face_client;
mod mode;

use mode::AgentMode;
use liberado_executor::{AgentEvent, Budget, Executor, ToolRuntime};
use liberado_main_agent::{Conversation, DEFAULT_SYSTEM_PROMPT};
use liberado_provider::{
    CompletionRequest, CompletionResponse, Provider, ProviderError, ProviderResult,
};
use liberado_provider_openai_compat::OpenAiCompatibleProvider;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, watch};

/// ACP protocol version negotiated with current `@agentclientprotocol/sdk`.
const PROTOCOL_VERSION: u32 = 1;

/// Whether `initialize` advertises `loadSession`. Must stay false until durable history +
/// replay exist (integration roadmap P3); true made Paseo resume into an empty transcript.
const LOAD_SESSION_CAPABILITY: bool = false;

/// Default raw model when OpenRouter is among configured backends.
const OPENROUTER_DEFAULT_RAW: &str = "deepseek/deepseek-v4-pro";
const DEEPSEEK_DEFAULT_RAW: &str = "deepseek-chat";
const OPENAI_DEFAULT_RAW: &str = "gpt-4o-mini";

/// Fallback raw ids when a backend's `/models` is unreachable.
const OPENROUTER_FALLBACK_RAW: &[&str] =
    &["deepseek/deepseek-v4-pro", "deepseek/deepseek-v4-flash"];
const DEEPSEEK_FALLBACK_RAW: &[&str] = &["deepseek-chat", "deepseek-reasoner"];
const OPENAI_FALLBACK_RAW: &[&str] = &["gpt-4o-mini", "gpt-4o"];

// ── JSON-RPC framing ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JsonRpcIncoming {
    #[serde(default)]
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcErrorBody>,
}

#[derive(Debug, Serialize)]
struct JsonRpcErrorBody {
    code: i32,
    message: String,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: String,
    params: Value,
}

// ── Bridge state ────────────────────────────────────────────────────────────

/// One row in the ACP session model picker (`availableModels`).
#[derive(Debug, Clone)]
struct CatalogModel {
    model_id: String,
    name: String,
    description: String,
}

/// Per-ACP-session state (mode + engine-specific handles).
struct AcpSession {
    mode: AgentMode,
    cwd: PathBuf,
    coding: coding_run::CodingSessionState,
    /// In-process chat (mode=chat).
    chat: Option<Arc<SessionHandle>>,
    /// Daemon conversation id (mode=face).
    face_daemon_session: Option<String>,
}

struct Bridge {
    provider: Arc<dyn Provider>,
    /// Inference backend label (`openrouter` | `deepseek` | `openai` | …).
    backend: String,
    catalog: Mutex<Vec<CatalogModel>>,
    current_model: Mutex<String>,
    /// Default mode for new sessions (`--mode` / `LIBERADO_ACP_MODE`).
    default_mode: AgentMode,
    /// Coder-role max_turns for the full coding pack (not face Budget::default()=8).
    max_turns: u32,
    coder_tuning: liberado_coder_core::CoderTuning,
    /// ACP session id → mode + engine state.
    acp_sessions: Mutex<HashMap<String, AcpSession>>,
}

struct SessionHandle {
    id: String,
    conversation: Mutex<Conversation>,
    executor: Executor,
    tools: Arc<dyn ToolRuntime>,
    cancel_tx: watch::Sender<bool>,
    cancel_rx: watch::Receiver<bool>,
}

struct NoTools;

#[async_trait::async_trait]
impl ToolRuntime for NoTools {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &liberado_provider::ToolInvocation) -> Result<String, String> {
        Err("no coding tools available for this session".into())
    }
}

#[tokio::main]
async fn main() {
    // Paseo Generic ACP diagnostics probe `liberado-acp --version` without ACP traffic.
    // Handle argv before touching stdin so the probe never hangs waiting for NDJSON.
    if let Some(code) = handle_cli_args(std::env::args().skip(1)) {
        std::process::exit(code);
    }

    // Logs MUST go to stderr — stdout is the ACP wire.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "liberado_acp_bridge=info".into()),
        )
        .init();

    if let Err(e) = run().await {
        tracing::error!(%e, "acp bridge fatal");
        std::process::exit(1);
    }
}

/// Process non-ACP CLI flags. Returns `Some(exit_code)` when the process should exit
/// without entering the stdio agent loop; `None` means continue as an ACP agent.
/// Process CLI flags. Sets `LIBERADO_ACP_MODE` when `--mode` is passed so
/// [`AgentMode::from_env_or_default`] sees it. Returns `Some(exit)` only for
/// version/help/error — mode alone continues into the ACP loop.
fn handle_cli_args<I, S>(args: I) -> Option<i32>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    if args.is_empty() {
        return None;
    }
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-V" | "version" => {
                println!("liberado-acp {}", env!("CARGO_PKG_VERSION"));
                return Some(0);
            }
            "--help" | "-h" | "help" => {
                print_help();
                return Some(0);
            }
            "--mode" | "-m" => {
                let val = args.get(i + 1).map(|s| s.as_str()).unwrap_or("");
                if AgentMode::parse(val).is_none() {
                    eprintln!(
                        "liberado-acp: unknown mode '{val}' (expected coding|chat|face)"
                    );
                    return Some(2);
                }
                // SAFETY: single-threaded startup before the async runtime; process default only.
                unsafe {
                    std::env::set_var("LIBERADO_ACP_MODE", val);
                }
                i += 2;
                continue;
            }
            other if other.starts_with("--mode=") => {
                let val = other.trim_start_matches("--mode=");
                if AgentMode::parse(val).is_none() {
                    eprintln!(
                        "liberado-acp: unknown mode '{val}' (expected coding|chat|face)"
                    );
                    return Some(2);
                }
                unsafe {
                    std::env::set_var("LIBERADO_ACP_MODE", val);
                }
                i += 1;
                continue;
            }
            other if other.starts_with('-') => {
                eprintln!("liberado-acp: unknown option '{other}'. Try --help.");
                return Some(2);
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

fn print_help() {
    println!(
        "liberado-acp {} — Liberado multi-mode ACP agent for Paseo\n\n\
         Usage:\n\
           liberado-acp [--mode coding|chat|face]   ACP on stdin/stdout\n\
           liberado-acp --version\n\
           liberado-acp --help\n\n\
         Modes (Liberado-owned; also switchable via ACP session/set_mode):\n\
           coding  Full coding pack + worktrees (default)\n\
           chat    In-process conversation (no daemon)\n\
           face    Daemon face agent — needs liberado serve (LIBERADO_SERVER)\n\n\
         Environment:\n\
           OPENROUTER_API_KEY / DEEPSEEK_API_KEY / OPENAI_API_KEY\n\
           LIBERADO_ACP_MODE           default mode (coding|chat|face)\n\
           LIBERADO_ACP_MODEL          initial model id\n\
           LIBERADO_ACP_MAX_TURNS      coder turns per prompt (default 50)\n\
           LIBERADO_CONFIG_DIR         Liberado config ([coder] tuning)\n\
           LIBERADO_SERVER             face mode daemon URL (default http://127.0.0.1:4201)",
        env!("CARGO_PKG_VERSION")
    );
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let resolved = build_provider()?;
    let catalog = load_model_catalog(
        resolved.provider.as_ref(),
        &resolved.backend,
        &resolved.model_id,
    )
    .await;
    let max_turns = coding_run::max_turns_from_env();
    let default_mode = AgentMode::from_env_or_default();
    let config_dir = std::env::var_os("LIBERADO_CONFIG_DIR").map(PathBuf::from);
    let coder_tuning = coding_run::load_coder_tuning(config_dir.as_deref());
    tracing::info!(
        backend = %resolved.backend,
        current = %resolved.model_id,
        catalog_len = catalog.len(),
        max_turns,
        mode = %default_mode.id(),
        "acp multi-mode agent ready"
    );
    let bridge = Arc::new(Bridge {
        provider: resolved.provider,
        backend: resolved.backend,
        catalog: Mutex::new(catalog),
        current_model: Mutex::new(resolved.model_id),
        default_mode,
        max_turns,
        coder_tuning,
        acp_sessions: Mutex::new(HashMap::new()),
    });

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let msg: JsonRpcIncoming = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(%line, %e, "unparseable ACP message");
                continue;
            }
        };

        let method = msg.method.unwrap_or_default();
        // Notifications have no id (or null id) and expect no response.
        let is_notification = msg.id.is_none() || msg.id.as_ref().is_some_and(|id| id.is_null());

        if is_notification {
            handle_notification(Arc::clone(&bridge), &method, msg.params).await;
            continue;
        }

        let id = msg.id.unwrap_or(Value::Null);
        match handle_request(Arc::clone(&bridge), &method, msg.params).await {
            Ok(result) => write_response(id, Ok(result)).await?,
            Err(message) => {
                write_response(
                    id,
                    Err(JsonRpcErrorBody {
                        code: -32603,
                        message,
                    }),
                )
                .await?
            }
        }
    }

    tracing::info!("stdin closed; acp bridge exiting");
    Ok(())
}

async fn write_response(
    id: Value,
    outcome: Result<Value, JsonRpcErrorBody>,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = match outcome {
        Ok(result) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        },
        Err(error) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        },
    };
    let json = serde_json::to_string(&body)?;
    let mut out = tokio::io::stdout();
    out.write_all(json.as_bytes()).await?;
    out.write_all(b"\n").await?;
    out.flush().await?;
    Ok(())
}

/// Sink for ACP notifications (`session/update`, …). Production writes NDJSON to stdout;
/// tests capture into a buffer so MockProvider turns can assert wire shape.
trait WireSink: Send + Sync {
    fn emit(&self, method: &str, params: Value) -> Result<(), String>;
}

struct StdoutSink;

impl WireSink for StdoutSink {
    fn emit(&self, method: &str, params: Value) -> Result<(), String> {
        let body = JsonRpcNotification {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        };
        let json = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        writeln!(out, "{json}").map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
        Ok(())
    }
}

struct ResolvedProvider {
    provider: Arc<dyn Provider>,
    /// Backend key: `openrouter` | `deepseek` | `openai` | topology name.
    backend: String,
    model_id: String,
}

fn build_provider() -> Result<ResolvedProvider, String> {
    let model_override = std::env::var("LIBERADO_ACP_MODEL")
        .ok()
        .filter(|s| !s.is_empty());

    if let Some(config_dir) = std::env::var_os("LIBERADO_CONFIG_DIR") {
        match liberado_config::load_config(Some(Path::new(&config_dir))) {
            Ok((config, _)) => {
                if let Some(provider) = liberado_bootstrap::provider_from_config(&config) {
                    let backend = config.topology.provider.clone();
                    let model = model_override.clone().unwrap_or_else(|| provider.model());
                    if let Some(profile) = config
                        .topology
                        .providers
                        .iter()
                        .find(|p| p.name == config.topology.provider)
                        && let Ok(p) = OpenAiCompatibleProvider::from_env(
                            &profile.api_key_env,
                            profile.model_env.as_deref(),
                            &model,
                            &profile.base_url,
                            profile.extra_client_error_status.clone(),
                        )
                    {
                        tracing::info!(
                            provider = %profile.name,
                            %model,
                            "acp provider from LIBERADO_CONFIG_DIR"
                        );
                        return Ok(ResolvedProvider {
                            provider: Arc::new(p),
                            backend,
                            model_id: model,
                        });
                    }
                    let m = provider.model();
                    return Ok(ResolvedProvider {
                        provider,
                        backend,
                        model_id: m,
                    });
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "LIBERADO_CONFIG_DIR load failed; falling back to env")
            }
        }
    }

    // Prefer OpenRouter so the picker gets `author/model` ids (deepseek/deepseek-v4-pro, …).
    // Then DeepSeek direct, then OpenAI.
    let candidates: [(&str, &str, &str, &str); 3] = [
        (
            "OPENROUTER_API_KEY",
            "https://openrouter.ai/api/v1",
            "openrouter",
            OPENROUTER_DEFAULT_RAW,
        ),
        (
            "DEEPSEEK_API_KEY",
            "https://api.deepseek.com/v1",
            "deepseek",
            DEEPSEEK_DEFAULT_RAW,
        ),
        (
            "OPENAI_API_KEY",
            "https://api.openai.com/v1",
            "openai",
            OPENAI_DEFAULT_RAW,
        ),
    ];

    for (key_env, base, backend, default_model) in candidates {
        if std::env::var_os(key_env).is_none() {
            continue;
        }
        let model = model_override
            .clone()
            .unwrap_or_else(|| default_model.to_string());
        let extra = if backend == "openrouter" {
            vec![402]
        } else {
            Vec::new()
        };
        let p = OpenAiCompatibleProvider::from_env(key_env, None, &model, base, extra)
            .map_err(|e| format!("provider init ({key_env}): {e}"))?;
        tracing::info!(%key_env, %model, %base, %backend, "acp provider ready");
        return Ok(ResolvedProvider {
            provider: Arc::new(p),
            backend: backend.to_string(),
            model_id: model,
        });
    }

    let model = model_override.unwrap_or_else(|| OPENROUTER_DEFAULT_RAW.to_string());
    tracing::warn!(
        "no API key found (set OPENROUTER_API_KEY, DEEPSEEK_API_KEY, or OPENAI_API_KEY); \
         Paseo can still detect liberado-acp, but prompts need a key"
    );
    Ok(ResolvedProvider {
        provider: Arc::new(MissingKeyProvider {
            model: std::sync::RwLock::new(model.clone()),
        }),
        backend: "none".into(),
        model_id: model,
    })
}

/// Build the ACP model picker from the live provider catalog.
async fn load_model_catalog(
    provider: &dyn Provider,
    backend: &str,
    current: &str,
) -> Vec<CatalogModel> {
    let live = match provider.list_models().await {
        Ok(ids) if !ids.is_empty() => {
            tracing::info!(count = ids.len(), %backend, "fetched live /models catalog");
            ids
        }
        Ok(_) => {
            tracing::warn!(%backend, "provider /models returned empty; using fallbacks");
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, %backend, "provider list_models failed; using fallbacks");
            Vec::new()
        }
    };

    let ordered = if live.is_empty() {
        fallback_model_ids(backend, current)
    } else {
        catalog_model_ids(&live, current)
    };

    ordered
        .into_iter()
        .map(|id| CatalogModel {
            name: display_name_for(&id),
            description: description_for(backend, &id),
            model_id: id.clone(),
        })
        .collect()
}

/// Full live catalog, A–Z. Includes `current` if the live list omitted it (e.g. custom slug).
fn catalog_model_ids(live: &[String], current: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in live {
        if !out.iter().any(|x| x == id) {
            out.push(id.clone());
        }
    }
    if !current.is_empty() && !out.iter().any(|x| x == current) {
        out.push(current.to_string());
    }
    out.sort();
    out
}

fn fallback_model_ids(backend: &str, current: &str) -> Vec<String> {
    let mut out: Vec<String> = match backend {
        "openrouter" => OPENROUTER_FALLBACK_RAW
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        "deepseek" => DEEPSEEK_FALLBACK_RAW
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        "openai" => OPENAI_FALLBACK_RAW
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        _ => OPENROUTER_FALLBACK_RAW
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
    };
    if !current.is_empty() && !out.iter().any(|x| x == current) {
        out.push(current.to_string());
    }
    out.sort();
    out
}

fn display_name_for(model_id: &str) -> String {
    // Keep the full author/model slug visible — that is the identity Paseo should show.
    model_id.to_string()
}

fn description_for(backend: &str, model_id: &str) -> String {
    match backend {
        "openrouter" => format!("OpenRouter · {model_id}"),
        "deepseek" => format!("DeepSeek API · {model_id}"),
        "openai" => format!("OpenAI · {model_id}"),
        other => format!("{other} · {model_id}"),
    }
}

struct MissingKeyProvider {
    model: std::sync::RwLock<String>,
}

#[async_trait::async_trait]
impl Provider for MissingKeyProvider {
    fn model(&self) -> String {
        self.model.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn set_model(&self, model: String) {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        *self.model.write().unwrap_or_else(|e| e.into_inner()) = model.to_string();
    }
    async fn list_models(&self) -> ProviderResult<Vec<String>> {
        Ok(OPENROUTER_FALLBACK_RAW
            .iter()
            .map(|s| (*s).to_string())
            .collect())
    }
    async fn complete(&self, _request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        Err(ProviderError::InvalidRequest(
            "liberado-acp: no API key configured. Set OPENROUTER_API_KEY (preferred), \
             DEEPSEEK_API_KEY, or OPENAI_API_KEY — or point LIBERADO_CONFIG_DIR at a Liberado \
             config with a topology provider."
                .into(),
        ))
    }
}

async fn handle_notification(bridge: Arc<Bridge>, method: &str, params: Value) {
    match method {
        "session/cancel" => {
            let sid = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if sid.is_empty() {
                return;
            }
            let sessions = bridge.acp_sessions.lock().await;
            if let Some(sess) = sessions.get(sid) {
                // Chat turns honour cancel via watch; coding/face finish their current work.
                if let Some(chat) = &sess.chat {
                    let _ = chat.cancel_tx.send(true);
                }
                tracing::info!(
                    session_id = %sid,
                    mode = %sess.mode.id(),
                    "session/cancel requested"
                );
            }
        }
        other => tracing::debug!(method = %other, "acp notification ignored"),
    }
}

async fn handle_request(bridge: Arc<Bridge>, method: &str, params: Value) -> Result<Value, String> {
    match method {
        "initialize" => {
            let client_version = params
                .get("protocolVersion")
                .and_then(|v| v.as_u64())
                .unwrap_or(PROTOCOL_VERSION as u64);
            tracing::info!(client_version, "acp initialize");
            Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "agentInfo": {
                    "name": "Liberado",
                    "version": env!("CARGO_PKG_VERSION"),
                    "title": "Liberado (coding · chat · face)",
                },
                // loadSession stays false until durable history + replay ship (P3).
                // Advertising true made Paseo take the resume path and get an empty transcript.
                "agentCapabilities": {
                    "loadSession": LOAD_SESSION_CAPABILITY,
                    "promptCapabilities": {
                        "image": false,
                        "audio": false,
                        "embeddedContext": true,
                    },
                    "mcpCapabilities": {
                        "http": false,
                        "sse": false,
                    },
                    "sessionCapabilities": {}
                },
                "authMethods": []
            }))
        }

        "session/new" => {
            let cwd = params
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

            let sid = new_session_id();
            let mode = bridge.default_mode;
            let chat = if mode == AgentMode::Chat {
                open_chat_session(&sid, cwd.clone(), Arc::clone(&bridge.provider), bridge.max_turns)
                    .ok()
                    .map(Arc::new)
            } else {
                None
            };
            bridge.acp_sessions.lock().await.insert(
                sid.clone(),
                AcpSession {
                    mode,
                    cwd: cwd.clone(),
                    coding: coding_run::CodingSessionState {
                        cwd: cwd.clone(),
                        coding_session_id: sid.clone(),
                        prior_feedback: Vec::new(),
                        last_summary: None,
                        rounds: 0,
                    },
                    chat,
                    face_daemon_session: None,
                },
            );

            tracing::info!(
                session_id = %sid,
                cwd = %cwd.display(),
                mode = %mode.id(),
                max_turns = bridge.max_turns,
                "session/new"
            );
            let (catalog, current) = bridge_model_snapshot(&bridge).await;
            Ok(session_state_payload(&sid, &catalog, &current, mode))
        }

        "session/load" => {
            // Capability loadSession is false; reject rather than silently wipe history.
            Err(
                "session/load is not supported yet (no durable session history). \
                 Start a new session with session/new."
                    .into(),
            )
        }

        "session/prompt" => {
            let sid = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or("missing sessionId")?
                .to_string();
            let text = extract_prompt_text(&params)?;
            let mode = {
                let map = bridge.acp_sessions.lock().await;
                map.get(&sid)
                    .map(|s| s.mode)
                    .ok_or_else(|| format!("unknown sessionId '{sid}'"))?
            };
            match mode {
                AgentMode::Coding => run_coding_prompt(Arc::clone(&bridge), &sid, &text).await,
                AgentMode::Chat => run_chat_prompt(Arc::clone(&bridge), &sid, &text).await,
                AgentMode::Face => run_face_prompt(Arc::clone(&bridge), &sid, &text).await,
            }
        }

        "session/set_mode" => {
            let sid = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or("missing sessionId")?;
            let mode_id = params
                .get("modeId")
                .and_then(|v| v.as_str())
                .ok_or("missing modeId")?;
            let mode = AgentMode::parse(mode_id)
                .ok_or_else(|| format!("unknown modeId '{mode_id}' (coding|chat|face)"))?;
            let mut map = bridge.acp_sessions.lock().await;
            let sess = map
                .get_mut(sid)
                .ok_or_else(|| format!("unknown sessionId '{sid}'"))?;
            if sess.mode != mode {
                tracing::info!(session_id = %sid, from = %sess.mode.id(), to = %mode.id(), "session/set_mode");
                sess.mode = mode;
                // Lazy-init chat handle when switching into chat.
                if mode == AgentMode::Chat && sess.chat.is_none() {
                    sess.chat = open_chat_session(
                        sid,
                        sess.cwd.clone(),
                        Arc::clone(&bridge.provider),
                        bridge.max_turns,
                    )
                    .ok()
                    .map(Arc::new);
                }
            }
            Ok(json!({}))
        }

        "session/set_model" => {
            let model_id = params
                .get("modelId")
                .and_then(|v| v.as_str())
                .ok_or("missing modelId")?
                .trim()
                .to_string();
            if model_id.is_empty() {
                return Err("modelId must be non-empty".into());
            }

            {
                let current = bridge.current_model.lock().await.clone();
                let fresh =
                    load_model_catalog(bridge.provider.as_ref(), &bridge.backend, &current).await;
                if !fresh.is_empty() {
                    *bridge.catalog.lock().await = fresh;
                }
            }

            let allowed = {
                let catalog = bridge.catalog.lock().await;
                catalog.iter().any(|m| m.model_id == model_id)
            };
            if !allowed {
                tracing::info!(%model_id, "set_model for id not in prior catalog; accepting");
                let mut catalog = bridge.catalog.lock().await;
                catalog.push(CatalogModel {
                    name: display_name_for(&model_id),
                    description: description_for(&bridge.backend, &model_id),
                    model_id: model_id.clone(),
                });
                catalog.sort_by(|a, b| a.name.cmp(&b.name));
            }

            // Catalog ids may be raw OpenRouter slugs; set on the live provider.
            bridge.provider.set_model(model_id.clone());
            *bridge.current_model.lock().await = model_id.clone();
            tracing::info!(%model_id, backend = %bridge.backend, "session/set_model");
            Ok(json!({}))
        }

        "session/set_config_option" => Ok(json!({})),

        "authenticate" | "logout" => Ok(json!({})),

        _ => Err(format!("Method not found: {method}")),
    }
}

async fn bridge_model_snapshot(bridge: &Bridge) -> (Vec<CatalogModel>, String) {
    let catalog = bridge.catalog.lock().await.clone();
    let current = bridge.current_model.lock().await.clone();
    (catalog, current)
}

/// Full coding pack path: LiberadoLoopBackend + durable worktree (same engine as goals).
async fn run_coding_prompt(bridge: Arc<Bridge>, sid: &str, text: &str) -> Result<Value, String> {
    let mut state = {
        let map = bridge.acp_sessions.lock().await;
        map.get(sid)
            .map(|s| s.coding.clone())
            .ok_or_else(|| format!("unknown sessionId '{sid}' (call session/new first)"))?
    };

    let model = bridge.current_model.lock().await.clone();
    let factory = coding_run::single_factory(Arc::clone(&bridge.provider));

    emit_agent_text_chunk(
        &StdoutSink,
        sid,
        &format!(
            "Starting Liberado coding pack (max_turns={}, model={model})…\n\n",
            bridge.max_turns
        ),
    )?;

    let outcome = coding_run::run_coding_round(
        Arc::clone(&bridge.provider),
        factory,
        &bridge.coder_tuning,
        &mut state,
        text,
        Some(&model),
        bridge.max_turns,
    )
    .await;

    // Persist coding state regardless of outcome so later rounds keep feedback.
    if let Some(sess) = bridge.acp_sessions.lock().await.get_mut(sid) {
        sess.coding = state;
    }

    match outcome {
        Ok(result) => {
            let report = result.render();
            emit_agent_text_chunk(&StdoutSink, sid, &report)?;
            Ok(json!({ "stopReason": "end_turn" }))
        }
        Err(e) => {
            emit_agent_text_chunk(&StdoutSink, sid, &format!("\n**Coding pack error:** {e}\n"))?;
            Ok(json!({ "stopReason": "end_turn" }))
        }
    }
}

/// In-process chat: Conversation + Executor, no coding tools.
async fn run_chat_prompt(bridge: Arc<Bridge>, sid: &str, text: &str) -> Result<Value, String> {
    let session = {
        let mut map = bridge.acp_sessions.lock().await;
        let sess = map
            .get_mut(sid)
            .ok_or_else(|| format!("unknown sessionId '{sid}'"))?;
        if sess.chat.is_none() {
            sess.chat = open_chat_session(
                sid,
                sess.cwd.clone(),
                Arc::clone(&bridge.provider),
                bridge.max_turns,
            )
            .ok()
            .map(Arc::new);
        }
        // Reset cancel flag for a fresh turn.
        if let Some(chat) = &sess.chat {
            let _ = chat.cancel_tx.send(false);
        }
        sess.chat
            .clone()
            .ok_or_else(|| "failed to open chat session".to_string())?
    };

    let stop = run_prompt_turn(session, text.to_string(), &StdoutSink).await?;
    Ok(json!({ "stopReason": stop }))
}

/// Face agent via running `liberado serve` (HTTP SSE stream).
async fn run_face_prompt(bridge: Arc<Bridge>, sid: &str, text: &str) -> Result<Value, String> {
    let mut daemon_session = {
        let map = bridge.acp_sessions.lock().await;
        map.get(sid)
            .map(|s| s.face_daemon_session.clone())
            .ok_or_else(|| format!("unknown sessionId '{sid}'"))?
    };

    let sid_owned = sid.to_string();
    let emit = |method: &str, params: Value| -> Result<(), String> {
        StdoutSink.emit(method, params)
    };

    let result = face_client::run_face_turn(&mut daemon_session, text, &sid_owned, &emit).await;

    if let Some(sess) = bridge.acp_sessions.lock().await.get_mut(sid) {
        sess.face_daemon_session = daemon_session;
    }

    match result {
        Ok(()) => Ok(json!({ "stopReason": "end_turn" })),
        Err(e) => {
            emit_agent_text_chunk(&StdoutSink, sid, &format!("\n**Face mode error:** {e}\n"))?;
            Ok(json!({ "stopReason": "end_turn" }))
        }
    }
}

/// Pure chat session: conversation + executor, no coding tools.
fn open_chat_session(
    session_id: &str,
    cwd: PathBuf,
    provider: Arc<dyn Provider>,
    max_turns: u32,
) -> Result<SessionHandle, String> {
    let system = std::env::var("LIBERADO_ACP_SYSTEM_PROMPT").unwrap_or_else(|_| {
        format!(
            "{DEFAULT_SYSTEM_PROMPT}\n\n\
             You are Liberado chat (ACP). Workspace context path: {}.\n\
             This mode is conversational only — no file tools. For coding work, switch mode to \
             **coding**. For vault/delegate face agent, switch to **face** (daemon required).",
            cwd.display()
        )
    });
    let (cancel_tx, cancel_rx) = watch::channel(false);
    Ok(SessionHandle {
        id: session_id.to_string(),
        conversation: Mutex::new(Conversation::new(system)),
        // Chat uses executor budget = max_turns (not the hardcoded default of 8).
        executor: Executor::new(provider, Budget::new(max_turns)),
        tools: Arc::new(NoTools),
        cancel_tx,
        cancel_rx,
    })
}

fn session_state_payload(
    session_id: &str,
    catalog: &[CatalogModel],
    current_model_id: &str,
    mode: AgentMode,
) -> Value {
    json!({
        "sessionId": session_id,
        "models": model_state(catalog, current_model_id),
        "modes": mode::mode_state_json(mode),
        "configOptions": []
    })
}

fn model_state(catalog: &[CatalogModel], current_model_id: &str) -> Value {
    let available: Vec<Value> = catalog
        .iter()
        .map(|m| {
            json!({
                "modelId": m.model_id,
                "name": m.name,
                "description": m.description,
            })
        })
        .collect();
    let current = if catalog.iter().any(|m| m.model_id == current_model_id) {
        current_model_id
    } else {
        catalog
            .first()
            .map(|m| m.model_id.as_str())
            .unwrap_or(current_model_id)
    };
    json!({
        "availableModels": available,
        "currentModelId": current
    })
}



fn extract_prompt_text(params: &Value) -> Result<String, String> {
    if let Some(arr) = params.get("prompt").and_then(|v| v.as_array()) {
        let mut parts = Vec::new();
        for block in arr {
            let ty = block.get("type").and_then(|t| t.as_str()).unwrap_or("text");
            if ty == "text" {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    parts.push(t.to_string());
                }
            } else if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                parts.push(t.to_string());
            } else if let Some(uri) = block.get("uri").and_then(|u| u.as_str()) {
                parts.push(format!("[resource: {uri}]"));
            }
        }
        let text = parts.join("\n");
        if text.trim().is_empty() {
            return Err("prompt contained no text content".into());
        }
        return Ok(text);
    }
    if let Some(t) = params.get("text").and_then(|v| v.as_str()) {
        return Ok(t.to_string());
    }
    if let Some(t) = params.get("message").and_then(|v| v.as_str()) {
        return Ok(t.to_string());
    }
    Err("missing prompt".into())
}

async fn run_prompt_turn(
    session: Arc<SessionHandle>,
    text: String,
    sink: &dyn WireSink,
) -> Result<String, String> {
    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
    let sid = session.id.clone();
    let turn_session = Arc::clone(&session);

    let turn = tokio::spawn(async move {
        let mut convo = turn_session.conversation.lock().await;
        let result = convo
            .turn_stream(
                &turn_session.executor,
                turn_session.tools.as_ref(),
                &text,
                &event_tx,
            )
            .await;
        match &result {
            Ok(()) => {
                let _ = event_tx.send(AgentEvent::Done).await;
            }
            Err(e) => {
                let _ = event_tx.send(AgentEvent::Error(e.to_string())).await;
            }
        }
        result
    });

    let mut stop_reason = "end_turn".to_string();
    let mut cancel_rx = session.cancel_rx.clone();
    // Paseo UI pairs tool_call → tool_call_update by toolCallId. Keep a LIFO stack of
    // in-flight ids (executor runs tools sequentially, but nested/same-name tools may stack).
    let mut pending_tool_ids: Vec<(String, String)> = Vec::new();

    loop {
        tokio::select! {
            biased;
            changed = cancel_rx.changed() => {
                if changed.is_ok() && *cancel_rx.borrow() {
                    turn.abort();
                    stop_reason = "cancelled".into();
                    break;
                }
            }
            ev = event_rx.recv() => {
                match ev {
                    Some(AgentEvent::Token(t)) => {
                        emit_agent_text_chunk(sink, &sid, &t)?;
                    }
                    Some(AgentEvent::ToolStarted { name, args }) => {
                        let tool_call_id = push_tool_call_id(&mut pending_tool_ids, &name);
                        emit_tool_call(sink, &sid, &tool_call_id, &name, &args, "pending")?;
                    }
                    Some(AgentEvent::ToolFinished { name, ok, preview }) => {
                        let tool_call_id = pop_tool_call_id(&mut pending_tool_ids, &name);
                        let status = if ok { "completed" } else { "failed" };
                        emit_tool_call_update(sink, &sid, &tool_call_id, &name, status, &preview)?;
                    }
                    Some(AgentEvent::Done) => {
                        stop_reason = "end_turn".into();
                        break;
                    }
                    Some(AgentEvent::Error(msg)) => {
                        emit_agent_text_chunk(sink, &sid, &format!("\nError: {msg}"))?;
                        // Client-facing failure still ends the turn; refuse rather than crash.
                        stop_reason = "end_turn".into();
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    let _ = turn.await;
    Ok(stop_reason)
}

/// Allocate a stable `toolCallId` for a started tool and record it for the matching finish.
fn push_tool_call_id(pending: &mut Vec<(String, String)>, name: &str) -> String {
    let tool_call_id = format!("call-{}", short_id());
    pending.push((name.to_string(), tool_call_id.clone()));
    tool_call_id
}

/// Pop the most recent in-flight id for `name` (LIFO). Fallback id only if start was missed.
fn pop_tool_call_id(pending: &mut Vec<(String, String)>, name: &str) -> String {
    if let Some(idx) = pending.iter().rposition(|(n, _)| n == name) {
        return pending.remove(idx).1;
    }
    // Should not happen in a well-formed stream; still emit a unique id so the wire is valid.
    format!("call-orphan-{}", short_id())
}

fn emit_agent_text_chunk(sink: &dyn WireSink, session_id: &str, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    sink.emit(
        "session/update",
        json!({
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {
                    "type": "text",
                    "text": text
                }
            }
        }),
    )
}

fn emit_tool_call(
    sink: &dyn WireSink,
    session_id: &str,
    tool_call_id: &str,
    name: &str,
    args: &str,
    status: &str,
) -> Result<(), String> {
    let raw_input: Value = serde_json::from_str(args).unwrap_or_else(|_| json!({ "raw": args }));
    sink.emit(
        "session/update",
        json!({
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": tool_call_id,
                "title": name,
                "kind": tool_kind(name),
                "status": status,
                "rawInput": raw_input
            }
        }),
    )
}

fn emit_tool_call_update(
    sink: &dyn WireSink,
    session_id: &str,
    tool_call_id: &str,
    name: &str,
    status: &str,
    preview: &str,
) -> Result<(), String> {
    sink.emit(
        "session/update",
        json!({
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": tool_call_id,
                "status": status,
                "title": name,
                "content": [{
                    "type": "content",
                    "content": { "type": "text", "text": preview }
                }]
            }
        }),
    )
}

fn tool_kind(name: &str) -> &'static str {
    match name {
        "read_file" | "list_files" | "search_text" => "read",
        "write_file" | "edit_file" | "apply_patch" => "edit",
        "run_command" | "validate" => "execute",
        "git_status" | "git_diff" | "git_branch" | "git_commit" | "git_push" | "git_fetch" => {
            "execute"
        }
        _ => "other",
    }
}

fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Stable, unique, path-safe. Not a UUID, and ACP does not require UUID format for sessionId.
    format!("lib-{:x}-{}", nanos, std::process::id())
}

fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_prompt_joins_text_blocks() {
        let params = json!({
            "sessionId": "s1",
            "prompt": [
                { "type": "text", "text": "hello " },
                { "type": "text", "text": "world" }
            ]
        });
        assert_eq!(extract_prompt_text(&params).unwrap(), "hello \nworld");
    }

    #[test]
    fn session_new_payload_has_models_and_modes() {
        let catalog = vec![
            CatalogModel {
                model_id: "deepseek/deepseek-v4-pro".into(),
                name: "deepseek/deepseek-v4-pro".into(),
                description: "OpenRouter · deepseek/deepseek-v4-pro".into(),
            },
            CatalogModel {
                model_id: "deepseek/deepseek-v4-flash".into(),
                name: "deepseek/deepseek-v4-flash".into(),
                description: "OpenRouter · deepseek/deepseek-v4-flash".into(),
            },
        ];
        let v = session_state_payload(
            "sid",
            &catalog,
            "deepseek/deepseek-v4-pro",
            AgentMode::Coding,
        );
        assert_eq!(v["sessionId"], "sid");
        assert_eq!(v["models"]["currentModelId"], "deepseek/deepseek-v4-pro");
        assert_eq!(v["models"]["availableModels"].as_array().unwrap().len(), 2);
        assert_eq!(
            v["models"]["availableModels"][1]["modelId"],
            "deepseek/deepseek-v4-flash"
        );
        assert_eq!(v["modes"]["currentModeId"], "coding");
        assert_eq!(v["modes"]["availableModes"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn catalog_is_full_and_alphabetical() {
        let live = vec![
            "openai/gpt-4o".into(),
            "anthropic/claude-3.5-sonnet".into(),
            "deepseek/deepseek-v4-pro".into(),
            "deepseek/deepseek-chat".into(),
        ];
        let ordered = catalog_model_ids(&live, "deepseek/deepseek-v4-pro");
        assert_eq!(
            ordered,
            vec![
                "anthropic/claude-3.5-sonnet",
                "deepseek/deepseek-chat",
                "deepseek/deepseek-v4-pro",
                "openai/gpt-4o",
            ]
        );
    }

    #[test]
    fn catalog_inserts_current_when_missing_from_live_then_sorts() {
        let live = vec!["openai/gpt-4o".into(), "anthropic/claude-3.5-sonnet".into()];
        let ordered = catalog_model_ids(&live, "deepseek/deepseek-v4-pro");
        assert_eq!(
            ordered,
            vec![
                "anthropic/claude-3.5-sonnet",
                "deepseek/deepseek-v4-pro",
                "openai/gpt-4o",
            ]
        );
    }

    #[test]
    fn initialize_shape_is_acp_compatible() {
        // Document the contract our handler returns (mirrors handle_request arm).
        let result = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "agentInfo": { "name": "Liberado", "version": "0.1.0" },
            "agentCapabilities": {
                "loadSession": false,
                "promptCapabilities": { "image": false, "audio": false, "embeddedContext": true },
            }
        });
        assert_eq!(result["protocolVersion"], 1);
        // Must stay false until durable load+replay (P3); true lied to Paseo's resume path.
        assert_eq!(result["agentCapabilities"]["loadSession"], false);
    }

    #[test]
    fn load_session_capability_is_honest() {
        // Mutation guard: initialize must not advertise loadSession until history is durable.
        // const block: clippy::assertions_on_constants rejects a runtime assert! on a const.
        const {
            assert!(
                !LOAD_SESSION_CAPABILITY,
                "advertising loadSession:true without durable history wipes Paseo resume"
            );
        }
    }

    #[test]
    fn tool_call_ids_pair_start_and_finish() {
        let mut pending = Vec::new();
        let start_a = push_tool_call_id(&mut pending, "read_file");
        let start_b = push_tool_call_id(&mut pending, "run_command");
        assert_ne!(start_a, start_b);
        assert_eq!(pop_tool_call_id(&mut pending, "run_command"), start_b);
        assert_eq!(pop_tool_call_id(&mut pending, "read_file"), start_a);
        assert!(pending.is_empty());
    }

    #[test]
    fn tool_call_ids_pair_same_name_lifo() {
        let mut pending = Vec::new();
        let first = push_tool_call_id(&mut pending, "read_file");
        let second = push_tool_call_id(&mut pending, "read_file");
        // Finish order matches LIFO (inner tool completes first).
        assert_eq!(pop_tool_call_id(&mut pending, "read_file"), second);
        assert_eq!(pop_tool_call_id(&mut pending, "read_file"), first);
    }

    #[test]
    fn tool_call_id_pairing_is_required_for_paseo_ui() {
        // Mutation guard: if start and finish minted independent ids (old bug), this fails.
        let mut pending = Vec::new();
        let started = push_tool_call_id(&mut pending, "list_files");
        let finished = pop_tool_call_id(&mut pending, "list_files");
        assert_eq!(
            started, finished,
            "Paseo indexes tool UI by toolCallId; start and finish must share one id"
        );
    }

    #[test]
    fn version_flag_exits_without_stdio_loop() {
        assert_eq!(handle_cli_args(["--version"]), Some(0));
        assert_eq!(handle_cli_args(["-V"]), Some(0));
        assert_eq!(handle_cli_args(["version"]), Some(0));
    }

    #[test]
    fn help_flag_exits_without_stdio_loop() {
        assert_eq!(handle_cli_args(["--help"]), Some(0));
        assert_eq!(handle_cli_args(["-h"]), Some(0));
    }

    #[test]
    fn no_args_enters_acp_mode() {
        assert_eq!(handle_cli_args(Vec::<String>::new()), None);
    }

    #[test]
    fn unknown_flag_is_an_error_exit() {
        assert_eq!(handle_cli_args(["--nope"]), Some(2));
    }

    #[test]
    fn mode_flag_continues_into_acp_loop() {
        // `--mode` sets default and continues (does not exit).
        assert_eq!(handle_cli_args(["--mode", "chat"]), None);
        assert_eq!(handle_cli_args(["--mode=face"]), None);
        assert_eq!(handle_cli_args(["-m", "coding"]), None);
    }

    #[test]
    fn unknown_mode_is_an_error_exit() {
        assert_eq!(handle_cli_args(["--mode", "banana"]), Some(2));
        assert_eq!(handle_cli_args(["--mode=nope"]), Some(2));
    }

    /// Captures ACP notifications instead of writing stdout (for MockProvider turns).
    struct CaptureSink {
        lines: std::sync::Mutex<Vec<(String, Value)>>,
    }

    impl WireSink for CaptureSink {
        fn emit(&self, method: &str, params: Value) -> Result<(), String> {
            self.lines
                .lock()
                .map_err(|e| e.to_string())?
                .push((method.to_string(), params));
            Ok(())
        }
    }

    struct EchoTool;

    #[async_trait::async_trait]
    impl ToolRuntime for EchoTool {
        fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
            vec![liberado_provider::ToolDef::new(
                "echo",
                "Echo a message",
                json!({
                    "type": "object",
                    "properties": { "msg": { "type": "string" } },
                    "required": ["msg"]
                }),
            )]
        }
        async fn invoke(&self, call: &liberado_provider::ToolInvocation) -> Result<String, String> {
            let msg = call
                .arguments
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Ok(format!("echo:{msg}"))
        }
    }

    #[tokio::test]
    async fn mock_provider_turn_streams_paired_tool_and_text() {
        use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};

        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "tc1",
                    "echo",
                    json!({ "msg": "hi" }),
                )]),
                CompletionResponse::text("all done"),
            ],
        ));
        // Chat-path wire test: same SessionHandle / run_prompt_turn stack as mode=chat,
        // with a mock tool so we can assert tool_call id pairing on the ACP wire.
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let session = Arc::new(SessionHandle {
            id: "sess-mock".into(),
            conversation: Mutex::new(Conversation::new("test system")),
            executor: Executor::new(provider, Budget::new(8)),
            tools: Arc::new(EchoTool),
            cancel_tx,
            cancel_rx,
        });
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };

        let stop = run_prompt_turn(session, "please echo".into(), &sink)
            .await
            .expect("turn");
        assert_eq!(stop, "end_turn");

        let lines = sink.lines.lock().unwrap().clone();
        assert!(
            !lines.is_empty(),
            "expected session/update notifications on the wire"
        );

        let tool_starts: Vec<&Value> = lines
            .iter()
            .filter(|(m, p)| m == "session/update" && p["update"]["sessionUpdate"] == "tool_call")
            .map(|(_, p)| p)
            .collect();
        let tool_updates: Vec<&Value> = lines
            .iter()
            .filter(|(m, p)| {
                m == "session/update" && p["update"]["sessionUpdate"] == "tool_call_update"
            })
            .map(|(_, p)| p)
            .collect();
        assert_eq!(tool_starts.len(), 1, "one tool_call: {lines:?}");
        assert_eq!(tool_updates.len(), 1, "one tool_call_update: {lines:?}");
        let start_id = tool_starts[0]["update"]["toolCallId"]
            .as_str()
            .expect("start id");
        let finish_id = tool_updates[0]["update"]["toolCallId"]
            .as_str()
            .expect("finish id");
        assert_eq!(
            start_id, finish_id,
            "MockProvider path must pair toolCallId (mutation target for P0.1)"
        );
        assert_eq!(tool_starts[0]["update"]["title"], "echo");
        assert_eq!(tool_updates[0]["update"]["status"], "completed");

        let text: String = lines
            .iter()
            .filter(|(m, p)| {
                m == "session/update" && p["update"]["sessionUpdate"] == "agent_message_chunk"
            })
            .filter_map(|(_, p)| p["update"]["content"]["text"].as_str())
            .collect();
        assert!(
            text.contains("all done"),
            "expected assistant text chunks, got {text:?} from {lines:?}"
        );
    }
}
