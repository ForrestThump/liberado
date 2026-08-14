//! The `liberado` binary — the single entry point: a thin client + launcher over the daemon server
//! (`liberado_server`). It owns nothing but argument dispatch and tracing-subscriber init; all daemon
//! and chat logic lives in libraries.
//!
//! Usage:
//!   liberado serve <vault-path>      run the daemon + HTTP/SSE API in the foreground
//!   liberado <vault-path>            back-compat alias for `serve`
//!   LIBERADO_VAULT=<vault> liberado  same, taking the vault from the environment
//!   liberado chat [session-id]       the streaming terminal client of a running daemon
//!   liberado config check            load + validate config, print a summary (or an error)
//!   liberado ci check                run the cross-platform repository ship preflight
//!   liberado docs check-links         check relative Markdown links
//!   liberado docs crate-map           check the generated crate map
//!   liberado docs crate-map --write   regenerate the crate map
//!   liberado docs metadata <command>  lint or generate documentation metadata
//!   liberado prompt \[profile\]        print the system prompt a chat under <profile> would get
//!   liberado coder trace <id>        render a durable coding trace as a human transcript
//!   liberado coder compare <a> <b>   side-by-side harness metrics for two native traces
//!   liberado coder summarize <path>  summarize a cross-harness compare run
//!   liberado coder import <file>     foreign (Kilo / OpenHands) → `.messages.json`
//!
//! `serve` runs in the foreground, hosting the vault watch loop and the chat/HTTP/SSE API until
//! killed. `chat` is a thin HTTP/SSE client of a separately-running daemon (see [`chat_client`]).
//! `config check` resolves the config dir (`LIBERADO_CONFIG_DIR` or the platform default) and runs
//! the loader, reporting what it found or the first actionable error. `prompt` composes the system
//! prompt a chat would actually be given from that same config — the model's-eye view, without a
//! daemon. Reactions are logged to stderr by the server; stdout is left for data.

mod chat_client;
mod ci_cmd;
mod coder_cmd;
mod crate_map_cmd;
mod docs_cmd;
mod docs_meta_cmd;
mod summarize_cmd;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr) // logs to stderr (unbuffered); stdout stays for data
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // First-arg dispatch: `chat` is the streaming client; `serve` (or a bare vault path, for
    // back-compat) launches the daemon server. With no arg, fall back to `LIBERADO_VAULT`.
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("chat") => chat_client::run(args.next()).await,
        // Harness observability over durable coding traces (F1–F3). Synchronous — no daemon.
        Some("coder") => coder_cmd::run(args),
        Some("ci") => match args.next().as_deref() {
            Some("check") => ci_cmd::check(),
            _ => Err("usage: liberado ci check".into()),
        },
        Some("docs") => match args.next().as_deref() {
            Some("check-links") => docs_cmd::check_links(),
            Some("crate-map") => {
                let write = args.next().as_deref() == Some("--write");
                crate_map_cmd::check_or_write(&crate_map_cmd::repository_root()?, write)
            }
            Some("metadata") => {
                let command = args.next().ok_or(
                    "usage: liberado docs metadata <lint|generate|check-stale-rs|self-test>",
                )?;
                docs_meta_cmd::run(&crate_map_cmd::repository_root()?, &command)
            }
            _ => Err("usage: liberado docs <check-links|crate-map|metadata>".into()),
        },
        Some("config") => match args.next().as_deref() {
            // `config check` is synchronous (no daemon): resolve the default dir via bootstrap
            // (passing None) and run the loader. Routed through the server so the cli keeps a single
            // dependency.
            Some("check") => liberado_server::config_check(None),
            // `config explain <component> <mcp:tool> <path>` — answers "would this write be
            // allowed, and if not, which guard stops it?" from config alone. Every guard's verdict
            // is printed, not just the first failure: the first `no` is rarely the only one, and
            // discovering them one deploy at a time is the slow path.
            Some("explain") => {
                let component = args.next();
                let tool = args.next();
                let path = args.next();
                match (component, tool, path) {
                    (Some(c), Some(t), Some(p)) => liberado_server::explain_write(None, &c, &t, &p),
                    _ => Err(
                        "usage: liberado config explain <component> <mcp:tool> <vault/path.md>\n  \
                         e.g. liberado config explain dispatcher turbovault:write_note Learning/x.md"
                            .into(),
                    ),
                }
            }
            _ => Err("usage: liberado config <check|explain>".into()),
        },
        // `prompt [profile]` — what a chat under that profile is actually told, composed from
        // config alone. No daemon, so it runs mid-debug and in CI, which is the point: the prompt
        // and the tool list disagreeing is a class of bug that otherwise costs a deploy to see.
        // A bare `prompt` shows the no-profile case.
        Some("prompt") => liberado_server::show_prompt(None, args.next().as_deref()),
        Some("serve") => {
            let vault = args
                .next()
                .or_else(|| std::env::var("LIBERADO_VAULT").ok())
                .ok_or("usage: liberado serve <vault-path>  (or set LIBERADO_VAULT)")?;
            liberado_server::run(vault).await
        }
        Some(vault) => liberado_server::run(vault.to_string()).await, // back-compat: bare vault == serve
        None => {
            let vault = std::env::var("LIBERADO_VAULT").map_err(
                |_| "usage: liberado [serve <vault>|chat [session]]  (or set LIBERADO_VAULT)",
            )?;
            liberado_server::run(vault).await
        }
    }
}
