//! Model resolution: discovery + ranking + caching, behind one call.
//!
//! [`BestFreeModelResolver`] answers one question — *which free models, best-coding-first* — and
//! owns every policy decision that question implies:
//!
//! - **Source priority**: Benchmarks API when it answers, scraped leaderboards merged under it
//!   (the API never loses a field to a scrape), heuristic order as the floor.
//! - **Caching**: results live for a TTL because upstream benchmarks move slowly and the API is
//!   rate-limited (500/day/key); refreshes are single-flight so concurrent requests cannot stampede.
//! - **Staleness**: a failed *ranking* serves whatever it managed to learn; a failed *discovery*
//!   (transport) serves the previous snapshot rather than erroring — an old ordering beats no
//!   ordering at all, and only a first-ever transport failure is fatal. An empty free catalog
//!   is not a hiccup: the snapshot is dropped so chat cannot keep routing to now-paid slugs.
//!
//! Sources are traits so tests inject fixtures instead of network; production wiring lives in
//! [`DefaultSources`].

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::bench_api;
use crate::free::{self, FreeModel};
use crate::match_slug;
use crate::rank::{self, ModelScores, RankTable};
use crate::scrape_parse::{self, ScrapeSource};
use crate::spider::SpiderClient;

/// Where the current ordering came from — recorded so logs can say *why* a model was picked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Origin {
    /// OpenRouter Benchmarks API answered (optionally enriched by scrapes).
    BenchmarksApi,
    /// The API did not answer; scraped leaderboard(s) supplied the order.
    Scraped,
    /// Nothing ranked; heuristic order (tools, then context size).
    Heuristic,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::BenchmarksApi => "benchmarks-api",
            Origin::Scraped => "scraped-leaderboards",
            Origin::Heuristic => "heuristic",
        }
    }
}

/// One resolved snapshot: free models ordered best-coding-first.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub ranked: Vec<FreeModel>,
    pub origin: Origin,
    pub resolved_at: Instant,
}

impl Resolution {
    /// Slugs in ranked order — what routing and `/v1/models` consume.
    pub fn ranked_ids(&self) -> Vec<String> {
        self.ranked.iter().map(|m| m.id.clone()).collect()
    }

    /// Whether `slug` is currently servable (i.e. free).
    pub fn contains(&self, slug: &str) -> bool {
        self.ranked.iter().any(|m| m.id == slug)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("free-model discovery unavailable: {0}")]
    Discovery(String),
    #[error("no zero-priced free models from any configured provider")]
    NoFreeModels,
}

/// Free-model discovery (OpenRouter `/models`).
#[async_trait]
pub trait FreeModelDiscovery: Send + Sync {
    async fn discover(&self) -> Result<Vec<FreeModel>, String>;
}

/// Coding-benchmark rows keyed by OpenRouter slug (the Benchmarks API path).
#[async_trait]
pub trait CodingBenchmarkSource: Send + Sync {
    async fn coding_benchmark_rows(&self) -> Result<Vec<(String, ModelScores)>, String>;
}

/// Scraped leaderboard rows keyed by the page's own model names.
#[async_trait]
pub trait ScrapeRankingSource: Send + Sync {
    async fn scraped_leaderboard_rows(&self) -> Vec<(String, ModelScores)>;
}

/// Production wiring over real HTTP.
pub struct DefaultSources {
    http: reqwest::Client,
    providers: Vec<crate::providers::Upstream>,
    spider: Option<SpiderClient>,
    scrape_timeout_secs: u64,
}

impl DefaultSources {
    /// OpenRouter-only wiring, kept so existing tests can point one mock at `/models`.
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        spider: Option<SpiderClient>,
    ) -> Self {
        let providers = vec![crate::providers::Upstream::from_parts(
            "openrouter",
            base_url,
            "OPENROUTER_API_KEY",
            api_key.unwrap_or_default(),
            crate::providers::CatalogPolicy::ZeroPriceRequired,
            crate::providers::BillingKind::ZeroPricedOnly,
            crate::providers::ModelAllow::Chat,
        )];
        Self::from_upstreams(providers, spider)
    }

    /// Production wiring: every configured adapter.
    pub fn from_upstreams(
        providers: Vec<crate::providers::Upstream>,
        spider: Option<SpiderClient>,
    ) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("static client config"),
            providers,
            spider,
            scrape_timeout_secs: crate::settings::DEFAULT_SCRAPE_TIMEOUT_SECS,
        }
    }

    /// Per-scrape budget for the fallback sources.
    pub fn with_scrape_timeout_secs(mut self, secs: u64) -> Self {
        self.scrape_timeout_secs = secs.max(1);
        self
    }

    fn openrouter(&self) -> Option<&crate::providers::Upstream> {
        self.providers.iter().find(|u| u.id == "openrouter")
    }

    /// OpenRouter adapter with a non-empty bearer. Missing adapter and empty key
    /// share one error so ranking can fall through without logging the key.
    fn openrouter_for_benchmarks(&self) -> Result<&crate::providers::Upstream, String> {
        self.openrouter()
            .filter(|up| !up.bearer().is_empty())
            .ok_or_else(|| "no OPENROUTER_API_KEY configured".into())
    }

    async fn get_json(&self, url: &str, bearer: Option<&str>) -> Result<serde_json::Value, String> {
        let mut req = self.http.get(url);
        if let Some(key) = bearer.filter(|k| !k.is_empty()) {
            req = req.bearer_auth(key);
        }
        let response = req.send().await.map_err(|e| format!("transport: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }
        response
            .json()
            .await
            .map_err(|e| format!("body not JSON: {e}"))
    }

    async fn discover_one(
        &self,
        up: &crate::providers::Upstream,
    ) -> Result<Vec<FreeModel>, String> {
        let body = self
            .get_json(
                &format!("{}/models", up.base_url.trim_end_matches('/')),
                Some(up.bearer()),
            )
            .await?;
        Ok(free::parse_provider_models(
            &body, &up.id, up.catalog, up.allow,
        ))
    }
}

#[async_trait]
impl FreeModelDiscovery for DefaultSources {
    async fn discover(&self) -> Result<Vec<FreeModel>, String> {
        if self.providers.is_empty() {
            return Ok(Vec::new());
        }
        let mut merged = Vec::new();
        let mut last_err: Option<String> = None;
        let mut any_ok = false;
        for up in &self.providers {
            match self.discover_one(up).await {
                Ok(models) => {
                    any_ok = true;
                    tracing::info!(
                        provider = %up.id,
                        models = models.len(),
                        "provider catalog merged"
                    );
                    merged.extend(models);
                }
                Err(e) => {
                    tracing::warn!(
                        provider = %up.id,
                        error = %e,
                        "skipping provider during discovery"
                    );
                    last_err = Some(format!("{}: {e}", up.id));
                }
            }
        }
        if merged.is_empty() && !any_ok {
            return Err(last_err.unwrap_or_else(|| "no providers configured".into()));
        }
        merged.sort_by(|a, b| a.id.cmp(&b.id));
        merged.dedup_by(|a, b| a.id == b.id);
        Ok(merged)
    }
}

#[async_trait]
impl CodingBenchmarkSource for DefaultSources {
    async fn coding_benchmark_rows(&self) -> Result<Vec<(String, ModelScores)>, String> {
        let up = self.openrouter_for_benchmarks()?;
        let url = format!(
            "{}/benchmarks?task_type=coding&max_results=100",
            up.base_url.trim_end_matches('/')
        );
        let body = self.get_json(&url, Some(up.bearer())).await?;
        Ok(bench_api::parse_benchmarks(&body))
    }
}

#[async_trait]
impl ScrapeRankingSource for DefaultSources {
    async fn scraped_leaderboard_rows(&self) -> Vec<(String, ModelScores)> {
        let Some(spider) = &self.spider else {
            tracing::debug!("SPIDER_MCP_URL unset; scrape fallback disabled");
            return Vec::new();
        };
        for source in ScrapeSource::ALL {
            match spider
                .scrape_markdown(source.url(), self.scrape_timeout_secs)
                .await
            {
                Ok(md) => {
                    let rows = scrape_parse::parse_scraped_markdown(*source, &md);
                    if !rows.is_empty() {
                        tracing::info!(
                            source = source.label(),
                            rows = rows.len(),
                            "scrape fallback produced ranking rows"
                        );
                        return scrape_parse::rows_as_scores(&rows);
                    }
                    tracing::warn!(
                        source = source.label(),
                        "page shape unrecognised; trying next source"
                    );
                }
                Err(e) => tracing::warn!(source = source.label(), error = %e, "scrape failed"),
            }
        }
        Vec::new()
    }
}

struct CacheState {
    snapshot: Option<Resolution>,
}

/// The resolver itself. Share via `Arc`.
pub struct BestFreeModelResolver {
    discovery: Arc<dyn FreeModelDiscovery>,
    benchmarks: Arc<dyn CodingBenchmarkSource>,
    scraper: Arc<dyn ScrapeRankingSource>,
    ttl: Duration,
    state: Mutex<CacheState>,
    /// Single-flight guard around refreshes; held across awaits, hence tokio's mutex.
    refresh_lock: tokio::sync::Mutex<()>,
}

impl BestFreeModelResolver {
    /// Production wiring: one [`DefaultSources`] behind all three source traits.
    pub fn with_defaults(sources: Arc<DefaultSources>, ttl_secs: u64) -> Self {
        let discovery: Arc<dyn FreeModelDiscovery> = sources.clone();
        let benchmarks: Arc<dyn CodingBenchmarkSource> = sources.clone();
        let scraper: Arc<dyn ScrapeRankingSource> = sources;
        Self::new(
            discovery,
            benchmarks,
            scraper,
            Duration::from_secs(ttl_secs),
        )
    }

    pub fn new(
        discovery: Arc<dyn FreeModelDiscovery>,
        benchmarks: Arc<dyn CodingBenchmarkSource>,
        scraper: Arc<dyn ScrapeRankingSource>,
        ttl: Duration,
    ) -> Self {
        Self {
            discovery,
            benchmarks,
            scraper,
            ttl,
            state: Mutex::new(CacheState { snapshot: None }),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The current ordering: cached while fresh, refreshed when stale, single-flight throughout.
    ///
    /// Errors when discovery has never succeeded, or when the catalog currently
    /// has no zero-priced models (yesterday's ranking is not reused in that case).
    pub async fn current(&self) -> Result<Resolution, ResolveError> {
        if let Some(fresh) = self.fresh_snapshot() {
            return Ok(fresh);
        }
        let _guard = self.refresh_lock.lock().await;
        // Re-check: another request may have refreshed while we waited on the lock.
        if let Some(fresh) = self.fresh_snapshot() {
            return Ok(fresh);
        }
        self.refresh_locked().await
    }

    /// Ignore the cache and re-resolve now.
    pub async fn force_refresh(&self) -> Result<Resolution, ResolveError> {
        let _guard = self.refresh_lock.lock().await;
        self.refresh_locked().await
    }

    /// Refresh. Caller must hold [`refresh_lock`](Self::refresh_lock).
    async fn refresh_locked(&self) -> Result<Resolution, ResolveError> {
        match self.resolve_once().await {
            Ok(resolution) => {
                self.store(resolution.clone());
                tracing::info!(
                    origin = resolution.origin.label(),
                    models = resolution.ranked.len(),
                    best = resolution
                        .ranked
                        .first()
                        .map(|m| m.id.as_str())
                        .unwrap_or("(none)"),
                    "free-model ranking resolved"
                );
                Ok(resolution)
            }
            Err(e) => self.serve_stale_or_fail(e),
        }
    }

    /// Transport failures may reuse yesterday's ranking. An empty free catalog
    /// may not: those slugs can now be paid, and a still-fresh snapshot would
    /// keep routing them until TTL.
    fn serve_stale_or_fail(&self, error: ResolveError) -> Result<Resolution, ResolveError> {
        if matches!(error, ResolveError::NoFreeModels) {
            self.clear_snapshot();
            return Err(error);
        }
        if let Some(stale) = self.snapshot_any_age() {
            tracing::warn!(
                error = %error,
                age_secs = stale.resolved_at.elapsed().as_secs(),
                "refresh failed; serving stale ranking"
            );
            return Ok(stale);
        }
        Err(error)
    }

    async fn resolve_once(&self) -> Result<Resolution, ResolveError> {
        let free_models = self
            .discovery
            .discover()
            .await
            .map_err(ResolveError::Discovery)?;
        if free_models.is_empty() {
            return Err(ResolveError::NoFreeModels);
        }

        let mut table = RankTable::default();
        let mut api_matched = 0usize;
        match self.benchmarks.coding_benchmark_rows().await {
            Ok(rows) => {
                for (slug, scores) in rows {
                    if let Some(id) = match_slug::free_id_for_api_slug(&slug, &free_models) {
                        table.record(id, scores);
                        api_matched += 1;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "benchmarks API unavailable; falling back to scrapes");
            }
        };

        let mut scraped_matched = 0usize;
        for (name, scores) in self.scraper.scraped_leaderboard_rows().await {
            let Some(slug) = match_slug::best_slug_for(&name, &free_models) else {
                tracing::debug!(leader_name = %name, "no unambiguous free-slug match; skipped");
                continue;
            };
            // API precedence: a slug the API already scored with real coding data does not lose
            // that field to a scraped percentage.
            if table
                .get(&slug)
                .is_some_and(|s| s.coding_index.is_some() || s.design_arena_elo.is_some())
            {
                continue;
            }
            table.record(slug, scores);
            scraped_matched += 1;
        }

        let origin = if api_matched > 0 {
            Origin::BenchmarksApi
        } else if scraped_matched > 0 {
            Origin::Scraped
        } else {
            Origin::Heuristic
        };
        let ordered_ids = rank::order_free_models(&free_models, &table);
        let by_id: std::collections::HashMap<&str, &FreeModel> =
            free_models.iter().map(|m| (m.id.as_str(), m)).collect();
        let ranked = ordered_ids
            .into_iter()
            .filter_map(|id| by_id.get(id.as_str()).map(|m| (*m).clone()))
            .collect();
        Ok(Resolution {
            ranked,
            origin,
            resolved_at: Instant::now(),
        })
    }

    fn fresh_snapshot(&self) -> Option<Resolution> {
        let state = self.state.lock().expect("cache lock");
        // `<` vs `<=` here differs only at elapsed == TTL exactly — an unreachable instant
        // without an injectable clock, and not worth one for a cache boundary.
        state
            .snapshot
            .clone()
            .filter(|s| s.resolved_at.elapsed() < self.ttl)
    }

    fn snapshot_any_age(&self) -> Option<Resolution> {
        let state = self.state.lock().expect("cache lock");
        state.snapshot.clone()
    }

    fn store(&self, resolution: Resolution) {
        let mut state = self.state.lock().expect("cache lock");
        state.snapshot = Some(resolution);
    }

    fn clear_snapshot(&self) {
        self.state.lock().expect("cache lock").snapshot = None;
    }
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod tests;
