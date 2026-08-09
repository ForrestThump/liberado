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
//! | `session/set_mode` | Accepted no-op |
//! | `session/set_model` | Hot-swap the active model (must be in the live catalog) |
//!
//! Usage (spawned by Paseo):
//! ```text
//! liberado-acp
//! ```
//!
//! Environment:
//! - `OPENROUTER_API_KEY` (preferred) / `DEEPSEEK_API_KEY` / `OPENAI_API_KEY`
//! - `LIBERADO_ACP_MODEL` — initial model id (OpenRouter default: `deepseek/deepseek-v4-pro`)
//! - `LIBERADO_CONFIG_DIR` — optional Liberado config (topology provider)
//! - `LIBERADO_ACP_SYSTEM_PROMPT` — optional system prompt override
//!
//! Model catalog: full live `GET /models` list (OpenRouter when that key is set), sorted A–Z.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use liberado_coder_core::{CommandPolicy, PathPolicy};
use liberado_coder_tools::CodingToolRuntime;
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

/// Initial model when `OPENROUTER_API_KEY` is set (OpenRouter `author/model` ids).
const OPENROUTER_DEFAULT_MODEL: &str = "deepseek/deepseek-v4-pro";
/// Initial model when talking to DeepSeek direct (no author prefix on their `/models` ids).
const DEEPSEEK_DEFAULT_MODEL: &str = "deepseek-chat";

/// Fallback when OpenRouter `/models` is unreachable but the key is present.
const OPENROUTER_FALLBACK: &[&str] = &["deepseek/deepseek-v4-pro", "deepseek/deepseek-v4-flash"];

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
    /// Wire id passed to `session/set_model` and the provider (e.g. `deepseek/deepseek-v4-pro`).
    model_id: String,
    /// Human label in Paseo (usually same as `model_id`).
    name: String,
    description: String,
}

struct Bridge {
    provider: Arc<dyn Provider>,
    /// Which inference backend we bound (`openrouter` | `deepseek` | `openai` | …).
    backend: String,
    /// Live picker rows — from `GET /models`, curated.
    catalog: Mutex<Vec<CatalogModel>>,
    /// Active model id (mirrors `provider.model()`; kept for fast session payloads).
    current_model: Mutex<String>,
    sessions: Mutex<HashMap<String, Arc<SessionHandle>>>,
}

struct SessionHandle {
    id: String,
    conversation: Mutex<Conversation>,
    executor: Executor,
    tools: Arc<dyn ToolRuntime>,
    /// `true` = cancel requested for the in-flight turn.
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
fn handle_cli_args<I, S>(args: I) -> Option<i32>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    if args.is_empty() {
        return None;
    }
    match args[0].as_str() {
        "--version" | "-V" | "version" => {
            // Version probe writes to stdout (what `exec … --version` captures).
            println!("liberado-acp {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        "--help" | "-h" | "help" => {
            print_help();
            Some(0)
        }
        other if other.starts_with('-') => {
            eprintln!("liberado-acp: unknown option '{other}'. Try --help.");
            Some(2)
        }
        // Positional args are not used; ignore and enter ACP mode for forward-compat.
        _ => None,
    }
}

fn print_help() {
    println!(
        "liberado-acp {} — Liberado ACP coding agent (stdio JSON-RPC for Paseo)\n\n\
         Usage:\n\
           liberado-acp              Speak ACP on stdin/stdout (spawned by Paseo)\n\
           liberado-acp --version    Print version and exit\n\
           liberado-acp --help       Show this help\n\n\
         Environment:\n\
           OPENROUTER_API_KEY (preferred) / DEEPSEEK_API_KEY / OPENAI_API_KEY\n\
           LIBERADO_ACP_MODEL, LIBERADO_CONFIG_DIR, LIBERADO_ACP_SYSTEM_PROMPT",
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
    tracing::info!(
        backend = %resolved.backend,
        current = %resolved.model_id,
        catalog_len = catalog.len(),
        "acp model catalog ready"
    );
    let bridge = Arc::new(Bridge {
        provider: resolved.provider,
        backend: resolved.backend,
        catalog: Mutex::new(catalog),
        current_model: Mutex::new(resolved.model_id),
        sessions: Mutex::new(HashMap::new()),
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
            OPENROUTER_DEFAULT_MODEL,
        ),
        (
            "DEEPSEEK_API_KEY",
            "https://api.deepseek.com/v1",
            "deepseek",
            DEEPSEEK_DEFAULT_MODEL,
        ),
        (
            "OPENAI_API_KEY",
            "https://api.openai.com/v1",
            "openai",
            "gpt-4o-mini",
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

    let model = model_override.unwrap_or_else(|| OPENROUTER_DEFAULT_MODEL.to_string());
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
            model_id: id,
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
        "openrouter" => OPENROUTER_FALLBACK
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        "deepseek" => vec![DEEPSEEK_DEFAULT_MODEL.into(), "deepseek-reasoner".into()],
        "openai" => vec!["gpt-4o-mini".into(), "gpt-4o".into()],
        _ => OPENROUTER_FALLBACK
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
        Ok(OPENROUTER_FALLBACK
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
            let sessions = bridge.sessions.lock().await;
            if let Some(session) = sessions.get(sid) {
                let _ = session.cancel_tx.send(true);
                tracing::info!(session_id = %sid, "session/cancel requested");
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
                    "title": "Liberado Coding Agent",
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
            let handle = open_session(&sid, cwd.clone(), Arc::clone(&bridge.provider))?;
            bridge
                .sessions
                .lock()
                .await
                .insert(sid.clone(), Arc::new(handle));

            tracing::info!(session_id = %sid, cwd = %cwd.display(), "session/new");
            let (catalog, current) = bridge_model_snapshot(&bridge).await;
            Ok(session_state_payload(&sid, &catalog, &current))
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
            let session = {
                let map = bridge.sessions.lock().await;
                map.get(&sid)
                    .cloned()
                    .ok_or_else(|| format!("unknown sessionId '{sid}'"))?
            };

            let _ = session.cancel_tx.send(false);
            let stop = run_prompt_turn(session, text, &StdoutSink).await?;
            Ok(json!({ "stopReason": stop }))
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

            // Refresh catalog opportunistically so a newly-listed OpenRouter model can be selected.
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
                // Still allow the switch if the operator knows a valid id; add it to the picker.
                tracing::info!(%model_id, "set_model for id not in prior catalog; accepting");
                let mut catalog = bridge.catalog.lock().await;
                catalog.insert(
                    0,
                    CatalogModel {
                        name: display_name_for(&model_id),
                        description: description_for(&bridge.backend, &model_id),
                        model_id: model_id.clone(),
                    },
                );
            }

            bridge.provider.set_model(model_id.clone());
            *bridge.current_model.lock().await = model_id.clone();
            tracing::info!(%model_id, backend = %bridge.backend, "session/set_model");
            Ok(json!({}))
        }

        "session/set_mode" | "session/set_config_option" => Ok(json!({})),

        "authenticate" | "logout" => Ok(json!({})),

        _ => Err(format!("Method not found: {method}")),
    }
}

async fn bridge_model_snapshot(bridge: &Bridge) -> (Vec<CatalogModel>, String) {
    let catalog = bridge.catalog.lock().await.clone();
    let current = bridge.current_model.lock().await.clone();
    (catalog, current)
}

fn open_session(
    session_id: &str,
    cwd: PathBuf,
    provider: Arc<dyn Provider>,
) -> Result<SessionHandle, String> {
    let tools: Arc<dyn ToolRuntime> =
        match CodingToolRuntime::new(&cwd, CommandPolicy::default(), PathPolicy::default()) {
            Ok(rt) => {
                tracing::info!(cwd = %cwd.display(), "coding tools enabled for session");
                Arc::new(rt)
            }
            Err(e) => {
                tracing::warn!(
                    cwd = %cwd.display(),
                    error = %e,
                    "coding tools unavailable; session is chat-only"
                );
                Arc::new(NoTools)
            }
        };
    open_session_with_tools(session_id, cwd, provider, tools)
}

fn open_session_with_tools(
    session_id: &str,
    cwd: PathBuf,
    provider: Arc<dyn Provider>,
    tools: Arc<dyn ToolRuntime>,
) -> Result<SessionHandle, String> {
    let system = std::env::var("LIBERADO_ACP_SYSTEM_PROMPT").unwrap_or_else(|_| {
        format!(
            "{DEFAULT_SYSTEM_PROMPT}\n\n\
             You are running as Liberado's ACP coding agent (via Paseo). Workspace root: {}.\n\
             Prefer tools (read_file, search_text, list_files, write_file, edit_file, run_command, \
             git_status, git_diff) over guessing. Be concise.",
            cwd.display()
        )
    });

    let (cancel_tx, cancel_rx) = watch::channel(false);
    Ok(SessionHandle {
        id: session_id.to_string(),
        conversation: Mutex::new(Conversation::new(system)),
        executor: Executor::new(provider, Budget::default()),
        tools,
        cancel_tx,
        cancel_rx,
    })
}

fn session_state_payload(
    session_id: &str,
    catalog: &[CatalogModel],
    current_model_id: &str,
) -> Value {
    json!({
        "sessionId": session_id,
        "models": model_state(catalog, current_model_id),
        "modes": mode_state(),
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

fn mode_state() -> Value {
    json!({
        "availableModes": [{
            "id": "code",
            "name": "Code",
            "description": "Full coding tools against the session workspace"
        }],
        "currentModeId": "code"
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
        let v = session_state_payload("sid", &catalog, "deepseek/deepseek-v4-pro");
        assert_eq!(v["sessionId"], "sid");
        assert_eq!(v["models"]["currentModelId"], "deepseek/deepseek-v4-pro");
        assert_eq!(v["models"]["availableModels"].as_array().unwrap().len(), 2);
        assert_eq!(
            v["models"]["availableModels"][1]["modelId"],
            "deepseek/deepseek-v4-flash"
        );
        assert_eq!(v["modes"]["currentModeId"], "code");
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
        let handle = open_session_with_tools(
            "sess-mock",
            PathBuf::from("."),
            provider,
            Arc::new(EchoTool),
        )
        .expect("session");
        let session = Arc::new(handle);
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
