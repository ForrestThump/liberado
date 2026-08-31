//! The `liberado` command line: argument routing + launcher over the daemon server
//! ([`liberado_server`]). It owns nothing but dispatch and tracing-subscriber
//! init; all daemon and chat logic lives in libraries.
//!
//! This is a *library* target housing the real logic, with `main.rs` a three-line shell over
//! [`run`]. The shape matters beyond taste: coverage gates attribute per-function results to
//! library targets only, so a router buried in a bin scores as permanently untested no matter
//! what the test suite does (the CRAP ratchet reads that as cc²+cc).
//!
//! Usage:
//!   liberado serve \\<\1\\>      run the daemon + HTTP/SSE API in the foreground
//!   liberado \\<\1\\>            back-compat alias for `serve`
//!   LIBERADO_VAULT=\\<\1\\> liberado  same, taking the vault from the environment
//!   liberado chat [session-id]       the streaming terminal client of a running daemon
//!   liberado config check            load + validate config, print a summary (or an error)
//!   liberado ci                      full local CI; ratchet and stage/amend crap-baseline.json
//!   liberado ci check                ship preflight (fmt, clippy, tests, deny)
//!   liberado ci crap                 compare CRAP scores to crap-baseline.json (no write)
//!   liberado ci ready                final checks + exact Linux CRAP + readiness receipt
//!   liberado ci verify-ready         reject a stale readiness receipt
//!   liberado ci crap-linux           native Debian CRAP; Debian WSL on Windows
//!   liberado ci ratchet              check, write baseline, then stage or amend it
//!                                    console is a summary; full child log is .liberado/ci.log
//!   liberado shepherd --once          run the unattended PR shepherd once
//!   liberado deploy <homelab|webui|smoke|latency> run configured deployment operations
//!   liberado dev <start|stop|webui-start|webui-stop|status|tui> manage local processes
//!   liberado paseo install            install ACP bridge and merge Paseo configuration
//!   liberado ops config check         validate the resolved operations TOML
//!   liberado docs check-links         check relative Markdown links
//!   liberado docs crate-map           check the generated crate map
//!   liberado docs crate-map --write   regenerate the crate map
//!   liberado docs metadata \\<\1\\>  lint or generate documentation metadata
//!   liberado docs site [--out \\<\1\\>] generate the searchable documentation site
//!   liberado prompt \[profile\]        print the system prompt a chat under \\<\1\\> would get
//!   liberado coder trace \\<\1\\>        render a durable coding trace as a human transcript
//!   liberado coder compare \\<\1\\> \\<\1\\>   side-by-side harness metrics for two native traces
//!   liberado coder compare prepare   create isolated, pinned comparison worktrees
//!   liberado coder compare run       run both harnesses and preserve all results
//!   liberado coder compare save      commit and archive one harness result
//!   liberado coder compare reset     restore tracked files in a compare workspace
//!   liberado coder compare submit    enqueue a durable user-worker comparison job
//!   liberado coder compare await     wait locally for one comparison job
//!   liberado coder summarize \\<\1\\>  summarize a cross-harness compare run
//!   liberado coder smoke              validate the coder runner process boundary
//!   liberado coder import \\<\1\\>     foreign (Kilo / OpenHands) → `.messages.json`
//!   liberado mutants run \\<\1\\>   run cargo-mutants and append to mutants-ledger.json
//!   liberado mutants record [crate-dir] ingest mutants.out/outcomes.json into the ledger
//!   liberado mutants report [--all]    print never/historical/drift campaign health
//!   liberado mutants next [--all]        suggest the next crate to mutation-test
//!
//! `serve` runs in the foreground, hosting the vault watch loop and the chat/HTTP/SSE API until
//! killed. `chat` is a thin HTTP/SSE client of a separately-running daemon (see `chat_client`).
//! `config check` resolves the config dir (`LIBERADO_CONFIG_DIR` or the platform default) and runs
//! the loader, reporting what it found or the first actionable error. `prompt` composes the system
//! prompt a chat would actually be given from that same config — the model's-eye view, without a
//! daemon. Reactions are logged to stderr by the server; stdout is left for data.

mod branch_cleaner_ci;
mod chat_client;
mod ci_cmd;
mod coder_cmd;
mod compare_cmd;
mod crate_map_cmd;
mod dependency_security_cmd;
mod docs_audit_cmd;
mod docs_cmd;
mod docs_meta_cmd;
mod docs_site_cmd;
mod function_complexity_cmd;
mod module_health_cmd;
mod mutants_cmd;
mod ops_cmd;
mod readiness_cmd;
mod shepherd_cmd;
mod summarize_cmd;
pub mod unwrap_classification_cmd;

/// Install the stderr tracing subscriber, then route `args` (argv without the program name). The
/// binary's `main` is exactly this — every decision above the sub-command modules happens in
/// `dispatch`.
pub async fn run(
    args: &mut impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    dispatch(args).await
}

/// Install the stderr tracing subscriber. Logs go to stderr (unbuffered); stdout stays for data.
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

/// First-arg dispatch: `chat` is the streaming client; `serve` (or a bare vault path, for
/// back-compat) launches the daemon server. With no arg, fall back to `LIBERADO_VAULT`.
///
/// Extracted from `main` so the argument grammar is a plain function and the binary stays a
/// thin shell over the command modules. The daemon-adjacent arms stay here; the synchronous
/// sub-command groups route through `route_named`, so the router's branch count stays flat as
/// commands are added.
async fn dispatch(
    args: &mut impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    match args.next().as_deref() {
        Some("chat") => chat_client::run(args.next()).await,
        // `prompt [profile]` — what a chat under that profile is actually told, composed from
        // config alone. No daemon, so it runs mid-debug and in CI, which is the point: the prompt
        // and the tool list disagreeing is a class of bug that otherwise costs a deploy to see.
        // A bare `prompt` shows the no-profile case.
        Some("prompt") => liberado_server::show_prompt(None, args.next().as_deref()),
        Some("serve") => run_serve(args.next()).await,
        None => run_serve_from_env().await,
        Some(named) => route_named(named, args).await,
    }
}

/// A named first argument that is not one of dispatch's own arms: either one of the synchronous
/// sub-command groups, or the bare-vault back-compat alias for `serve`.
async fn route_named(
    name: &str,
    args: &mut impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if matches!(name, "deploy" | "dev" | "paseo" | "ops") {
        let name = name.to_string();
        let arguments: Vec<String> = args.collect();
        return tokio::task::spawn_blocking(move || run_operations_route(&name, arguments))
            .await
            .map_err(|error| format!("operations task failed: {error}"))?
            .map_err(Into::into);
    }
    match route_sync(name, args) {
        Some(result) => result,
        None => liberado_server::run(name.to_string()).await,
    }
}

/// Operator commands use blocking filesystem, process, SSH, and HTTP clients by design. Keep the
/// complete command on Tokio's blocking pool: dropping reqwest's blocking client on an async
/// worker panics because that client owns a small internal runtime.
fn run_operations_route(name: &str, arguments: Vec<String>) -> Result<(), String> {
    let mut args = arguments.into_iter();
    let result = match name {
        "deploy" => ops_cmd::run_deploy(&mut args),
        "dev" => ops_cmd::run_dev(&mut args),
        "paseo" => ops_cmd::run_paseo(&mut args),
        "ops" => ops_cmd::run_ops(&mut args),
        _ => return Err(format!("unknown operations command: {name}")),
    };
    result.map_err(|error| error.to_string())
}

/// The synchronous sub-command groups, by first argument. Each wrapper owns its own
/// argument match; `None` means "not a known group", leaving the caller free to fall back.
fn route_sync(
    name: &str,
    args: &mut impl Iterator<Item = String>,
) -> Option<Result<(), Box<dyn std::error::Error>>> {
    let route = match name {
        "coder" => coder_cmd::run(args),
        "mutants" => cmd_mutants(args),
        "ci" => cmd_ci(args),
        "shepherd" => shepherd_cmd::run(args),
        "docs" => cmd_docs(args),
        "config" => cmd_config(args),
        _ => return None,
    };
    Some(route)
}

/// `liberado ci …` — ship preflight, CRAP check, or the local check-then-ratchet run.
fn cmd_ci(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    ci_cmd::run(args)
}

/// `liberado mutants …` — campaign ledger run/record/report/next.
fn cmd_mutants(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.next().as_deref() {
        Some("run") => mutants_cmd::run(args),
        Some("record") => mutants_cmd::record(args),
        Some("report") => mutants_cmd::report(args),
        Some("next") => mutants_cmd::next_crate(args),
        _ => Err("usage: liberado mutants <run|record|report|next> …\n\
             run:    liberado mutants run [--lib-only] <crate-dir>\n\
             record: liberado mutants record [crate-dir]\n\
             report: liberado mutants report [--all]\n\
             next:   liberado mutants next [--all]"
            .into()),
    }
}

/// `liberado docs …`
fn cmd_docs(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.next().as_deref() {
        Some("check-links") => docs_cmd::check_links(),
        Some("crate-map") => {
            let arguments: Vec<_> = args.collect();
            let write = match arguments.as_slice() {
                [] => false,
                [flag] if flag == "--write" => true,
                _ => return Err("usage: liberado docs crate-map [--write]".into()),
            };
            crate_map_cmd::check_or_write(&crate_map_cmd::repository_root()?, write)
        }
        Some("metadata") => {
            let command = args
                .next()
                .ok_or("usage: liberado docs metadata <lint|generate|check-stale-rs|self-test>")?;
            if args.next().is_some() {
                return Err(
                    "usage: liberado docs metadata <lint|generate|check-stale-rs|self-test>".into(),
                );
            }
            docs_meta_cmd::run(&crate_map_cmd::repository_root()?, &command)
        }
        command => cmd_docs_auxiliary(command, args),
    }
}

fn cmd_docs_auxiliary(
    command: Option<&str>,
    args: &mut impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Some("audit") => docs_audit_cmd::run(&crate_map_cmd::repository_root()?, args),
        Some("site") => docs_site_cmd::run(args),
        _ => Err("usage: liberado docs <audit|check-links|crate-map|metadata|site>".into()),
    }
}

/// `liberado config …`
fn cmd_config(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.next().as_deref() {
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
    }
}

/// `liberado serve [<vault>]` — run the daemon in the foreground, or fall back to the
/// `LIBERADO_VAULT` environment variable.
async fn run_serve(vault: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault
        .or_else(|| std::env::var("LIBERADO_VAULT").ok())
        .ok_or("usage: liberado serve <vault-path>  (or set LIBERADO_VAULT)")?;
    liberado_server::run(vault).await
}

/// No first arg — the vault must come from `LIBERADO_VAULT`.
async fn run_serve_from_env() -> Result<(), Box<dyn std::error::Error>> {
    let vault = std::env::var("LIBERADO_VAULT")
        .map_err(|_| "usage: liberado [serve <vault>|chat [session]]  (or set LIBERADO_VAULT)")?;
    liberado_server::run(vault).await
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    /// The router exists so the argument grammar stays a plain function (see its doc comment).
    /// This pins the fail-fast contract: every named subcommand must reject an unknown
    /// sub-argument with its usage text instead of falling through to the bare-vault
    /// back-compat arm. Only pure, side-effect-free arms are exercised — the serve/chat/prompt
    /// arms reach for a daemon or network and stay out of unit tests.
    #[tokio::test]
    async fn named_subcommands_reject_unknown_subarguments_with_usage() {
        for command in ["ci", "coder", "mutants", "shepherd", "docs", "config"] {
            let args = [command.to_string(), "not-a-real-subcommand".to_string()];
            let err = dispatch(&mut args.into_iter())
                .await
                .expect_err("an unknown sub-argument must be rejected");
            let err = err.to_string();
            assert!(
                err.to_lowercase().contains("usage"),
                "{command}: expected usage text, got: {err}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn development_readiness_probe_runs_on_the_blocking_pool() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("ops.toml");
        std::fs::write(
            &config,
            format!(
                "[development]\ndaemon_url = \"http://127.0.0.1:{port}\"\ndaemon_port = {port}\n"
            ),
        )
        .unwrap();
        let args = [
            "dev".to_string(),
            "start".to_string(),
            "--config".to_string(),
            config.display().to_string(),
        ];

        dispatch(&mut args.into_iter()).await.unwrap();
        server.join().unwrap();
    }
}
