//! `liberado-worker` — the delegation worker's composition root (plan §4, D1).
//!
//! Wires concrete choices the library layers leave open: Gitea forge from flags/env,
//! provider factory from the worker's own topology profile, token from an env var.
//! Argument parsing lives in [`liberado_worker::cli`] so it can be covered; this file
//! stays a shell over `run`.

use std::sync::Arc;

use liberado_worker::cli::{Args, ProcessEnv, parse_args};
use liberado_worker::config::{self, WorkerSettings};
use liberado_worker::http::{AppState, router};
use liberado_worker::queue::TaskStore;
use liberado_worker::runner::RunContext;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args = parse_args(std::env::args().skip(1), &ProcessEnv)?;
    let profile = config::provider_profile(
        args.config_dir.as_deref(),
        std::env::var("LIBERADO_CODER_PROVIDER").ok().as_deref(),
    )?;

    let settings = Arc::new(settings_from_args(&args, &profile));

    // Forge construction happens here because the token is a secret; the runner only
    // ever sees the trait object.
    let forge: Option<Arc<dyn liberado_forge::ForgeClient>> = match &settings.forge_url {
        Some(url) if !settings.forge_token.is_empty() => Some(Arc::new(
            liberado_forge::gitea::GiteaForge::with_tls(
                url,
                &settings.forge_token,
                settings.forge_insecure_tls,
            )
            .map_err(|error| format!("forge client: {error}"))?,
        )),
        _ => {
            tracing::warn!(
                "no forge configured (--forge-url + --forge-token-env); tasks will be \
                 rejected when they reach the PR step"
            );
            None
        }
    };

    let store = Arc::new(TaskStore::open(&args.data_dir).map_err(|error| error.to_string())?);
    rescan_after_restart(&store, &settings, &profile, &forge).await?;

    let run_ctx = RunContext::production(settings.clone(), store.clone(), profile, forge)?;
    let state = Arc::new(AppState {
        slots: Arc::new(tokio::sync::Semaphore::new(settings.max_concurrent)),
        settings: settings.clone(),
        store,
        run: run_ctx,
    });

    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .map_err(|error| format!("bind {}: {error}", args.bind))?;
    tracing::info!(bind = %args.bind, fingerprint = %liberado_worker::build_fingerprint(), "worker listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|error| format!("serve: {error}"))?;
    Ok(())
}

fn settings_from_args(
    args: &Args,
    profile: &liberado_config_loader::ProviderProfile,
) -> WorkerSettings {
    WorkerSettings {
        bind: args.bind.clone(),
        token: args.token.clone(),
        data_dir: args.data_dir.clone(),
        config_dir: args.config_dir.clone(),
        model: Some(args.effective_model(&profile.default_model)),
        forge_url: args.forge_url.clone(),
        forge_token: args.forge_token.clone(),
        forge_insecure_tls: args.forge_insecure_tls,
        clone_base_url: args.clone_base_url.clone(),
        max_concurrent: args.max_concurrent,
    }
}

/// Restart recovery (plan §14): queued work re-spawns; a task found mid-run after a
/// restart cannot be trusted to still be executing, so it reports honestly as failed.
/// The worktree and any pushed branch survive either way.
async fn rescan_after_restart(
    store: &Arc<TaskStore>,
    settings: &Arc<WorkerSettings>,
    profile: &liberado_config_loader::ProviderProfile,
    forge: &Option<Arc<dyn liberado_forge::ForgeClient>>,
) -> Result<(), String> {
    use liberado_delegate_contract::{TaskId, TaskStatus};

    for id in store.known_ids().map_err(|error| error.to_string())? {
        let Some(record) = store.get(&id).map_err(|error| error.to_string())? else {
            continue;
        };
        match record.status {
            TaskStatus::Queued => {
                tracing::info!(task = %id, "re-spawning queued task after restart");
                let ctx = RunContext::production(
                    settings.clone(),
                    store.clone(),
                    profile.clone(),
                    forge.clone(),
                )?;
                tokio::spawn(liberado_worker::runner::execute(ctx, record.spec));
            }
            TaskStatus::Running => {
                tracing::warn!(task = %id, "found Running at startup; marking failed");
                store
                    .finish(
                        &TaskId(id.clone()),
                        TaskStatus::Failed {
                            reason: "worker restarted mid-run".into(),
                        },
                    )
                    .map_err(|error| error.to_string())?;
            }
            _ => {}
        }
    }
    Ok(())
}
