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
//! | `session/set_mode` | Switch coding / goal / chat / face (Liberado-owned; one Paseo provider) |
//! | `session/set_model` | Hot-swap the active model (must be in the live catalog) |
//!
//! Modes (same process; switch via ACP or `--mode` / `LIBERADO_ACP_MODE`):
//! - **coding** — interactive conversation + coding tools on a durable worktree (default)
//! - **goal** — one-shot coding pack (`LiberadoLoopBackend`) to a terminal
//! - **chat** — in-process conversation (no tools, no daemon)
//! - **face** — daemon face agent (`liberado serve`; vault + delegate)
//!
//! Usage (spawned by Paseo — one provider is enough):
//! ```text
//! liberado-acp
//! liberado-acp --mode chat
//! liberado-acp --mode goal
//! liberado-acp --mode face
//! ```
//!
//! Environment:
//! - `OPENROUTER_API_KEY` / `DEEPSEEK_API_KEY` / `OPENAI_API_KEY`
//! - `LIBERADO_ACP_MODE` — default mode (`coding` \| `goal` \| `chat` \| `face`)
//! - `LIBERADO_ACP_MODEL` — initial model id
//! - `LIBERADO_ACP_MAX_TURNS` — per-launch override of `[acp] max_turns`
//! - `LIBERADO_CONFIG_DIR` — optional Liberado config (topology + `[coder]` tuning)
//! - `LIBERADO_SERVER` — face-mode daemon base URL (default `http://127.0.0.1:4201`)
//!
//! Model catalog: live `GET /models` from the configured backend, A–Z by id.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

mod ask_human;
mod coding_run;
mod done;
mod interactive;
mod permission;
mod prompt_support;
mod provider;
mod session_store;
mod stdin_guard;
mod wire;
mod workspace_targets;

use wire::{
    JsonRpcErrorBody, JsonRpcIncoming, StdoutWire, WireSink, emit_agent_text_chunk, emit_tool_call,
    emit_tool_call_update, emit_user_message_chunk, pop_tool_call_id, push_tool_call_id,
};

use prompt_support::{
    coding_verdict, drain_coding_events, emit_finish_report, persist_coding_state,
    preserved_worktree_report, render_face_prompt_result,
};
use provider::{
    CatalogModel, build_provider, description_for, display_name_for, load_model_catalog,
};
mod face_client;
mod mode;

use liberado_executor::{AgentEvent, Budget, ExecError, Executor, ToolRuntime};
use liberado_main_agent::{Conversation, DEFAULT_SYSTEM_PROMPT};
use liberado_provider::{Provider, Role};
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
    /// One-shot pack state (mode=goal). Unused in interactive coding.
    coding: coding_run::CodingSessionState,
    /// In-process conversation (mode=coding with tools, or mode=chat without).
    converse: Option<Arc<SessionHandle>>,
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
    /// Outbound `session/request_permission` waiters (denied commands).
    permissions: Arc<permission::PermissionBroker>,
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
    /// True when [`liberado_coder_tools::CodingToolRuntime`] is attached (interactive coding).
    coding_tools: bool,
    /// `ask_human` call id waiting for the next `session/prompt`, if any.
    pending_ask: std::sync::Mutex<Option<String>>,
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
                println!("{}", help_text());
                return Some(0);
            }
            "--mode" | "-m" => {
                let val = args.get(i + 1).map(|s| s.as_str()).unwrap_or("");
                if AgentMode::parse(val).is_none() {
                    eprintln!(
                        "liberado-acp: unknown mode '{val}' (expected {})",
                        AgentMode::EXPECTED
                    );
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
                    eprintln!(
                        "liberado-acp: unknown mode '{val}' (expected {})",
                        AgentMode::EXPECTED
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

/// The `--help` text, built rather than printed so tests can pin what a client sees.
fn help_text() -> String {
    format!(
        "liberado-acp {} — Liberado multi-mode ACP agent for Paseo\n\n\
         Usage:\n\
           liberado-acp [--mode coding|goal|chat|face]   ACP on stdin/stdout\n\
           liberado-acp --version\n\
           liberado-acp --help\n\n\
         Modes (Liberado-owned; also switchable via ACP session/set_mode):\n\
           coding  Interactive conversation + coding tools (default)\n\
           goal    One-shot /goal coding pack (run to a terminal)\n\
           chat    In-process conversation, no file tools\n\
           face    Daemon face agent — needs liberado serve (LIBERADO_SERVER)\n\n\
         Environment:\n\
           OPENROUTER_API_KEY / DEEPSEEK_API_KEY / OPENAI_API_KEY\n\
           LIBERADO_ACP_MODE           default mode (coding|goal|chat|face)\n\
           LIBERADO_ACP_MODEL          initial model id\n\
           LIBERADO_ACP_MAX_TURNS      coder turns per prompt (default 50)\n\
           LIBERADO_CONFIG_DIR         Liberado config ([coder] tuning)\n\
           LIBERADO_SERVER             face mode daemon URL (default http://127.0.0.1:4201)",
        env!("CARGO_PKG_VERSION")
    )
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let bridge = resolve_bridge_startup().await?;
    // Single writer for JSON-RPC responses *and* session/update notifications.
    // Required once prompts run concurrent with stdin (cancel mid-turn).
    let wire = Arc::new(StdoutWire);
    bridge.permissions.bind_wire(Arc::clone(&wire));

    // Take a private handle on the JSON-RPC wire and point the process-level stdin at the null
    // device, so no child can inherit the wire even if some future spawn site forgets to null
    // its stdin. See `stdin_guard` for why the order matters.
    //
    // Lines then arrive over a channel regardless of source, so the select! below does not care
    // which of the two readers is running.
    let (stdin_tx, mut stdin_rx) = mpsc::channel::<std::io::Result<Option<String>>>(64);
    spawn_stdin_reader(stdin_tx);
    // One sink object serves responses and notifications for the whole loop.
    let wire_dyn: Arc<dyn WireSink> = wire.clone();
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
                handle_prompt_join(wire_dyn.as_ref(), join, &mut in_flight)?;
            }
            line = stdin_rx.recv() => {
                // A closed channel means the reader ended — same as EOF on stdin.
                if !handle_stdin_line(&bridge, &wire_dyn, line, &mut in_flight).await? {
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

/// Spawn `session/prompt` on a task so `session/cancel` can be read mid-turn. Refuses with a
/// JSON-RPC error when another prompt is already in flight or the session id is missing/empty.
/// Returns once the task is registered in `in_flight`.
fn spawn_prompt_if_free(
    bridge: &Arc<Bridge>,
    wire: &Arc<dyn WireSink>,
    params: &Value,
    id: Value,
    in_flight: &mut Option<InFlightPrompt>,
) -> Result<(), Box<dyn std::error::Error>> {
    match prompt_slot_check(in_flight.is_some(), params) {
        PromptSlot::Busy => wire.write_rpc_response(
            id,
            Err(JsonRpcErrorBody {
                code: JSONRPC_INTERNAL_ERROR,
                message: "another session/prompt is already in flight".into(),
            }),
        )?,
        PromptSlot::MissingSessionId => {
            wire.write_rpc_response(id, Err(missing_session_error()))?;
        }
        PromptSlot::Ready(sid) => {
            let bridge_p = Arc::clone(bridge);
            let sink: Arc<dyn WireSink> = Arc::clone(wire);
            let params = params.clone();
            let handle =
                tokio::spawn(async move { run_session_prompt(bridge_p, sink, params).await });
            *in_flight = Some(InFlightPrompt {
                session_id: sid,
                request_id: id,
                handle,
            });
        }
    }
    Ok(())
}

/// Whether another `session/prompt` may start, and the session id it would run for.
///
/// One prompt at a time keeps `session/cancel` meaningful; a missing or empty session
/// id is invalid params, not an internal error.
enum PromptSlot {
    Busy,
    MissingSessionId,
    Ready(String),
}

fn prompt_slot_check(in_flight: bool, params: &Value) -> PromptSlot {
    if in_flight {
        return PromptSlot::Busy;
    }
    match params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    {
        Some(s) if !s.is_empty() => PromptSlot::Ready(s),
        _ => PromptSlot::MissingSessionId,
    }
}

/// -32602 Invalid params: the request is well-formed and the method exists, the arguments
/// are wrong.
fn missing_session_error() -> JsonRpcErrorBody {
    JsonRpcErrorBody {
        code: JSONRPC_INVALID_PARAMS,
        message: "missing sessionId".into(),
    }
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
    let source_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    workspace_targets::apply_workspace_targets(&coder_tuning.workspace_build, &source_root);
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
        permissions: Arc::new(permission::PermissionBroker::new()),
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
                    let done = stdin_read_done(&msg);
                    if reader_should_stop(stdin_tx.blocking_send(msg).is_err(), done) {
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
                    let done = stdin_read_done(&msg);
                    if reader_should_stop(stdin_tx.send(msg).await.is_err(), done) {
                        break;
                    }
                }
            });
        }
    }
}

/// Either condition ends the loop: a dead receiver means nobody is listening, and EOF or a
/// read error means the wire is gone. Requiring *both* would spin forever after either one.
fn reader_should_stop(send_failed: bool, done: bool) -> bool {
    send_failed || done
}

/// A read is *done* when it reports EOF or an error — either ends the reader loop. A
/// successful line keeps it running; mistaking one for the other either drops the wire
/// mid-session or spins forever after EOF.
fn stdin_read_done(msg: &std::io::Result<Option<String>>) -> bool {
    matches!(msg, Ok(None) | Err(_))
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

/// Map one finished prompt task onto its JSON-RPC outcome.
///
/// A cancelled task is a *cancelled turn*, not an error: `session/cancel` aborts as a hard
/// backup after the cooperative path, and the client must see `stopReason: "cancelled"`.
fn prompt_join_outcome(
    join_result: Result<Result<Value, String>, tokio::task::JoinError>,
) -> Result<Value, JsonRpcErrorBody> {
    match join_result {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(message)) => Err(JsonRpcErrorBody {
            code: JSONRPC_INTERNAL_ERROR,
            message,
        }),
        // Task abort (hard cancel backup) → cancelled turn.
        Err(je) if je.is_cancelled() => Ok(json!({ "stopReason": "cancelled" })),
        Err(je) => Err(JsonRpcErrorBody {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!("prompt task failed: {je}"),
        }),
    }
}

fn handle_prompt_join(
    wire: &dyn WireSink,
    join: Option<PromptJoin>,
    in_flight: &mut Option<InFlightPrompt>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((_sid, id, join_result)) = join else {
        return Ok(());
    };
    *in_flight = None;
    wire.write_rpc_response(id, prompt_join_outcome(join_result))?;
    Ok(())
}

/// Process one line off the JSON-RPC wire. Returns `false` on EOF (closed channel or a `null`
/// line) so the caller can end the loop; everything else — including notifications, which expect
/// no response — returns `true`.
async fn handle_stdin_line(
    bridge: &Arc<Bridge>,
    wire: &Arc<dyn WireSink>,
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

    dispatch_stdin_message(bridge, wire, msg, in_flight).await?;
    Ok(true)
}

async fn dispatch_stdin_message(
    bridge: &Arc<Bridge>,
    wire: &Arc<dyn WireSink>,
    msg: JsonRpcIncoming,
    in_flight: &mut Option<InFlightPrompt>,
) -> Result<(), Box<dyn std::error::Error>> {
    if apply_client_rpc_reply(bridge, &msg) {
        return Ok(());
    }

    let method = msg.method.unwrap_or_default();
    // Notifications have no id (or null id) and expect no response.
    let is_notification = msg.id.is_none() || msg.id.as_ref().is_some_and(|id| id.is_null());

    if is_notification {
        dispatch_notification(bridge, &method, msg.params, in_flight).await;
        return Ok(());
    }

    let id = msg.id.unwrap_or(Value::Null);

    // session/prompt runs in a task so session/cancel can be read mid-turn.
    if method == "session/prompt" {
        spawn_prompt_if_free(bridge, wire, &msg.params, id, in_flight)?;
        return Ok(());
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
    Ok(())
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
    bridge.permissions.cancel_all();
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
        if let Some(converse) = &sess.converse {
            let _ = converse.cancel_tx.send(true);
        }
        tracing::info!(
            session_id = %sid,
            mode = %sess.mode.id(),
            "session/cancel requested"
        );
    }
}

fn apply_client_rpc_reply(bridge: &Bridge, msg: &JsonRpcIncoming) -> bool {
    // Client answers to agent-initiated `session/request_permission` (no method, has id).
    let has_id = msg.id.as_ref().is_some_and(|id| !id.is_null());
    let is_reply = msg.method.as_ref().is_none_or(|m| m.is_empty())
        && has_id
        && (msg.result.is_some() || msg.error.is_some());
    if !is_reply {
        return false;
    }
    bridge.permissions.complete(
        msg.id.as_ref().unwrap_or(&Value::Null),
        msg.result.clone(),
        msg.error.clone(),
    );
    true
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
            converse: None,
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
            converse: None,
            face_daemon_session: None,
            cancel_tx,
            cancel_rx,
        },
    );
    restore_loaded_converse(bridge, sid, mode).await;
    replay_loaded_messages(sink, sid, &record.messages)?;

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

async fn restore_loaded_converse(bridge: &Bridge, sid: &str, mode: AgentMode) {
    if !mode.is_converse() {
        return;
    }
    if let Err(e) = ensure_converse(bridge, sid).await {
        tracing::warn!(session_id = %sid, error = %e, "session/load: failed to restore converse");
    }
}

fn replay_loaded_messages(
    sink: &dyn WireSink,
    sid: &str,
    messages: &[session_store::StoredMessage],
) -> Result<(), String> {
    for msg in messages {
        match msg.role.as_str() {
            "user" => emit_user_message_chunk(sink, sid, &msg.content)?,
            "assistant" => emit_agent_text_chunk(sink, sid, &msg.content)?,
            _ => {}
        }
    }
    Ok(())
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
        .ok_or_else(|| format!("unknown modeId '{mode_id}' ({})", AgentMode::EXPECTED))?;
    apply_live_mode(bridge, sid, mode).await?;
    Ok(json!({}))
}

async fn apply_live_mode(bridge: &Bridge, sid: &str, mode: AgentMode) -> Result<(), String> {
    {
        let mut map = bridge.acp_sessions.lock().await;
        let sess = map
            .get_mut(sid)
            .ok_or_else(|| format!("unknown sessionId '{sid}'"))?;
        if sess.mode != mode {
            tracing::info!(session_id = %sid, from = %sess.mode.id(), to = %mode.id(), "session/set_mode");
            sess.mode = mode;
            if !mode.is_converse() {
                sess.converse = None;
            }
            if let Err(e) = session_store::update_mode(sid, mode.id()) {
                tracing::warn!(session_id = %sid, error = %e, "session/set_mode: failed to update persisted mode");
            }
        }
    }
    if mode.is_converse() {
        ensure_converse(bridge, sid).await?;
    }
    Ok(())
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

    fn write_rpc_response(
        &self,
        id: Value,
        outcome: Result<Value, JsonRpcErrorBody>,
    ) -> Result<(), String> {
        self.inner.write_rpc_response(id, outcome)
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
        if let Some(converse) = &sess.converse {
            let _ = converse.cancel_tx.send(false);
        }
        sess.mode
    };

    // Wrap the real sink so we can capture assistant text for persistence.
    let capturing = CapturingSink {
        inner: sink,
        captured: std::sync::Mutex::new(String::new()),
    };

    let result = match mode {
        AgentMode::Coding | AgentMode::Chat => {
            run_converse_prompt(Arc::clone(&bridge), &capturing, &sid, &text).await
        }
        AgentMode::Goal => run_goal_prompt(Arc::clone(&bridge), &capturing, &sid, &text).await,
        AgentMode::Face => run_face_prompt(Arc::clone(&bridge), &capturing, &sid, &text).await,
    };

    // Persist the user message and the agent reply (soft — failure never fails the turn).
    let assistant_text = capturing.captured.into_inner().unwrap_or_default();
    if let Err(e) = session_store::append_messages(&sid, &text, &assistant_text) {
        tracing::warn!(session_id = %sid, error = %e, "session/prompt: failed to persist messages");
    }

    result
}

/// One-shot `/goal` path: LiberadoLoopBackend + durable worktree (ACP mode `goal`).
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
    let preserved = preserved_worktree_report(workspace, label).await;
    emit_finish_report(sink, sid, outcome.as_deref(), &preserved)
}

async fn run_goal_prompt(
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
    drain_coding_events(sink, sid, &mut ev_rx, &mut pending_tool_ids)?;

    return finish_coding_tail(bridge, sink, sid, &workspace, joined).await;
}

/// Persist the finished round's coding state, report the outcome, and write the run artifact.
///
/// Persist only after the pack finished (not mid-cancel). Preserve the workspace on failure
/// too: a failed run's diff is the evidence, and it is lost if nobody commits it.
async fn finish_coding_tail(
    bridge: Arc<Bridge>,
    sink: &dyn WireSink,
    sid: &str,
    workspace: &std::path::Path,
    joined: Result<
        (
            coding_run::CodingSessionState,
            Result<coding_run::CodingRoundOutcome, String>,
        ),
        tokio::task::JoinError,
    >,
) -> Result<Value, String> {
    let (state, outcome) = joined.map_err(|e| format!("coding task panicked: {e}"))?;

    // Persist coding state only when the pack finished (not mid-cancel).
    persist_coding_state(&bridge, sid, state).await;

    // Preserve before reporting, and on the failure path too: a failed run's diff is the
    // evidence for why it failed, and it is just as lost if nobody commits it.
    let (label, verdict) = coding_verdict(sink, sid, outcome)?;
    finish_coding_run(sink, sid, workspace, label, verdict).await?;
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

/// Interactive coding or chat: lasting Conversation + Executor. Coding attaches tools.
async fn run_converse_prompt(
    bridge: Arc<Bridge>,
    sink: &dyn WireSink,
    sid: &str,
    text: &str,
) -> Result<Value, String> {
    let session = ensure_converse(&bridge, sid).await?;
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

    render_face_prompt_result(sink, sid, result)
}

/// Open or rebuild the converse handle so it matches the session's current mode.
///
/// Interactive coding and chat share this handle. A mode switch that changes whether
/// coding tools are attached rebuilds the conversation, keeping user/assistant turns.
async fn ensure_converse(bridge: &Bridge, sid: &str) -> Result<Arc<SessionHandle>, String> {
    let (mode, cwd, reuse, live_history) = {
        let map = bridge.acp_sessions.lock().await;
        let sess = map
            .get(sid)
            .ok_or_else(|| format!("unknown sessionId '{sid}'"))?;
        if !sess.mode.is_converse() {
            return Err(format!(
                "mode '{}' is not a conversation (coding|chat)",
                sess.mode.id()
            ));
        }
        let want_tools = sess.mode.uses_coding_tools();
        match &sess.converse {
            Some(handle) if handle.coding_tools == want_tools => (
                sess.mode,
                sess.cwd.clone(),
                Some(Arc::clone(handle)),
                Vec::new(),
            ),
            Some(handle) => {
                let history = snapshot_turns(&handle.conversation);
                (sess.mode, sess.cwd.clone(), None, history)
            }
            None => (sess.mode, sess.cwd.clone(), None, Vec::new()),
        }
    };
    if let Some(handle) = reuse {
        return Ok(handle);
    }

    let stored = if live_history.is_empty() {
        session_store::load(sid)
            .ok()
            .flatten()
            .map(|r| r.messages)
            .unwrap_or_default()
    } else {
        live_history
    };

    let handle = if mode.uses_coding_tools() {
        let permission = permission_attach(bridge, sid, &cwd);
        let parts = interactive::prepare_coding_converse(
            &cwd,
            sid,
            &bridge.coder_tuning,
            ask_human::may_ask_human(&bridge.local_grant),
            bridge.config_dir.as_deref(),
            permission,
        )
        .await?;
        open_handle(
            sid,
            Arc::clone(&bridge.provider),
            bridge.max_turns,
            parts.system,
            &stored,
            parts.tools,
            true,
        )
    } else {
        open_handle(
            sid,
            Arc::clone(&bridge.provider),
            bridge.max_turns,
            chat_system_prompt(&cwd, bridge.system_prompt.as_deref()),
            &stored,
            Arc::new(NoTools),
            false,
        )
    };
    let handle = Arc::new(handle);
    if let Some(sess) = bridge.acp_sessions.lock().await.get_mut(sid) {
        sess.converse = Some(Arc::clone(&handle));
    }
    Ok(handle)
}

fn permission_attach(
    bridge: &Bridge,
    session_id: &str,
    cwd: &std::path::Path,
) -> Option<permission::PermissionAttach> {
    Some(permission::PermissionAttach {
        ask: Arc::clone(&bridge.permissions) as Arc<dyn permission::PermissionAsk>,
        session_id: session_id.to_string(),
        client_cwd: cwd.to_path_buf(),
        global_grant_dir: None,
        policy: bridge.coder_tuning.command_policy.clone(),
    })
}

fn snapshot_turns(conversation: &Mutex<Conversation>) -> Vec<session_store::StoredMessage> {
    let Ok(convo) = conversation.try_lock() else {
        return Vec::new();
    };
    convo
        .messages()
        .iter()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant) && !m.content.is_empty())
        .map(|m| session_store::StoredMessage {
            role: match m.role {
                Role::User => "user".into(),
                Role::Assistant => "assistant".into(),
                _ => "assistant".into(),
            },
            content: m.content.clone(),
        })
        .collect()
}

fn chat_system_prompt(cwd: &std::path::Path, configured: Option<&str>) -> String {
    configured
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            format!(
                "{DEFAULT_SYSTEM_PROMPT}\n\n\
                 You are Liberado chat (ACP). Workspace context path: {}.\n\
                 This mode is conversational only — no file tools. For interactive coding, switch \
                 mode to **coding**. For an unattended /goal run, switch to **goal**. For \
                 vault/delegate, switch to **face** (daemon required).",
                cwd.display()
            )
        })
}

/// Open a converse session, optionally seeded with a stored transcript.
///
/// `history` is what makes a resume a resume. Replaying the transcript to the *client* only
/// repaints the editor; if the conversation the model sees starts empty, the user is looking at
/// their own history while the agent has none of it. That is the exact failure `loadSession:
/// false` was chosen to avoid — and it is worse once the flag says `true`, because the interface
/// now claims the memory is there.
fn open_handle(
    session_id: &str,
    provider: Arc<dyn Provider>,
    max_turns: u32,
    system: String,
    history: &[session_store::StoredMessage],
    tools: Arc<dyn ToolRuntime>,
    coding_tools: bool,
) -> SessionHandle {
    let (cancel_tx, cancel_rx) = watch::channel(false);
    SessionHandle {
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
        executor: Executor::new(provider, Budget::new(max_turns)),
        tools,
        coding_tools,
        pending_ask: std::sync::Mutex::new(None),
        cancel_tx,
        cancel_rx,
    }
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
        let result = drive_converse_turn(&turn_session, &text, &event_tx).await;
        let _ = event_tx.send(converse_terminal_event(&result)).await;
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

    remember_parked_ask(&session, turn.await);
    Ok(stop_reason)
}

fn converse_terminal_event(result: &Result<(), ExecError>) -> AgentEvent {
    match result {
        Ok(()) | Err(ExecError::AwaitingHuman { .. }) => AgentEvent::Done,
        Err(e) => AgentEvent::Error(e.to_string()),
    }
}

fn remember_parked_ask(
    session: &SessionHandle,
    joined: Result<Result<(), ExecError>, tokio::task::JoinError>,
) {
    let Ok(Err(ExecError::AwaitingHuman { call_id })) = joined else {
        return;
    };
    if let Ok(mut pending) = session.pending_ask.lock() {
        *pending = Some(call_id);
    }
}

/// One converse drive: either a fresh user turn or the answer to a parked `ask_human`.
async fn drive_converse_turn(
    session: &SessionHandle,
    text: &str,
    events: &tokio::sync::mpsc::Sender<AgentEvent>,
) -> Result<(), ExecError> {
    let pending = session.pending_ask.lock().ok().and_then(|mut g| g.take());
    let mut convo = session.conversation.lock().await;
    let result = if let Some(call_id) = pending.as_ref() {
        convo
            .resume_stream(
                &session.executor,
                session.tools.as_ref(),
                call_id,
                text,
                events,
            )
            .await
    } else {
        convo
            .turn_stream(&session.executor, session.tools.as_ref(), text, events)
            .await
    };
    if let Err(ExecError::AwaitingHuman { .. }) = &result {
        return result;
    }
    if result.is_err()
        && let Some(call_id) = pending
        && let Ok(mut g) = session.pending_ask.lock()
    {
        *g = Some(call_id);
    }
    result
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
#[path = "main_test_support.rs"]
mod test_support;

#[cfg(test)]
mod main_cli_args_tests;

#[cfg(test)]
mod main_dispatch_tests;

#[cfg(test)]
mod main_session_catalog_tests;

#[cfg(test)]
mod main_prompt_event_tests;

#[cfg(test)]
mod main_misc_tests;
