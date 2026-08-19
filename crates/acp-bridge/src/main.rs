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
//! | `session/load` | Advertised (`loadSession: true`); restores stored history into model conversation |
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
//! - `LIBERADO_ACP_MAX_TURNS` — per-launch override of `[acp] max_turns`
//! - `LIBERADO_CONFIG_DIR` — optional Liberado config (topology + `[coder]` tuning)
//! - `LIBERADO_SERVER` — face-mode daemon base URL (default `http://127.0.0.1:4201`)
//!
//! Model catalog: live `GET /models` from the configured backend, A–Z by id.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

mod coding_run;
mod provider;
mod session_store;
mod stdin_guard;
mod wire;

use wire::{
    JsonRpcErrorBody, JsonRpcIncoming, StdoutWire, WireSink, emit_agent_text_chunk, emit_tool_call,
    emit_tool_call_update, emit_user_message_chunk, pop_tool_call_id, push_tool_call_id,
};

use provider::{
    CatalogModel, build_provider, description_for, display_name_for, load_model_catalog,
};
mod face_client;
mod mode;

use liberado_executor::{AgentEvent, Budget, Executor, ToolRuntime};
use liberado_main_agent::{Conversation, DEFAULT_SYSTEM_PROMPT};
use liberado_provider::Provider;
use mode::AgentMode;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, mpsc, watch};

/// ACP protocol version negotiated with current `@agentclientprotocol/sdk`.
const PROTOCOL_VERSION: u32 = 1;

/// Whether `initialize` advertises `loadSession`. Now true — durable history and replay
/// are implemented in `session/load`.
const LOAD_SESSION_CAPABILITY: bool = true;

/// JSON-RPC 2.0 error codes. Named because "-32602" at a call site says nothing about which of the
/// spec's four failure kinds it is, and every one of these used to be -32603.
const JSONRPC_METHOD_NOT_FOUND: i32 = -32601;
const JSONRPC_INVALID_PARAMS: i32 = -32602;
const JSONRPC_INTERNAL_ERROR: i32 = -32603;

/// Marks a `handle_request` error as "no such method" so the wire layer can pick -32601.
const METHOD_NOT_FOUND_PREFIX: &str = "Method not found: ";

// ── Bridge state ────────────────────────────────────────────────────────────

/// One row in the ACP session model picker (`availableModels`).
/// Per-ACP-session state (mode + engine-specific handles).
struct AcpSession {
    mode: AgentMode,
    cwd: PathBuf,
    coding: coding_run::CodingSessionState,
    /// In-process chat (mode=chat).
    chat: Option<Arc<SessionHandle>>,
    /// Daemon conversation id (mode=face).
    face_daemon_session: Option<String>,
    /// Cooperative cancel for the in-flight turn (coding / chat / face).
    cancel_tx: watch::Sender<bool>,
    cancel_rx: watch::Receiver<bool>,
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
    /// `LIBERADO_CONFIG_DIR`, kept so a coding round can resolve which declared project it is in
    /// and therefore which ship bar it must clear. `None` is standalone: no topology, no bar.
    config_dir: Option<PathBuf>,
    /// Declared authority for coding mode (`policy.toml` `coding-local`), empty when standalone.
    ///
    /// Surfaced in `session/new` so the editor can show what this agent may do. Deeper enforcement
    /// — mapping capabilities onto the pack's `PathPolicy`/`CommandPolicy` — is deliberately not
    /// here yet; what this buys today is that the authority is *declared and visible* rather than
    /// implied by code, and that a configured deployment missing the grant fails at startup.
    local_grant: liberado_common::CapabilitySet,
    /// Chat-mode system prompt from `[acp] system_prompt`. `None` = built-in.
    system_prompt: Option<String>,
    /// ACP session id → mode + engine state.
    acp_sessions: Mutex<HashMap<String, AcpSession>>,
}

/// One `session/prompt` running while the stdin loop stays free for `session/cancel`.
struct InFlightPrompt {
    session_id: String,
    request_id: Value,
    handle: tokio::task::JoinHandle<Result<Value, String>>,
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
                    eprintln!("liberado-acp: unknown mode '{val}' (expected coding|chat|face)");
                    return Some(2);
                }
                // SAFETY: `#[tokio::main]` has already started the multi-threaded runtime by
                // the time this runs, so "single-threaded" is not the argument. It is sound
                // because nothing else in this process reads env vars before `run()` does, a few
                // lines later on this same task. Any future crate init that reads env from a
                // background thread would make this a data race.
                unsafe {
                    std::env::set_var("LIBERADO_ACP_MODE", val);
                }
                i += 2;
                continue;
            }
            other if other.starts_with("--mode=") => {
                let val = other.trim_start_matches("--mode=");
                if AgentMode::parse(val).is_none() {
                    eprintln!("liberado-acp: unknown mode '{val}' (expected coding|chat|face)");
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
    let bridge = resolve_bridge_startup().await?;
    // Single writer for JSON-RPC responses *and* session/update notifications.
    // Required once prompts run concurrent with stdin (cancel mid-turn).
    let wire = Arc::new(StdoutWire);

    // Take a private handle on the JSON-RPC wire and point the process-level stdin at the null
    // device, so no child can inherit the wire even if some future spawn site forgets to null
    // its stdin. See `stdin_guard` for why the order matters.
    //
    // Lines then arrive over a channel regardless of source, so the select! below does not care
    // which of the two readers is running.
    let (stdin_tx, mut stdin_rx) = mpsc::channel::<std::io::Result<Option<String>>>(64);
    spawn_stdin_reader(stdin_tx);
    let mut in_flight: Option<InFlightPrompt> = None;

    loop {
        tokio::select! {
            // Complete in-flight prompt responses without blocking cancel on stdin.
            join = async {
                match in_flight.as_mut() {
                    Some(inf) => {
                        let r = (&mut inf.handle).await;
                        Some((inf.session_id.clone(), inf.request_id.clone(), r))
                    }
                    None => std::future::pending().await,
                }
            } => {
                handle_prompt_join(&wire, join, &mut in_flight)?;
            }
            line = stdin_rx.recv() => {
                // A closed channel means the reader ended — same as EOF on stdin.
                if !handle_stdin_line(&bridge, &wire, line, &mut in_flight).await? {
                    break;
                }
            }
        }
    }

    if let Some(inf) = in_flight.take() {
        inf.handle.abort();
        let _ = inf.handle.await;
    }

    tracing::info!("stdin closed; acp bridge exiting");
    Ok(())
}

/// Say which config this run is using, before anything depends on it. The failure this replaces
/// was invisible precisely because nothing was ever printed about it.
fn report_config_dir(config_dir: &Option<std::path::PathBuf>) {
    match config_dir {
        Some(dir) => tracing::info!(
            config_dir = %dir.display(),
            topology = dir.join("topology.toml").exists(),
            policy = dir.join("policy.toml").exists(),
            tuning = dir.join("tuning.toml").exists(),
            "config directory resolved"
        ),
        None => tracing::warn!(
            "no config directory resolved; running on defaults with no declared project, no ship \
             bar and no policy grant"
        ),
    }
}

/// Point every child process at the shared build cache, once, before anything runs.
///
/// Set on this process rather than threaded through each command builder because it has to reach
/// three places that construct their own runners — the model's `run_command`, the verifier
/// pipeline, and the warm-up build — and a cache that two of the three use is a cache that warms
/// a directory the third ignores. Inheritance makes them agree by construction.
///
/// Correct here specifically because tuning is loaded once, above, and every session this bridge
/// serves shares it. A daemon serving sessions with *different* coder configs could not do this
/// and would have to thread the value.
///
/// SAFETY: single-threaded startup, before any session or task exists.
fn apply_shared_target_dir(shared_target_dir: &Option<String>) {
    if let Some(dir) = shared_target_dir
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        unsafe { std::env::set_var("CARGO_TARGET_DIR", dir) };
        tracing::info!(target_dir = %dir, "coding worktrees share one cargo build cache");
    }
}

/// Spawn `session/prompt` on a task so `session/cancel` can be read mid-turn. Refuses with a
/// JSON-RPC error when another prompt is already in flight or the session id is missing/empty.
/// Returns once the task is registered in `in_flight`.
fn spawn_prompt_if_free(
    bridge: &Arc<Bridge>,
    wire: &Arc<StdoutWire>,
    params: &Value,
    id: Value,
    in_flight: &mut Option<InFlightPrompt>,
) -> Result<(), Box<dyn std::error::Error>> {
    if in_flight.is_some() {
        wire.write_rpc_response(
            id,
            Err(JsonRpcErrorBody {
                code: -32603,
                message: "another session/prompt is already in flight".into(),
            }),
        )?;
        return Ok(());
    }
    let sid = match params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    {
        Some(s) if !s.is_empty() => s,
        _ => {
            wire.write_rpc_response(
                id,
                Err(JsonRpcErrorBody {
                    // -32602 Invalid params: the request is well-formed and the
                    // method exists, the arguments are wrong.
                    code: JSONRPC_INVALID_PARAMS,
                    message: "missing sessionId".into(),
                }),
            )?;
            return Ok(());
        }
    };
    let bridge_p = Arc::clone(bridge);
    let sink: Arc<dyn WireSink> = Arc::clone(wire) as Arc<dyn WireSink>;
    let params = params.clone();
    let handle = tokio::spawn(async move { run_session_prompt(bridge_p, sink, params).await });
    *in_flight = Some(InFlightPrompt {
        session_id: sid,
        request_id: id,
        handle,
    });
    Ok(())
}

/// Resolve every piece of startup state the bridge needs: provider, catalog, config (with the
/// resolution tier reported before anything depends on it), the shared build-cache override, and
/// the declared authority grant. Refuses to serve when the configured grant is missing.
async fn resolve_bridge_startup() -> Result<Arc<Bridge>, Box<dyn std::error::Error>> {
    let resolved = build_provider()?;
    let catalog = load_model_catalog(
        resolved.provider.as_ref(),
        &resolved.backend,
        &resolved.model_id,
    )
    .await;
    let default_mode = AgentMode::from_env_or_default();
    // `config_dir()`, not a bare read of `LIBERADO_CONFIG_DIR`.
    //
    // The env var is only the first of four tiers the rest of Liberado resolves through — platform
    // config dir, then a walk up from the running binary for a `config/`, then the platform dir as
    // a last resort. Reading the variable alone opted this surface out of all of them, so an
    // unset variable meant no topology, no policy, and no `[coder]` tuning, silently and with
    // every setting at a compiled-in default.
    let config_dir = liberado_config::config_dir();
    let coder_tuning = coding_run::load_coder_tuning(config_dir.as_deref())?;
    report_config_dir(&config_dir);
    apply_shared_target_dir(&coder_tuning.workspace_build.shared_target_dir);
    // `[acp]` from the same config the coding pack reads, so the prompt and the turn budget are
    // versioned prose in a file rather than JSON strings pasted into another tool's config.
    let acp_config = coding_run::load_acp_config(config_dir.as_deref());
    // Resolve the declared authority before serving anything. A configured deployment missing the
    // grant is refused by name here rather than discovered mid-session.
    let local_grant = coding_run::resolve_local_grant(config_dir.as_deref())?;
    let max_turns = coding_run::resolve_max_turns(acp_config.max_turns);
    let system_prompt = acp_config.system_prompt.clone();
    tracing::info!(
        backend = %resolved.backend,
        current = %resolved.model_id,
        catalog_len = catalog.len(),
        max_turns,
        mode = %default_mode.id(),
        "acp multi-mode agent ready"
    );
    Ok(Arc::new(Bridge {
        provider: resolved.provider,
        backend: resolved.backend,
        catalog: Mutex::new(catalog),
        current_model: Mutex::new(resolved.model_id),
        default_mode,
        max_turns,
        coder_tuning,
        config_dir,
        local_grant,
        system_prompt,
        acp_sessions: Mutex::new(HashMap::new()),
    }))
}

/// Take a private handle on the JSON-RPC wire and point the process-level stdin at the null
/// device, so no child can inherit the wire even if some future spawn site forgets to null its
/// stdin. Lines then arrive over a channel regardless of source, so the select loop does not care
/// which of the two readers is running.
fn spawn_stdin_reader(stdin_tx: mpsc::Sender<std::io::Result<Option<String>>>) {
    match stdin_guard::take_wire_stdin() {
        Some(wire_stdin) => {
            tracing::info!("stdin detached from children; reading the wire on a private handle");
            // A dedicated OS thread, because the handle is a plain `File` over a pipe and
            // Windows cannot register that with tokio's reactor for async reads.
            std::thread::spawn(move || {
                use std::io::BufRead;
                let mut reader = std::io::BufReader::new(wire_stdin);
                loop {
                    let mut buf = String::new();
                    let msg = match reader.read_line(&mut buf) {
                        Ok(0) => Ok(None),
                        Ok(_) => Ok(Some(buf.trim_end_matches(['\r', '\n']).to_string())),
                        Err(e) => Err(e),
                    };
                    let done = matches!(msg, Ok(None) | Err(_));
                    if stdin_tx.blocking_send(msg).is_err() || done {
                        break;
                    }
                }
            });
        }
        None => {
            // Non-Windows, or the swap failed. Children are still protected per-spawn by
            // `liberado_common::process::command`; this is the belt, not the braces.
            tracing::info!("stdin not detached; per-spawn nulling is the only guard");
            tokio::spawn(async move {
                let mut lines = BufReader::new(tokio::io::stdin()).lines();
                loop {
                    let msg = lines.next_line().await;
                    let done = matches!(msg, Ok(None) | Err(_));
                    if stdin_tx.send(msg).await.is_err() || done {
                        break;
                    }
                }
            });
        }
    }
}

/// Complete an in-flight prompt: clear the slot, map the join result onto a JSON-RPC response
/// (task abort — the hard cancel backup — becomes a cancelled stopReason), and write it.
/// The value the select loop's join arm receives: session id, request id, and the prompt task's
/// join result (the inner `Result` being the JSON-RPC outcome, the outer the task's own status).
type PromptJoin = (
    String,
    Value,
    Result<Result<Value, String>, tokio::task::JoinError>,
);

fn handle_prompt_join(
    wire: &StdoutWire,
    join: Option<PromptJoin>,
    in_flight: &mut Option<InFlightPrompt>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((_sid, id, join_result)) = join else {
        return Ok(());
    };
    *in_flight = None;
    let outcome = match join_result {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(message)) => Err(JsonRpcErrorBody {
            code: -32603,
            message,
        }),
        // Task abort (hard cancel backup) → cancelled turn.
        Err(je) if je.is_cancelled() => Ok(json!({ "stopReason": "cancelled" })),
        Err(je) => Err(JsonRpcErrorBody {
            code: -32603,
            message: format!("prompt task failed: {je}"),
        }),
    };
    wire.write_rpc_response(id, outcome)?;
    Ok(())
}

/// Process one line off the JSON-RPC wire. Returns `false` on EOF (closed channel or a `null`
/// line) so the caller can end the loop; everything else — including notifications, which expect
/// no response — returns `true`.
async fn handle_stdin_line(
    bridge: &Arc<Bridge>,
    wire: &Arc<StdoutWire>,
    line: Option<std::io::Result<Option<String>>>,
    in_flight: &mut Option<InFlightPrompt>,
) -> Result<bool, Box<dyn std::error::Error>> {
    // A closed channel means the reader ended — same as EOF on stdin.
    let Some(line) = line else {
        return Ok(false);
    };
    let Some(line) = line? else {
        return Ok(false);
    };
    let line = line.trim().to_string();
    if line.is_empty() {
        return Ok(true);
    }

    let msg: JsonRpcIncoming = match serde_json::from_str(&line) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(%line, %e, "unparseable ACP message");
            return Ok(true);
        }
    };

    let method = msg.method.unwrap_or_default();
    // Notifications have no id (or null id) and expect no response.
    let is_notification = msg.id.is_none() || msg.id.as_ref().is_some_and(|id| id.is_null());

    if is_notification {
        dispatch_notification(bridge, &method, msg.params, in_flight).await;
        return Ok(true);
    }

    let id = msg.id.unwrap_or(Value::Null);

    // session/prompt runs in a task so session/cancel can be read mid-turn.
    if method == "session/prompt" {
        spawn_prompt_if_free(bridge, wire, &msg.params, id, in_flight)?;
        return Ok(true);
    }

    match handle_request(Arc::clone(bridge), &method, msg.params, wire.as_ref()).await {
        Ok(result) => wire.write_rpc_response(id, Ok(result))?,
        Err(message) => wire.write_rpc_response(
            id,
            Err(JsonRpcErrorBody {
                code: if message.starts_with(METHOD_NOT_FOUND_PREFIX) {
                    JSONRPC_METHOD_NOT_FOUND
                } else {
                    JSONRPC_INTERNAL_ERROR
                },
                message,
            }),
        )?,
    }
    Ok(true)
}

/// Route a notification: `session/cancel` asks the live session to stop cooperatively and
/// hard-stops the in-flight task as a backup; anything else is logged and ignored.
async fn dispatch_notification(
    bridge: &Bridge,
    method: &str,
    params: Value,
    in_flight: &Option<InFlightPrompt>,
) {
    if method != "session/cancel" {
        handle_notification(method, params).await;
        return;
    }
    let sid = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if sid.is_empty() {
        return;
    }
    request_session_cancel(bridge, &sid).await;
    // Hard-stop backup if the turn is stuck outside cooperative points.
    if let Some(inf) = in_flight
        && inf.session_id == sid
    {
        inf.handle.abort();
    }
}

/// Sink for ACP notifications (`session/update`, …). Production writes NDJSON to stdout;
/// tests capture into a buffer so MockProvider turns can assert wire shape.
async fn wait_until_cancelled(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            // Sender dropped — treat as cancelled so the turn does not hang forever.
            return;
        }
    }
}

/// Signal cooperative cancel on the ACP session (and chat handle if present).
async fn request_session_cancel(bridge: &Bridge, sid: &str) {
    let sessions = bridge.acp_sessions.lock().await;
    if let Some(sess) = sessions.get(sid) {
        let _ = sess.cancel_tx.send(true);
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

async fn handle_notification(method: &str, _params: Value) {
    tracing::debug!(method = %method, "acp notification ignored");
}

async fn handle_request(
    bridge: Arc<Bridge>,
    method: &str,
    params: Value,
    sink: &dyn WireSink,
) -> Result<Value, String> {
    match method {
        "initialize" => handle_initialize(&params).await,

        "session/new" => handle_session_new(&bridge, &params).await,

        "session/load" => handle_session_load(&bridge, &params, sink).await,

        // session/prompt is handled by the main loop (spawned so cancel can interleave).
        "session/set_mode" => handle_set_mode(&bridge, &params).await,

        "session/set_model" => handle_set_model(&bridge, &params).await,

        "session/set_config_option" => Ok(json!({})),

        "authenticate" | "logout" => Ok(json!({})),

        // Prefixed so the stdin loop can map it to JSON-RPC -32601 without a second
        // error type. Everything used to answer -32603 (Internal error), which told a client
        // routing on the code that the agent had failed rather than that it does not implement
        // the method.
        _ => Err(format!("{METHOD_NOT_FOUND_PREFIX}{method}")),
    }
}

/// Pull the live model catalog from the provider and install it when non-empty (a failed or
/// empty fetch leaves the prior catalog in place).
async fn refresh_catalog_from_live(bridge: &Bridge) {
    let current = bridge.current_model.lock().await.clone();
    let fresh = load_model_catalog(bridge.provider.as_ref(), &bridge.backend, &current).await;
    if !fresh.is_empty() {
        *bridge.catalog.lock().await = fresh;
    }
}

/// Accept a model id the prior catalog did not list: append it as a pickable entry so a client
/// can select any id the provider actually serves, not just the ones known at boot.
async fn extend_catalog_with_model(bridge: &Bridge, model_id: &str) {
    let allowed = {
        let catalog = bridge.catalog.lock().await;
        catalog.iter().any(|m| m.model_id == model_id)
    };
    if !allowed {
        tracing::info!(%model_id, "set_model for id not in prior catalog; accepting");
        let mut catalog = bridge.catalog.lock().await;
        catalog.push(CatalogModel {
            name: display_name_for(model_id),
            description: description_for(&bridge.backend, model_id),
            model_id: model_id.to_string(),
        });
        catalog.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

async fn handle_initialize(params: &Value) -> Result<Value, String> {
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
        // Durable history and replay now back the loadSession capability.
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

/// `session/new`: create a live session, persist an initial record (soft), and answer with the
/// full session state payload.
async fn handle_session_new(bridge: &Bridge, params: &Value) -> Result<Value, String> {
    let cwd = params
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let sid = new_session_id();
    let mode = bridge.default_mode;
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let chat = if mode == AgentMode::Chat {
        open_chat_session(
            &sid,
            cwd.clone(),
            Arc::clone(&bridge.provider),
            bridge.max_turns,
            bridge.system_prompt.clone(),
            &[],
        )
        .ok()
        .map(Arc::new)
    } else {
        None
    };
    let model = bridge.current_model.lock().await.clone();
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
            cancel_tx,
            cancel_rx,
        },
    );

    // Persist an initial session record (soft — failure never fails the turn).
    if let Err(e) = session_store::save(&session_store::SessionRecord {
        id: sid.clone(),
        mode: mode.id().to_string(),
        cwd: cwd.clone(),
        model: model.clone(),
        messages: Vec::new(),
        updated_at: session_store::new_timestamp(),
    }) {
        tracing::warn!(session_id = %sid, error = %e, "session/new: failed to persist initial record");
    }

    tracing::info!(
        session_id = %sid,
        cwd = %cwd.display(),
        mode = %mode.id(),
        max_turns = bridge.max_turns,
        "session/new"
    );
    let (catalog, current) = bridge_model_snapshot(bridge).await;

    Ok(session_state_payload(
        &sid,
        &catalog,
        &current,
        mode,
        &bridge.local_grant,
    ))
}

/// `session/load`: restore a persisted session (replaying its transcript chunk-by-chunk), or
/// answer with current state when the id is already live.
async fn handle_session_load(
    bridge: &Bridge,
    params: &Value,
    sink: &dyn WireSink,
) -> Result<Value, String> {
    let sid = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or("missing sessionId")?;

    // When the id is already live, return current state without re-emitting history.
    {
        let sessions = bridge.acp_sessions.lock().await;
        if let Some(existing) = sessions.get(sid) {
            let (catalog, current) = bridge_model_snapshot(bridge).await;
            return Ok(session_state_payload(
                sid,
                &catalog,
                &current,
                existing.mode,
                &bridge.local_grant,
            ));
        }
    }

    let record = match session_store::load(sid) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err(format!(
                "no saved session found for id '{sid}' — start a new session with session/new"
            ));
        }
        Err(e) => return Err(format!("failed to load session '{sid}': {e}")),
    };

    let mode = AgentMode::parse(&record.mode).unwrap_or(bridge.default_mode);

    // Set the model from the stored record.
    bridge.provider.set_model(record.model.clone());
    *bridge.current_model.lock().await = record.model.clone();

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let chat = if mode == AgentMode::Chat {
        open_chat_session(
            sid,
            record.cwd.clone(),
            Arc::clone(&bridge.provider),
            bridge.max_turns,
            bridge.system_prompt.clone(),
            // The one call site where history is not empty. Everything else about a load
            // is cosmetic if this argument is.
            &record.messages,
        )
        .ok()
        .map(Arc::new)
    } else {
        None
    };

    let cwd = record.cwd.clone();
    bridge.acp_sessions.lock().await.insert(
        sid.to_string(),
        AcpSession {
            mode,
            cwd: cwd.clone(),
            coding: coding_run::CodingSessionState {
                cwd: cwd.clone(),
                coding_session_id: sid.to_string(),
                prior_feedback: Vec::new(),
                last_summary: None,
                rounds: 0,
            },
            chat,
            face_daemon_session: None,
            cancel_tx,
            cancel_rx,
        },
    );

    // Replay the stored transcript so the editor shows the conversation.
    for msg in &record.messages {
        match msg.role.as_str() {
            "user" => emit_user_message_chunk(sink, sid, &msg.content)?,
            "assistant" => emit_agent_text_chunk(sink, sid, &msg.content)?,
            _ => {}
        }
    }

    tracing::info!(
        session_id = %sid,
        mode = %mode.id(),
        messages = record.messages.len(),
        "session/load restored from disk"
    );

    let (catalog, current) = bridge_model_snapshot(bridge).await;
    Ok(session_state_payload(
        sid,
        &catalog,
        &current,
        mode,
        &bridge.local_grant,
    ))
}

/// `session/set_mode`: switch a live session's agent mode, lazy-initializing the chat handle when
/// entering chat, and persisting the change (soft).
async fn handle_set_mode(bridge: &Bridge, params: &Value) -> Result<Value, String> {
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
                bridge.system_prompt.clone(),
                &[],
            )
            .ok()
            .map(Arc::new);
        }
        // Persist the mode change (soft - failure never fails the turn).
        if let Err(e) = session_store::update_mode(sid, mode.id()) {
            tracing::warn!(session_id = %sid, error = %e, "session/set_mode: failed to update persisted mode");
        }
    }
    Ok(json!({}))
}

/// `session/set_model`: validate the model against the live catalog (extending it when the id is
/// new), set it on the provider, and persist per-session (soft).
async fn handle_set_model(bridge: &Bridge, params: &Value) -> Result<Value, String> {
    let model_id = params
        .get("modelId")
        .and_then(|v| v.as_str())
        .ok_or("missing modelId")?
        .trim()
        .to_string();
    if model_id.is_empty() {
        return Err("modelId must be non-empty".into());
    }

    refresh_catalog_from_live(bridge).await;
    extend_catalog_with_model(bridge, &model_id).await;

    // Catalog ids may be raw OpenRouter slugs; set on the live provider.
    bridge.provider.set_model(model_id.clone());
    *bridge.current_model.lock().await = model_id.clone();
    tracing::info!(%model_id, backend = %bridge.backend, "session/set_model");

    // Persist the model change (soft — failure never fails the turn).
    // Only when a valid session id is present — an empty id means this
    // was a global model switch not tied to a specific session.
    let sid = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    if let Some(sid) = sid
        && let Err(e) = session_store::update_model(sid, &model_id)
    {
        tracing::warn!(%model_id, error = %e, "session/set_model: failed to update persisted model");
    }

    Ok(json!({}))
}

async fn bridge_model_snapshot(bridge: &Bridge) -> (Vec<CatalogModel>, String) {
    let catalog = bridge.catalog.lock().await.clone();
    let current = bridge.current_model.lock().await.clone();
    (catalog, current)
}

/// Sink wrapper that delegates every emit to the real sink while capturing
/// every `agent_message_chunk` text for session record persistence.
struct CapturingSink {
    inner: Arc<dyn WireSink>,
    captured: std::sync::Mutex<String>,
}

impl WireSink for CapturingSink {
    fn emit(&self, method: &str, params: Value) -> Result<(), String> {
        if method == "session/update"
            && let Some(text) = params
                .get("update")
                .and_then(|u| u.get("content"))
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
        {
            match self.captured.lock() {
                Ok(mut buf) => buf.push_str(text),
                Err(e) => {
                    // A poisoned mutex in a single-threaded capture path is always a bug.
                    // Log it so persistence failures are never silently ignored.
                    tracing::warn!("CapturingSink: failed to lock captured buffer: {}", e);
                }
            }
        }
        self.inner.emit(method, params)
    }
}

/// Dispatch one `session/prompt` (runs on a spawned task; cancel stays live on stdin).
async fn run_session_prompt(
    bridge: Arc<Bridge>,
    sink: Arc<dyn WireSink>,
    params: Value,
) -> Result<Value, String> {
    let sid = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or("missing sessionId")?
        .to_string();
    let text = extract_prompt_text(&params)?;

    // Clear cancel from a prior turn; then route by mode.
    let mode = {
        let map = bridge.acp_sessions.lock().await;
        let sess = map
            .get(&sid)
            .ok_or_else(|| format!("unknown sessionId '{sid}'"))?;
        let _ = sess.cancel_tx.send(false);
        if let Some(chat) = &sess.chat {
            let _ = chat.cancel_tx.send(false);
        }
        sess.mode
    };

    // Wrap the real sink so we can capture assistant text for persistence.
    let capturing = CapturingSink {
        inner: sink,
        captured: std::sync::Mutex::new(String::new()),
    };

    let result = match mode {
        AgentMode::Coding => run_coding_prompt(Arc::clone(&bridge), &capturing, &sid, &text).await,
        AgentMode::Chat => run_chat_prompt(Arc::clone(&bridge), &capturing, &sid, &text).await,
        AgentMode::Face => run_face_prompt(Arc::clone(&bridge), &capturing, &sid, &text).await,
    };

    // Persist the user message and the agent reply (soft — failure never fails the turn).
    let assistant_text = capturing.captured.into_inner().unwrap_or_default();
    if let Err(e) = session_store::append_messages(&sid, &text, &assistant_text) {
        tracing::warn!(session_id = %sid, error = %e, "session/prompt: failed to persist messages");
    }

    result
}

/// Full coding pack path: LiberadoLoopBackend + durable worktree (same engine as goals).
/// Cancel the run: preserve the dirty tree, say so, and end the turn.
async fn cancel_and_preserve(sink: &dyn WireSink, sid: &str, workspace: &std::path::Path) -> Value {
    let note = match coding_run::preserve_worktree(workspace, "cancelled").await {
        Ok(Some(sha)) => format!("\n*(cancelled — work committed as `{sha}`)*\n"),
        Ok(None) => "\n*(cancelled)*\n".to_string(),
        Err(e) => format!("\n*(cancelled — could not preserve work: {e})*\n"),
    };
    let _ = emit_agent_text_chunk(sink, sid, &note);
    json!({ "stopReason": "cancelled" })
}

/// Preserve the worktree under the given label and emit the outcome (or error) to the surface.
async fn finish_coding_run(
    sink: &dyn WireSink,
    sid: &str,
    workspace: &std::path::Path,
    label: &str,
    outcome: Option<String>,
) -> Result<(), String> {
    let preserved = match coding_run::preserve_worktree(workspace, label).await {
        Ok(Some(sha)) => format!(
            "\n**Committed:** `{sha}` on `{}`\n",
            state_branch(workspace)
        ),
        Ok(None) => String::new(),
        Err(e) => format!("\n**Could not preserve work:** {e}\n"),
    };
    if let Some(report) = outcome {
        emit_agent_text_chunk(sink, sid, &report)?;
        emit_agent_text_chunk(sink, sid, &preserved)?;
    } else {
        emit_agent_text_chunk(sink, sid, &preserved)?;
    }
    Ok(())
}

async fn run_coding_prompt(
    bridge: Arc<Bridge>,
    sink: &dyn WireSink,
    sid: &str,
    text: &str,
) -> Result<Value, String> {
    let (mut state, mut cancel_rx) = {
        let map = bridge.acp_sessions.lock().await;
        let sess = map
            .get(sid)
            .ok_or_else(|| format!("unknown sessionId '{sid}' (call session/new first)"))?;
        (sess.coding.clone(), sess.cancel_rx.clone())
    };

    let model = bridge.current_model.lock().await.clone();
    let factory = coding_run::role_factory(Arc::clone(&bridge.provider));

    emit_agent_text_chunk(
        sink,
        sid,
        &format!(
            "Starting Liberado coding pack (max_turns={}, model={model})…\n\n",
            bridge.max_turns
        ),
    )?;

    // The run streams into this channel while it works; the loop below turns each event into an
    // ACP `session/update`. Bounded and lossy by design — `live::emit` uses `try_send`, so a
    // wedged UI drops frames instead of stalling the coding loop. 256 is far more than a turn
    // produces, so in practice nothing is dropped.
    let (ev_tx, mut ev_rx) = mpsc::channel::<liberado_session::SessionEvent>(256);

    // Resolve the worktree here rather than inside the run, so the path is still known if the
    // run is cancelled or panics — those are exactly the cases whose output would otherwise be
    // stranded, and a preservation step that cannot name the directory preserves nothing.
    let workspace = coding_run::prepare_workspace(&state.cwd, &state.coding_session_id).await?;

    // Owned by the task so the run can proceed while we render; handed back on completion,
    // because `prior_feedback` and the round counter must survive into the next prompt.
    let provider = Arc::clone(&bridge.provider);
    let tuning = bridge.coder_tuning.clone();
    let model_for_run = model.clone();
    let text_for_run = text.to_string();
    let max_turns = bridge.max_turns;
    let workspace_for_run = workspace.clone();
    // The client's directory, not the worktree: the ship bar is resolved from the declared project
    // that contains it, and the worktree lives under the data dir where no project reaches.
    let project_root_for_run = state.cwd.clone();
    let config_dir_for_run = bridge.config_dir.clone();
    let mut task = tokio::spawn(async move {
        let outcome = coding_run::run_coding_round(
            coding_run::CodingRound {
                provider,
                factory,
                tuning: &tuning,
                description: &text_for_run,
                model_override: Some(&model_for_run),
                max_turns,
                events: Some(ev_tx),
                workspace: workspace_for_run,
                project_root: project_root_for_run,
                config_dir: config_dir_for_run,
            },
            &mut state,
        )
        .await;
        (state, outcome)
    });

    // Paseo pairs tool_call -> tool_call_update by id; keep a LIFO of in-flight ids.
    let mut pending_tool_ids: Vec<(String, String)> = Vec::new();
    let mut events_open = true;
    let joined = loop {
        tokio::select! {
            biased;
            _ = wait_until_cancelled(&mut cancel_rx) => {
                task.abort();
                break None;
            }
            // The guard matters: once the sender is dropped `recv()` returns `None`
            // immediately and forever, which without it spins this loop at full tilt.
            ev = ev_rx.recv(), if events_open => match ev {
                Some(event) => render_coding_event(sink, sid, &event, &mut pending_tool_ids)?,
                None => events_open = false,
            },
            done = &mut task => break Some(done),
        }
    };

    let Some(joined) = joined else {
        return Ok(cancel_and_preserve(sink, sid, &workspace).await);
    };

    // Events buffered between the task finishing and the join landing would otherwise be lost —
    // typically the last tool result, which is the one a reader most wants.
    while let Ok(event) = ev_rx.try_recv() {
        render_coding_event(sink, sid, &event, &mut pending_tool_ids)?;
    }

    let (state, outcome) = joined.map_err(|e| format!("coding task panicked: {e}"))?;

    // Persist coding state only when the pack finished (not mid-cancel).
    if let Some(sess) = bridge.acp_sessions.lock().await.get_mut(sid) {
        sess.coding = state;
    }

    // Preserve before reporting, and on the failure path too: a failed run's diff is the
    // evidence for why it failed, and it is just as lost if nobody commits it.
    let label = if outcome.is_ok() { "done" } else { "failed" };
    let verdict = match outcome {
        Ok(result) => Some(result.render()),
        Err(e) => {
            emit_agent_text_chunk(sink, sid, &format!("\n**Coding pack error:** {e}\n"))?;
            None
        }
    };
    finish_coding_run(sink, sid, &workspace, label, verdict).await?;
    Ok(json!({ "stopReason": "end_turn" }))
}
/// Best-effort branch name for the report line. Cosmetic only — never fails the run.
fn state_branch(workspace: &std::path::Path) -> String {
    // `std_command`, not `std::process::Command::new` — this is the ACP bridge, whose stdin is
    // the JSON-RPC wire, and a child inheriting it is the bug this branch exists to fix. The
    // first draft of this helper used the raw constructor; `subprocess_rules.rs` is what makes
    // that a build failure rather than a rediscovery in six months.
    liberado_common::process::std_command("git")
        .args([
            "-C",
            &workspace.to_string_lossy(),
            "branch",
            "--show-current",
        ])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(detached)".into())
}

/// Turn one live pack event into the ACP updates Paseo renders.
///
/// The mapping is deliberately narrow. Tool activity becomes real `tool_call` /
/// `tool_call_update` entries so the editor can render them as tool cards; everything else
/// becomes text, because inventing richer ACP shapes for guard trips and validation results
/// would be guessing at a UI nobody has asked for yet. What matters is that a watcher can tell
/// the difference between working, stuck, and finished — which previously they could not, at all.
/// Emit the text-chunk ACP update for one text-shaped event (tokens, file changes, progress,
/// guards, validation, critic verdicts, roles).
fn emit_text_event(
    sink: &dyn WireSink,
    sid: &str,
    event: &liberado_session::SessionEvent,
) -> Result<(), String> {
    use liberado_session::SessionEventKind as K;
    match &event.kind {
        K::Token { text } => emit_agent_text_chunk(sink, sid, text)?,
        K::FileChanged { path, change } => {
            emit_agent_text_chunk(sink, sid, &format!("\n`{change}` {path}\n"))?;
        }
        K::Progress { message } => {
            emit_agent_text_chunk(sink, sid, &format!("\n_{message}_\n"))?;
        }
        // A guard trip is the single most useful thing to surface: it is the run telling you it
        // is going in circles, and it was previously invisible until the final summary.
        K::LoopGuard { guard, action } => {
            emit_agent_text_chunk(sink, sid, &format!("\n**guard** {guard} -> {action}\n"))?;
        }
        K::ValidationFinished { ok, summary } => {
            let mark = if *ok { "passed" } else { "FAILED" };
            emit_agent_text_chunk(sink, sid, &format!("\n**validation {mark}:** {summary}\n"))?;
        }
        K::CriticVerdict {
            reviewer,
            approved,
            issues,
            ..
        } => {
            let verdict = if *approved { "approved" } else { "rejected" };
            let detail = if issues.is_empty() {
                String::new()
            } else {
                format!(" ({})", issues.join("; "))
            };
            emit_agent_text_chunk(sink, sid, &format!("\n**{reviewer}** {verdict}{detail}\n"))?;
        }
        K::RoleStarted { role, model } => {
            emit_agent_text_chunk(sink, sid, &format!("\n_{role} ({model})_\n"))?;
        }
        _ => unreachable!("emit_text_event only receives text-shaped events"),
    }
    Ok(())
}

/// Emit the `tool_call` / `tool_call_update` ACP entries for tool start/finish, tracking the
/// open ids so the editor can render them as cards.
fn emit_tool_activity(
    sink: &dyn WireSink,
    sid: &str,
    event: &liberado_session::SessionEvent,
    pending_tool_ids: &mut Vec<(String, String)>,
) -> Result<(), String> {
    use liberado_session::SessionEventKind as K;
    match &event.kind {
        K::ToolStarted { name, args_preview } => {
            let id = push_tool_call_id(pending_tool_ids, name);
            emit_tool_call(sink, sid, &id, name, args_preview, "pending")?;
        }
        K::ToolFinished {
            name,
            ok,
            result_preview,
        } => {
            let id = pop_tool_call_id(pending_tool_ids, name);
            let status = if *ok { "completed" } else { "failed" };
            emit_tool_call_update(sink, sid, &id, name, status, result_preview)?;
        }
        _ => unreachable!("emit_tool_activity only receives tool events"),
    }
    Ok(())
}

/// Turn one live pack event into the ACP updates Paseo renders.
///
/// The mapping is deliberately narrow. Tool activity becomes real `tool_call` /
/// `tool_call_update` entries so the editor can render them as tool cards; everything else
/// becomes text, because inventing richer ACP shapes for guard trips and validation results
/// would be guessing at a UI nobody has asked for yet. What matters is that a watcher can tell
/// the difference between working, stuck, and finished — which previously they could not, at all.
fn render_coding_event(
    sink: &dyn WireSink,
    sid: &str,
    event: &liberado_session::SessionEvent,
    pending_tool_ids: &mut Vec<(String, String)>,
) -> Result<(), String> {
    use liberado_session::SessionEventKind as K;
    match &event.kind {
        K::ToolStarted { .. } | K::ToolFinished { .. } => {
            emit_tool_activity(sink, sid, event, pending_tool_ids)?;
        }
        K::Token { .. }
        | K::FileChanged { .. }
        | K::Progress { .. }
        | K::LoopGuard { .. }
        | K::ValidationFinished { .. }
        | K::CriticVerdict { .. }
        | K::RoleStarted { .. } => emit_text_event(sink, sid, event)?,
        // Deliberately silent. Checkpoints fire often and mean nothing to a reader; role-finished
        // is implied by whatever comes next; the terminal events are already covered by the
        // rendered result the caller emits when the run returns. Adding them would be noise
        // competing with the events above, which are the ones that carry information.
        K::Checkpoint { .. }
        | K::RoleFinished { .. }
        | K::SessionStarted { .. }
        | K::SessionFinished { .. }
        | K::Failed { .. }
        | K::AwaitingInput { .. }
        | K::HumanInput { .. } => {}
    }
    Ok(())
}

/// In-process chat: Conversation + Executor, no coding tools.
async fn run_chat_prompt(
    bridge: Arc<Bridge>,
    sink: &dyn WireSink,
    sid: &str,
    text: &str,
) -> Result<Value, String> {
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
                bridge.system_prompt.clone(),
                &[],
            )
            .ok()
            .map(Arc::new);
        }
        sess.chat
            .clone()
            .ok_or_else(|| "failed to open chat session".to_string())?
    };

    let stop = run_prompt_turn(session, text.to_string(), sink).await?;
    Ok(json!({ "stopReason": stop }))
}

/// Face agent via running `liberado serve` (HTTP SSE stream).
async fn run_face_prompt(
    bridge: Arc<Bridge>,
    sink: &dyn WireSink,
    sid: &str,
    text: &str,
) -> Result<Value, String> {
    let (mut daemon_session, mut cancel_rx) = {
        let map = bridge.acp_sessions.lock().await;
        let sess = map
            .get(sid)
            .ok_or_else(|| format!("unknown sessionId '{sid}'"))?;
        (sess.face_daemon_session.clone(), sess.cancel_rx.clone())
    };

    let sid_owned = sid.to_string();
    let emit = |method: &str, params: Value| -> Result<(), String> { sink.emit(method, params) };
    let result = tokio::select! {
        biased;
        _ = wait_until_cancelled(&mut cancel_rx) => None,
        result = face_client::run_face_turn(
            &mut daemon_session,
            text,
            &sid_owned,
            &emit,
        ) => Some(result),
    };

    if let Some(sess) = bridge.acp_sessions.lock().await.get_mut(sid) {
        sess.face_daemon_session = daemon_session;
    }

    match result {
        None => {
            let _ = emit_agent_text_chunk(sink, sid, "\n*(cancelled)*\n");
            Ok(json!({ "stopReason": "cancelled" }))
        }
        Some(Ok(())) => Ok(json!({ "stopReason": "end_turn" })),
        Some(Err(e)) => {
            emit_agent_text_chunk(sink, sid, &format!("\n**Face mode error:** {e}\n"))?;
            Ok(json!({ "stopReason": "end_turn" }))
        }
    }
}

/// Pure chat session: conversation + executor, no coding tools.
/// Open a chat session, optionally seeded with a stored transcript.
///
/// `history` is what makes a resume a resume. Replaying the transcript to the *client* only
/// repaints the editor; if the conversation the model sees starts empty, the user is looking at
/// their own history while the agent has none of it. That is the exact failure `loadSession:
/// false` was chosen to avoid — and it is worse once the flag says `true`, because the interface
/// now claims the memory is there.
fn open_chat_session(
    session_id: &str,
    cwd: PathBuf,
    provider: Arc<dyn Provider>,
    max_turns: u32,
    system_prompt: Option<String>,
    history: &[session_store::StoredMessage],
) -> Result<SessionHandle, String> {
    let system = system_prompt.unwrap_or_else(|| {
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
        conversation: Mutex::new(if history.is_empty() {
            Conversation::new(system)
        } else {
            // System prompt first, then the stored turns in order. A role the store does not
            // recognise is dropped rather than guessed at: inventing a speaker is how a resumed
            // conversation starts arguing with itself.
            let mut messages = vec![liberado_provider::Message::system(system)];
            for m in history {
                match m.role.as_str() {
                    "user" => messages.push(liberado_provider::Message::user(&m.content)),
                    "assistant" => messages.push(liberado_provider::Message::assistant(&m.content)),
                    _ => {}
                }
            }
            Conversation::from_history(messages)
        }),
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
    local_grant: &liberado_common::CapabilitySet,
) -> Value {
    json!({
        "sessionId": session_id,
        "models": model_state(catalog, current_model_id),
        "modes": mode::mode_state_json(mode),
        "configOptions": [],
        // Not part of the ACP schema — extra keys are ignored by clients that do not want them.
        // Carried anyway because "what is this agent allowed to do" should be answerable from the
        // session it is answered *about*, not by reading the binary's source.
        "liberadoAuthority": authority_summary(local_grant),
    })
}

/// Compact, human-readable summary of the declared grant for the session payload.
fn authority_summary(grant: &liberado_common::CapabilitySet) -> Value {
    if grant.capabilities.is_empty() {
        return json!({
            "component": "coding-local",
            "declared": false,
            "note": "standalone — no LIBERADO_CONFIG_DIR, so no policy to enforce against"
        });
    }
    json!({
        "component": "coding-local",
        "declared": true,
        "askHuman": grant.contains(&liberado_common::Capability::AskHuman),
        "capabilities": grant.capabilities.len(),
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

/// Reduce a `session/prompt` payload to the single text string sent to the model.
///
/// The wire carries ACP `ContentBlock`s. This bridge advertises `embeddedContext: true`, so a
/// client may embed `resource` blocks whose textual content must reach the model. Blocks render
/// in prompt order:
///
/// - `text` → the text verbatim.
/// - `resource` with `resource.text` → the text, preceded by a `[resource: …]` source line so
///   the model knows where it came from.
/// - `resource` with `resource.blob`, and `resource_link` → a concise `[resource: …]` source
///   marker. Payloads are never fetched or decoded, so a binary blob stays metadata-only.
/// - `image` / `audio` → dropped. The bridge advertises those capabilities false, so receiving
///   one is client error; decoding base64 media into text would be fake support.
fn extract_prompt_text(params: &Value) -> Result<String, String> {
    if let Some(arr) = params.get("prompt").and_then(|v| v.as_array()) {
        let mut parts = Vec::new();
        for block in arr {
            let ty = block.get("type").and_then(|t| t.as_str()).unwrap_or("text");
            match ty {
                "text" => {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        parts.push(t.to_string());
                    }
                }
                // Embedded resource:
                // `{ "type": "resource", "resource": { uri, text|blob, mimeType? } }`.
                "resource" => {
                    if let Some(res) = block.get("resource") {
                        let uri = res.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                        let mime = res.get("mimeType").and_then(|m| m.as_str()).unwrap_or("");
                        if let Some(t) = res
                            .get("text")
                            .and_then(|t| t.as_str())
                            .filter(|t| !t.is_empty())
                        {
                            parts.push(format!("{}\n{t}", resource_marker(uri, "", mime)));
                        } else {
                            // Blob (binary) payload or empty text: metadata-only marker.
                            parts.push(resource_marker(uri, "", mime));
                        }
                    }
                }
                // Resource link: `{ "type": "resource_link", uri, name, mimeType? }`.
                // No content was embedded; a concise source marker is all there is to render.
                "resource_link" => {
                    let uri = block.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let mime = block.get("mimeType").and_then(|m| m.as_str()).unwrap_or("");
                    parts.push(resource_marker(uri, name, mime));
                }
                // The bridge advertises these capabilities false. Never decode their base64 data
                // or accept a synthetic text field as a substitute.
                "image" | "audio" => {}
                // Unknown types keep the historical flattened top-level text/URI fallback.
                _ => {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        parts.push(t.to_string());
                    } else if let Some(uri) = block.get("uri").and_then(|u| u.as_str()) {
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let mime = block.get("mimeType").and_then(|m| m.as_str()).unwrap_or("");
                        parts.push(resource_marker(uri, name, mime));
                    }
                }
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

/// One-line source marker naming a resource the prompt references but does not carry usable text
/// for (resource links, binary blobs). Concise enough to read naturally in the model prompt;
/// never includes the payload, which for a blob is base64 and would be fake text.
fn resource_marker(uri: &str, name: &str, mime: &str) -> String {
    let mut marker = format!("[resource: {uri}");
    if !name.is_empty() {
        marker.push_str(&format!(" | {name}"));
    }
    if !mime.is_empty() {
        marker.push_str(&format!(" ({mime})"));
    }
    marker.push(']');
    marker
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
            // `wait_until_cancelled`, not a bare `changed()`: `changed()` only fires on a
            // transition *after* this receiver was cloned, so a cancel that lands between
            // session/prompt arriving and this loop starting was silently dropped and the turn ran
            // to completion. The helper checks the current value first, and treats a dropped sender
            // as cancelled rather than hanging.
            _ = wait_until_cancelled(&mut cancel_rx) => {
                turn.abort();
                stop_reason = "cancelled".into();
                break;
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
fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Stable, unique, path-safe. Not a UUID, and ACP does not require UUID format for sessionId.
    format!("lib-{:x}-{}", nanos, std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::catalog_model_ids;
    use tempfile::TempDir;

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

    /// Independent P4.3 oracle. The fixture uses the ACP wire shapes, not shapes inferred from an
    /// implementation: embedded text/blob content is nested under `resource`, while
    /// `resource_link`, `image`, and `audio` are top-level content blocks.
    #[test]
    fn p4_3_acceptance_uses_exact_acp_wire_shapes() {
        let cases: Value =
            serde_json::from_str(include_str!("../tests/fixtures/p4_3_prompt_blocks.json"))
                .expect("valid P4.3 fixture");
        for case in cases.as_array().expect("fixture case array") {
            let name = case["name"].as_str().expect("case name");
            let result = extract_prompt_text(&case["params"]);
            if let Some(expected) = case.get("expected").and_then(Value::as_str) {
                assert_eq!(result.expect(name), expected, "{name}");
            } else {
                let expected_error = case["error"].as_str().expect("case error");
                assert_eq!(result.expect_err(name), expected_error, "{name}");
            }
        }
    }

    /// The whole point of advertising `embeddedContext: true`: embedded textual resources reach
    /// the model with a source line above them, in prompt order, without dropping the rest.
    #[test]
    fn extract_prompt_preserves_mixed_block_order() {
        let params = json!({
            "sessionId": "s1",
            "prompt": [
                { "type": "text", "text": "first" },
                {
                    "type": "resource",
                    "resource": {
                        "uri": "file:///notes/plan.md",
                        "mimeType": "text/markdown",
                        "text": "embedded body"
                    }
                },
                { "type": "text", "text": "last" }
            ]
        });
        let out = extract_prompt_text(&params).unwrap();
        let first = out.find("first").unwrap();
        let marker = out.find("[resource: file:///notes/plan.md").unwrap();
        let body = out.find("embedded body").unwrap();
        let last = out.find("last").unwrap();
        assert!(
            first < marker && marker < body && body < last,
            "block order must survive extraction: {out:?}"
        );
    }

    #[test]
    fn extract_prompt_renders_embedded_text_resource() {
        let params = json!({
            "prompt": [
                {
                    "type": "resource",
                    "resource": {
                        "uri": "file:///notes/plan.md",
                        "mimeType": "text/markdown",
                        "text": "# Plan\n\nDo the thing."
                    }
                }
            ]
        });
        assert_eq!(
            extract_prompt_text(&params).unwrap(),
            "[resource: file:///notes/plan.md (text/markdown)]\n# Plan\n\nDo the thing."
        );
    }

    /// A resource link carries no embedded text, so it renders as a concise source marker.
    #[test]
    fn extract_prompt_marks_resource_link() {
        let params = json!({
            "prompt": [
                {
                    "type": "resource_link",
                    "uri": "file:///src/lib.rs",
                    "name": "lib.rs",
                    "mimeType": "text/x-rust"
                }
            ]
        });
        assert_eq!(
            extract_prompt_text(&params).unwrap(),
            "[resource: file:///src/lib.rs | lib.rs (text/x-rust)]"
        );
    }

    /// A resource link without optional MIME data still identifies its required name and URI.
    #[test]
    fn extract_prompt_marks_resource_link_without_mime() {
        let params = json!({
            "prompt": [
                { "type": "resource_link", "uri": "file:///data.csv", "name": "data.csv" }
            ]
        });
        assert_eq!(
            extract_prompt_text(&params).unwrap(),
            "[resource: file:///data.csv | data.csv]"
        );
    }

    /// A binary blob is embedded but must stay metadata-only: the base64 payload is never decoded
    /// into text, only the source marker is rendered.
    #[test]
    fn extract_prompt_keeps_binary_resource_metadata_only() {
        let params = json!({
            "prompt": [
                {
                    "type": "resource",
                    "resource": {
                        "uri": "file:///data.bin",
                        "mimeType": "application/octet-stream",
                        "blob": "SGVsbG8="
                    }
                }
            ]
        });
        let out = extract_prompt_text(&params).unwrap();
        assert_eq!(
            out,
            "[resource: file:///data.bin (application/octet-stream)]"
        );
        assert!(
            !out.contains("SGVsbG8="),
            "base64 payload must not reach the model"
        );
    }

    /// Image and audio blocks are rejected from the text stream entirely; a prompt made only of
    /// them has no usable text and must fail as before.
    #[test]
    fn extract_prompt_rejects_media_only_prompt() {
        let params = json!({
            "prompt": [
                { "type": "image", "data": "aW1hZ2U=", "mimeType": "image/png" },
                { "type": "audio", "data": "YXVkaW8=", "mimeType": "audio/mp3" }
            ]
        });
        assert_eq!(
            extract_prompt_text(&params).unwrap_err(),
            "prompt contained no text content"
        );
    }

    /// When media blocks sit next to real text, they are dropped, never decoded.
    #[test]
    fn extract_prompt_media_blocks_never_decode_to_text() {
        let params = json!({
            "prompt": [
                { "type": "text", "text": "keep me" },
                { "type": "image", "data": "aW1hZ2U=", "mimeType": "image/png" },
                { "type": "audio", "data": "YXVkaW8=", "mimeType": "audio/mp3" }
            ]
        });
        let out = extract_prompt_text(&params).unwrap();
        assert_eq!(out, "keep me");
        assert!(!out.contains("aW1hZ2U=") && !out.contains("YXVkaW8="));
    }

    /// Whitespace-only blocks must still fail; embedded-context support must not make an empty
    /// prompt look usable.
    #[test]
    fn extract_prompt_empty_text_still_fails() {
        let params = json!({
            "prompt": [
                { "type": "text", "text": "   " }
            ]
        });
        assert_eq!(
            extract_prompt_text(&params).unwrap_err(),
            "prompt contained no text content"
        );
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
            &liberado_common::CapabilitySet::empty(),
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

    /// A Bridge with a scripted provider — enough to drive `handle_request` in tests.
    fn test_bridge() -> Arc<Bridge> {
        use liberado_provider::MockProvider;
        Arc::new(Bridge {
            provider: Arc::new(MockProvider::with_script("mock", [])),
            backend: "mock".into(),
            catalog: Mutex::new(Vec::new()),
            current_model: Mutex::new("mock-model".into()),
            default_mode: AgentMode::Coding,
            max_turns: 8,
            coder_tuning: liberado_coder_core::CoderTuning::default(),
            config_dir: None,
            local_grant: liberado_common::CapabilitySet::empty(),
            system_prompt: None,
            acp_sessions: Mutex::new(HashMap::new()),
        })
    }

    /// A method the agent does not implement must answer -32601, not -32603.
    ///
    /// A cold review pointed out every error used the same "Internal error" code, so a client
    /// routing on it could not tell "you asked for something I do not implement" from "I broke".
    #[tokio::test]
    async fn an_unknown_method_is_method_not_found() {
        let bridge = test_bridge();
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        let err = handle_request(bridge, "session/does_not_exist", json!({}), &sink)
            .await
            .expect_err("an unimplemented method must be an error");
        assert!(
            err.starts_with(METHOD_NOT_FOUND_PREFIX),
            "must be taggable as -32601 by the wire layer, got: {err}"
        );
    }

    #[tokio::test]
    async fn initialize_shape_is_acp_compatible() {
        // Drives the real handler. The previous version built its own JSON literal and asserted on
        // that — it "mirrored the handle_request arm" by its own comment, so deleting the arm, or
        // dropping any field from the real response, left it green.
        let bridge = test_bridge();
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        let result = handle_request(bridge, "initialize", json!({}), &sink)
            .await
            .expect("initialize must succeed");

        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["agentInfo"]["name"], "Liberado");
        assert_eq!(result["agentCapabilities"]["loadSession"], true);
        assert_eq!(
            result["agentCapabilities"]["promptCapabilities"]["embeddedContext"],
            true
        );
    }

    #[test]
    fn load_session_capability_is_honest() {
        const {
            assert!(
                LOAD_SESSION_CAPABILITY,
                "loadSession must be true now that durable resume is implemented; \
                 false would make Paseo think resume is unsupported"
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

    #[tokio::test]
    async fn wait_until_cancelled_resolves_when_flag_set() {
        let (tx, mut rx) = watch::channel(false);
        let waiter = tokio::spawn(async move {
            wait_until_cancelled(&mut rx).await;
        });
        // Give the waiter a chance to park on changed().
        tokio::task::yield_now().await;
        tx.send(true).expect("send cancel");
        tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
            .await
            .expect("cancel wait timed out")
            .expect("join");
    }

    #[tokio::test]
    async fn wait_until_cancelled_sees_already_true() {
        let (_tx, mut rx) = watch::channel(true);
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            wait_until_cancelled(&mut rx),
        )
        .await
        .expect("must return immediately when already cancelled");
    }

    #[tokio::test]
    async fn chat_turn_stops_with_cancelled_on_cancel_flag() {
        use liberado_provider::{CompletionResponse, MockProvider};
        use std::time::Duration;

        // Slow first completion so cancel can win mid-turn.
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text("should not finish")],
        ));
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let session = Arc::new(SessionHandle {
            id: "sess-cancel".into(),
            conversation: Mutex::new(Conversation::new("test system")),
            executor: Executor::new(provider, Budget::new(8)),
            tools: Arc::new(NoTools),
            cancel_tx: cancel_tx.clone(),
            cancel_rx,
        });
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };

        // Cancel BEFORE the turn starts. The previous version slept 5ms and then cancelled, so
        // the fast mock almost always finished first and the assertion accepted `end_turn` —
        // which meant the test passed with the cancel path deleted entirely.
        cancel_tx.send(true).expect("cancel send");

        let turn = tokio::spawn({
            let session = Arc::clone(&session);
            async move { run_prompt_turn(session, "hello".into(), &sink).await }
        });

        let stop = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .expect("turn join timeout")
            .expect("join")
            .expect("turn result");
        assert_eq!(
            stop, "cancelled",
            "a turn whose session was already cancelled must report `cancelled`"
        );
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

    // ── session/load tests ───────────────────────────────────────────────

    /// Serializes tests in this module that redirect `sessions_dir()` so they do not race with
    /// `session_store` tests or each other.
    static SESSION_LOAD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_sessions_dir(
        dir: &TempDir,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
        session_store::TestDirGuard,
    ) {
        let lock = SESSION_LOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (dir_lock, guard) = session_store::set_sessions_dir(dir);
        (lock, dir_lock, guard)
    }

    #[tokio::test]
    async fn load_saved_session_restores_mode_and_model() {
        let dir = TempDir::new().unwrap();
        let _guards = lock_sessions_dir(&dir);

        let record = session_store::SessionRecord {
            id: "lib-load-test".into(),
            mode: "chat".into(),
            cwd: PathBuf::from("/tmp/test-project"),
            model: "gpt-4o".into(),
            messages: vec![],
            updated_at: session_store::new_timestamp(),
        };
        session_store::save(&record).expect("save");

        let bridge = test_bridge();
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        let result = handle_request(
            bridge,
            "session/load",
            json!({"sessionId": "lib-load-test"}),
            &sink,
        )
        .await
        .expect("session/load must succeed");

        assert_eq!(result["sessionId"], "lib-load-test");
        assert_eq!(result["modes"]["currentModeId"], "chat");
        assert_eq!(result["models"]["currentModelId"], "gpt-4o");
    }

    #[tokio::test]
    async fn load_saved_session_registers_in_memory_with_correct_cwd() {
        let dir = TempDir::new().unwrap();
        let _guards = lock_sessions_dir(&dir);

        let cwd = PathBuf::from("/tmp/load-cwd-test");
        let record = session_store::SessionRecord {
            id: "lib-cwd".into(),
            mode: "coding".into(),
            cwd: cwd.clone(),
            model: "mock-model".into(),
            messages: vec![],
            updated_at: session_store::new_timestamp(),
        };
        session_store::save(&record).expect("save");

        let bridge = test_bridge();
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        let result = handle_request(
            Arc::clone(&bridge),
            "session/load",
            json!({"sessionId": "lib-cwd"}),
            &sink,
        )
        .await
        .expect("session/load must succeed");

        assert_eq!(result["sessionId"], "lib-cwd");

        let sessions = bridge.acp_sessions.lock().await;
        let sess = sessions.get("lib-cwd").expect("session must be registered");
        assert_eq!(sess.cwd, cwd, "cwd must match loaded record");
    }

    #[tokio::test]
    async fn load_unsaved_id_is_clear_error_not_empty_session() {
        let dir = TempDir::new().unwrap();
        let _guards = lock_sessions_dir(&dir);

        let bridge = test_bridge();
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        let err = handle_request(
            bridge,
            "session/load",
            json!({"sessionId": "no-such-id"}),
            &sink,
        )
        .await
        .expect_err("loading an unsaved id must be an error");

        assert!(
            err.contains("no saved session found"),
            "error must say no session was found, got: {err}"
        );
    }

    #[tokio::test]
    async fn load_replays_stored_messages_in_stored_order() {
        let dir = TempDir::new().unwrap();
        let _guards = lock_sessions_dir(&dir);

        let record = session_store::SessionRecord {
            id: "lib-replay".into(),
            mode: "coding".into(),
            cwd: PathBuf::from("/tmp/replay"),
            model: "mock-model".into(),
            messages: vec![
                session_store::StoredMessage {
                    role: "user".into(),
                    content: "hello".into(),
                },
                session_store::StoredMessage {
                    role: "assistant".into(),
                    content: "hi there".into(),
                },
                session_store::StoredMessage {
                    role: "user".into(),
                    content: "second".into(),
                },
                session_store::StoredMessage {
                    role: "assistant".into(),
                    content: "answer".into(),
                },
            ],
            updated_at: session_store::new_timestamp(),
        };
        session_store::save(&record).expect("save");

        let bridge = test_bridge();
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        let result = handle_request(
            bridge,
            "session/load",
            json!({"sessionId": "lib-replay"}),
            &sink,
        )
        .await
        .expect("session/load must succeed");

        assert_eq!(result["sessionId"], "lib-replay");

        let lines = sink.lines.lock().unwrap();
        let updates: Vec<_> = lines
            .iter()
            .filter(|(m, _)| m == "session/update")
            .collect();
        assert_eq!(
            updates.len(),
            4,
            "must emit exactly 4 message chunks, got {:?}",
            updates
        );

        assert_eq!(
            updates[0].1["update"]["sessionUpdate"], "user_message_chunk",
            "first message must be user"
        );
        assert_eq!(updates[0].1["update"]["content"]["text"], "hello");
        assert_eq!(
            updates[1].1["update"]["sessionUpdate"], "agent_message_chunk",
            "second message must be assistant"
        );
        assert_eq!(updates[1].1["update"]["content"]["text"], "hi there");
        assert_eq!(
            updates[2].1["update"]["sessionUpdate"], "user_message_chunk",
            "third message must be user"
        );
        assert_eq!(updates[2].1["update"]["content"]["text"], "second");
        assert_eq!(
            updates[3].1["update"]["sessionUpdate"], "agent_message_chunk",
            "fourth message must be assistant"
        );
        assert_eq!(updates[3].1["update"]["content"]["text"], "answer");
    }

    /// A resume the *model* can see, not just the editor.
    ///
    /// Replaying the transcript to the client repaints the UI. If the conversation behind it starts
    /// empty, the user reads their own history while the agent has none of it — the precise failure
    /// `loadSession: false` existed to prevent, and worse once the flag says `true`, because the
    /// interface now asserts the memory is there.
    ///
    /// This is the requirement the original implementation skipped, and it skipped the test with
    /// it: five tests covered the replay and none covered the restore, so everything looked green.
    #[tokio::test]
    async fn load_restores_history_into_the_conversation_not_only_the_client() {
        let dir = TempDir::new().unwrap();
        let _guards = lock_sessions_dir(&dir);

        let record = session_store::SessionRecord {
            id: "lib-memory".into(),
            mode: "chat".into(),
            cwd: PathBuf::from("/tmp/memory"),
            model: "mock-model".into(),
            messages: vec![
                session_store::StoredMessage {
                    role: "user".into(),
                    content: "my name is Ada".into(),
                },
                session_store::StoredMessage {
                    role: "assistant".into(),
                    content: "noted, Ada".into(),
                },
            ],
            updated_at: session_store::new_timestamp(),
        };
        session_store::save(&record).expect("save");

        let bridge = test_bridge();
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        handle_request(
            bridge.clone(),
            "session/load",
            json!({"sessionId": "lib-memory"}),
            &sink,
        )
        .await
        .expect("session/load must succeed");

        let sessions = bridge.acp_sessions.lock().await;
        let chat = sessions
            .get("lib-memory")
            .and_then(|s| s.chat.clone())
            .expect("chat mode must have a live chat session after load");
        let convo = chat.conversation.lock().await;
        // `transient` is 0 on a freshly built conversation, so this is every message it holds.
        let messages = convo.turn_tail(0);
        let text: String = messages
            .iter()
            .map(|m| format!("{:?}:{}\n", m.role, m.content))
            .collect();

        assert!(
            text.contains("my name is Ada"),
            "the user's prior turn must be in the model's conversation: {text}"
        );
        assert!(
            text.contains("noted, Ada"),
            "the assistant's prior turn must be in the model's conversation: {text}"
        );
        assert!(
            messages
                .iter()
                .any(|m| matches!(m.role, liberado_provider::Role::System)),
            "the system prompt must survive the restore: {text}"
        );
    }

    #[tokio::test]
    async fn load_reloads_already_loaded_session_without_duplicate_emit() {
        let dir = TempDir::new().unwrap();
        let _guards = lock_sessions_dir(&dir);

        let record = session_store::SessionRecord {
            id: "lib-reload".into(),
            mode: "coding".into(),
            cwd: PathBuf::from("/tmp/reload"),
            model: "mock-model".into(),
            messages: vec![session_store::StoredMessage {
                role: "user".into(),
                content: "ping".into(),
            }],
            updated_at: session_store::new_timestamp(),
        };
        session_store::save(&record).expect("save");

        let bridge = test_bridge();
        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        // First load emits messages.
        handle_request(
            Arc::clone(&bridge),
            "session/load",
            json!({"sessionId": "lib-reload"}),
            &sink,
        )
        .await
        .expect("first load");

        assert_eq!(
            sink.lines.lock().unwrap().len(),
            1,
            "first load emits 1 message"
        );

        let sink2 = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        // Second load must succeed without re-emitting history for the already-loaded session.
        let result = handle_request(
            bridge,
            "session/load",
            json!({"sessionId": "lib-reload"}),
            &sink2,
        )
        .await
        .expect("second load");

        assert_eq!(result["sessionId"], "lib-reload");
        assert!(
            sink2.lines.lock().unwrap().is_empty(),
            "re-loading an already-loaded session must not re-emit messages"
        );
    }

    /// render_coding_event maps tool activity to tool_call entries and everything else to text.
    #[test]
    fn render_coding_event_emits_text_and_tool_entries() {
        use liberado_session::{SessionEvent, SessionEventKind};

        let sink = CaptureSink {
            lines: std::sync::Mutex::new(Vec::new()),
        };
        let mk = |kind| SessionEvent::new("s", kind);

        render_coding_event(
            &sink,
            "s",
            &mk(SessionEventKind::Token { text: "hi".into() }),
            &mut Vec::new(),
        )
        .unwrap();
        let mut pending = Vec::new();
        render_coding_event(
            &sink,
            "s",
            &mk(SessionEventKind::ToolStarted {
                name: "read".into(),
                args_preview: "\"a.json\"".into(),
            }),
            &mut pending,
        )
        .unwrap();
        assert!(!pending.is_empty(), "an open tool call must be tracked");
        render_coding_event(
            &sink,
            "s",
            &mk(SessionEventKind::ToolFinished {
                name: "read".into(),
                ok: true,
                result_preview: "ok".into(),
            }),
            &mut pending,
        )
        .unwrap();
        assert!(pending.is_empty(), "a finished tool call must be popped");
        render_coding_event(
            &sink,
            "s",
            &mk(SessionEventKind::ValidationFinished {
                ok: false,
                summary: "tests broke".into(),
            }),
            &mut Vec::new(),
        )
        .unwrap();

        let lines = sink.lines.lock().unwrap();
        let updates: Vec<String> = lines
            .iter()
            .map(|(_, p)| {
                p["update"]["sessionUpdate"]
                    .as_str()
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        assert!(
            updates.contains(&"agent_message_chunk".into()),
            "tokens and validation must render as text chunks: {updates:?}"
        );
        assert!(
            lines.iter().any(|(_, p)| {
                p["update"]["content"]["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("hi"))
            }),
            "the token text must be emitted"
        );
        assert!(
            updates.contains(&"tool_call".into()) && updates.contains(&"tool_call_update".into()),
            "tool start/finish must render as tool entries: {updates:?}"
        );
    }
}
