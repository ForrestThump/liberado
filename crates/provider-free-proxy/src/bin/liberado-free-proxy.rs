//! `liberado-free-proxy` — the binary half of the free-model proxy.
//!
//! Reads its wiring from the environment (documented in the crate root and
//! [`liberado_provider_free_proxy::settings`]), fails fast when the upstream credential is
//! missing, and serves until stopped. Deliberately no config file: every knob is a deployment
//! fact, and Liberado's own `[[providers]]` entry points *at* this process, so the proxy's
//! contract is a URL, not a schema.
//!
//! The first ranking resolves in the background: serving `/healthz` immediately matters more
//! than warming `/v1/models`, and a broken deployment says so on first use instead of refusing
//! to boot.

use std::sync::Arc;

use liberado_provider_free_proxy::resolver::{BestFreeModelResolver, DefaultSources};
use liberado_provider_free_proxy::service::{ProxyConfig, ProxyService};
use liberado_provider_free_proxy::settings::ProxySettings;
use liberado_provider_free_proxy::spider::SpiderClient;

fn tracing_subscriber_init() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        // Logs belong on stderr: supervisors and scripts capture stderr for diagnostics, and
        // stdout stays reserved for data. fmt()'s default is stdout, which silently empties
        // every `2>`-based capture.
        .with_writer(std::io::stderr)
        // Machine-read output: no terminal escapes even when someone runs it by hand.
        .with_ansi(false)
        .init();
}

#[tokio::main]
async fn main() {
    tracing_subscriber_init();

    let Ok(api_key) = std::env::var("OPENROUTER_API_KEY") else {
        eprintln!(
            "liberado-free-proxy: OPENROUTER_API_KEY is not set. Free models still require an \
             OpenRouter account; export the key and restart."
        );
        std::process::exit(2);
    };

    let settings = ProxySettings::from_env();
    let spider = SpiderClient::from_env();
    if spider.is_none() {
        eprintln!(
            "SPIDER_MCP_URL unset — the scrape fallback is disabled; ranking relies on the \
             Benchmarks API alone"
        );
    }

    let sources = Arc::new(
        DefaultSources::new(
            settings.upstream_base.clone(),
            Some(api_key.clone()),
            spider,
        )
        .with_scrape_timeout_secs(settings.scrape_timeout_secs),
    );
    let resolver = Arc::new(BestFreeModelResolver::with_defaults(
        sources,
        settings.ttl_secs,
    ));
    let service = Arc::new(ProxyService::new(
        Arc::clone(&resolver),
        ProxyConfig {
            upstream_base: settings.upstream_base,
            upstream_api_key: api_key,
            max_attempts: settings.max_attempts,
        },
    ));

    let boot_resolver = Arc::clone(&resolver);
    tokio::spawn(async move {
        match boot_resolver.current().await {
            Ok(resolution) => tracing::info!(
                origin = resolution.origin.label(),
                models = resolution.ranked.len(),
                best = resolution
                    .ranked
                    .first()
                    .map(|m| m.id.as_str())
                    .unwrap_or("(none)")
            ),
            Err(e) => {
                tracing::warn!(error = %e, "initial ranking unresolved; will retry per request")
            }
        }
    });

    let app = liberado_provider_free_proxy::http_router(service);
    let listener = tokio::net::TcpListener::bind(&settings.bind)
        .await
        .unwrap_or_else(|e| panic!("cannot bind {}: {e}", settings.bind));
    let bound = listener
        .local_addr()
        .unwrap_or_else(|e| panic!("cannot read bound address: {e}"));
    tracing::info!(%bound, "liberado-free-proxy listening");
    axum::serve(listener, app).await.expect("server run");
}
