//! `liberado-free-proxy` — the binary half of the free-model proxy.
//!
//! Reads its wiring from the environment (documented in the crate root and
//! [`liberado_provider_free_proxy::settings`]), fails fast when **no** provider key is set,
//! and serves until stopped. Deliberately no config file: every knob is a deployment fact,
//! and Liberado's own `[[providers]]` entry points *at* this process, so the proxy's
//! contract is a URL, not a schema.
//!
//! The first ranking resolves in the background: serving `/healthz` immediately matters more
//! than warming `/v1/models`, and a broken deployment says so on first use instead of refusing
//! to boot.

use std::sync::Arc;

use liberado_provider_free_proxy::providers::{
    UpstreamRegistry, any_listed_key_set, configured_upstreams, listed_key_env_names,
};
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

fn lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Exit 2 listing env *names* when no listed provider key is set. Never prints values.
fn abort_if_no_provider_keys() {
    if !any_listed_key_set(lookup) {
        eprintln!(
            "liberado-free-proxy: no provider API key is set. Export at least one of:\n  {}",
            listed_key_env_names().join("\n  ")
        );
        std::process::exit(2);
    }
}

fn warn_if_spider_unset(spider: &Option<SpiderClient>) {
    if spider.is_none() {
        eprintln!(
            "SPIDER_MCP_URL unset — the scrape fallback is disabled; ranking relies on the \
             Benchmarks API (when OpenRouter is configured) and the heuristic floor"
        );
    }
}

/// Upstreams, settings, sources, resolver, and the HTTP service. Ranking is not awaited here.
fn wire_service(
    lookup: impl Fn(&str) -> Option<String>,
) -> (Arc<ProxyService>, Arc<BestFreeModelResolver>, String) {
    let upstreams = configured_upstreams(lookup);
    let settings = ProxySettings::from_env();
    let spider = SpiderClient::from_env();
    warn_if_spider_unset(&spider);

    let registry = UpstreamRegistry::from_upstreams(upstreams.clone());
    let sources = Arc::new(
        DefaultSources::from_upstreams(upstreams, spider)
            .with_scrape_timeout_secs(settings.scrape_timeout_secs),
    );
    let resolver = Arc::new(BestFreeModelResolver::with_defaults(
        sources,
        settings.ttl_secs,
    ));
    let service = Arc::new(ProxyService::new(
        Arc::clone(&resolver),
        ProxyConfig {
            max_attempts: settings.max_attempts,
            attempt_timeout: liberado_provider_free_proxy::service::DEFAULT_ATTEMPT_TIMEOUT,
            registry,
        },
    ));
    (service, resolver, settings.bind)
}

fn spawn_initial_ranking(resolver: Arc<BestFreeModelResolver>) {
    tokio::spawn(async move {
        match resolver.current().await {
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
}

async fn bind_and_serve(app: axum::Router, bind: String) {
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|e| panic!("cannot bind {bind}: {e}"));
    let bound = listener
        .local_addr()
        .unwrap_or_else(|e| panic!("cannot read bound address: {e}"));
    tracing::info!(%bound, "liberado-free-proxy listening");
    axum::serve(listener, app).await.expect("server run");
}

#[tokio::main]
async fn main() {
    tracing_subscriber_init();
    abort_if_no_provider_keys();
    let (service, resolver, bind) = wire_service(lookup);
    spawn_initial_ranking(resolver);
    bind_and_serve(liberado_provider_free_proxy::http_router(service), bind).await;
}
