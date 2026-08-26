use super::*;
use crate::rank::ModelScores;
use crate::scrape_parse::ScrapeSource;
use crate::spider::SpiderClient;

struct FixedDiscovery(Vec<FreeModel>);
#[async_trait]
impl FreeModelDiscovery for FixedDiscovery {
    async fn discover(&self) -> Result<Vec<FreeModel>, String> {
        Ok(self.0.clone())
    }
}

struct FailingDiscovery;
#[async_trait]
impl FreeModelDiscovery for FailingDiscovery {
    async fn discover(&self) -> Result<Vec<FreeModel>, String> {
        Err("network down".into())
    }
}

struct ApiBenchmarks(Vec<(String, ModelScores)>);
#[async_trait]
impl CodingBenchmarkSource for ApiBenchmarks {
    async fn coding_benchmark_rows(&self) -> Result<Vec<(String, ModelScores)>, String> {
        Ok(self.0.clone())
    }
}

struct FailingBenchmarks;
#[async_trait]
impl CodingBenchmarkSource for FailingBenchmarks {
    async fn coding_benchmark_rows(&self) -> Result<Vec<(String, ModelScores)>, String> {
        Err("HTTP 401".into())
    }
}

struct FixedScrapes(Vec<(String, ModelScores)>);
#[async_trait]
impl ScrapeRankingSource for FixedScrapes {
    async fn scraped_leaderboard_rows(&self) -> Vec<(String, ModelScores)> {
        self.0.clone()
    }
}

fn fm(id: &str, ctx: u64, tools: bool) -> FreeModel {
    FreeModel {
        id: id.into(),
        context_length: ctx,
        supports_tools: tools,
    }
}

fn resolver(
    discovery: Arc<dyn FreeModelDiscovery>,
    benchmarks: Arc<dyn CodingBenchmarkSource>,
    scraper: Arc<dyn ScrapeRankingSource>,
) -> BestFreeModelResolver {
    BestFreeModelResolver::new(discovery, benchmarks, scraper, Duration::from_secs(3600))
}

#[tokio::test]
async fn api_ranked_order_wins_when_the_api_answers() {
    let r = resolver(
        Arc::new(FixedDiscovery(vec![fm("a/m", 0, true), fm("b/m", 0, true)])),
        Arc::new(ApiBenchmarks(vec![(
            "b/m".into(),
            ModelScores {
                coding_index: Some(80.0),
                ..Default::default()
            },
        )])),
        Arc::new(FixedScrapes(vec![])),
    );
    let res = r.current().await.expect("resolve");
    assert_eq!(res.origin, Origin::BenchmarksApi);
    assert_eq!(res.ranked_ids(), vec!["b/m", "a/m"]);
}

#[tokio::test]
async fn scrape_fallback_ranks_when_the_api_fails() {
    let r = resolver(
        Arc::new(FixedDiscovery(vec![
            fm("deepseek/deepseek-r1:free", 0, true),
            fm("z-ai/glm-5.2:free", 0, true),
        ])),
        Arc::new(FailingBenchmarks),
        Arc::new(FixedScrapes(vec![(
            "DeepSeek R1".into(),
            ModelScores {
                scraped_percent: Some(64.0),
                ..Default::default()
            },
        )])),
    );
    let res = r.current().await.expect("resolve");
    assert_eq!(res.origin, Origin::Scraped);
    assert_eq!(res.ranked_ids()[0], "deepseek/deepseek-r1:free");
}

#[tokio::test]
async fn heuristic_order_when_nothing_ranks() {
    let r = resolver(
        Arc::new(FixedDiscovery(vec![
            fm("a/no-tools", 8_000, false),
            fm("b/tools", 128_000, true),
        ])),
        Arc::new(FailingBenchmarks),
        Arc::new(FixedScrapes(vec![])),
    );
    let res = r.current().await.expect("resolve");
    assert_eq!(res.origin, Origin::Heuristic);
    assert_eq!(res.ranked_ids(), vec!["b/tools", "a/no-tools"]);
}

#[tokio::test]
async fn a_fresh_snapshot_is_reused_without_refetching() {
    struct CountingDiscovery(Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait]
    impl FreeModelDiscovery for CountingDiscovery {
        async fn discover(&self) -> Result<Vec<FreeModel>, String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![fm("a/m", 0, true)])
        }
    }
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let r = resolver(
        Arc::new(CountingDiscovery(counter.clone())),
        Arc::new(FailingBenchmarks),
        Arc::new(FixedScrapes(vec![])),
    );
    r.current().await.unwrap();
    r.current().await.unwrap();
    r.current().await.unwrap();
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_refresh_serves_stale_snapshot() {
    struct FlipFlop {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl FreeModelDiscovery for FlipFlop {
        async fn discover(&self) -> Result<Vec<FreeModel>, String> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Ok(vec![fm("a/m", 0, true)])
            } else {
                Err("upstream exploded".into())
            }
        }
    }
    let flip = Arc::new(FlipFlop {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let r = resolver(
        flip,
        Arc::new(FailingBenchmarks),
        Arc::new(FixedScrapes(vec![])),
    );

    let first = r.current().await.expect("first resolve succeeds");
    // Force staleness without sleeping out the TTL: rewrite the timestamp through a refresh
    // cycle is not possible from here, so instead verify via force_refresh hitting the
    // failing path and still returning the stored ordering.
    let second = r
        .force_refresh()
        .await
        .expect("stale served on refresh failure");
    assert_eq!(second.ranked_ids(), first.ranked_ids());
}

#[tokio::test]
async fn discovery_failure_with_no_snapshot_is_fatal() {
    let r = resolver(
        Arc::new(FailingDiscovery),
        Arc::new(FailingBenchmarks),
        Arc::new(FixedScrapes(vec![])),
    );
    assert!(matches!(r.current().await, Err(ResolveError::Discovery(_))));
}

#[tokio::test]
async fn an_empty_free_list_is_an_explicit_error() {
    let r = resolver(
        Arc::new(FixedDiscovery(vec![])),
        Arc::new(ApiBenchmarks(vec![])),
        Arc::new(FixedScrapes(vec![])),
    );
    assert!(matches!(r.current().await, Err(ResolveError::NoFreeModels)));
}

#[tokio::test]
async fn api_permaslug_joins_the_free_variant_and_ranks_it_first() {
    let r = resolver(
        Arc::new(FixedDiscovery(vec![
            fm("z-ai/glm-5.2:free", 32_000, true),
            fm("other/huge", 1_000_000, true),
        ])),
        Arc::new(ApiBenchmarks(vec![(
            "z-ai/glm-5.2".into(),
            ModelScores {
                coding_index: Some(71.4),
                ..Default::default()
            },
        )])),
        Arc::new(FixedScrapes(vec![])),
    );
    let res = r.current().await.expect("resolve");
    assert_eq!(res.origin, Origin::BenchmarksApi);
    assert_eq!(res.ranked_ids()[0], "z-ai/glm-5.2:free");
}

#[tokio::test]
async fn api_rows_that_miss_the_free_set_do_not_claim_the_api_origin() {
    let r = resolver(
        Arc::new(FixedDiscovery(vec![fm("z-ai/glm-5.2:free", 128_000, true)])),
        Arc::new(ApiBenchmarks(vec![(
            "unrelated/model".into(),
            ModelScores {
                coding_index: Some(99.0),
                ..Default::default()
            },
        )])),
        Arc::new(FixedScrapes(vec![])),
    );
    let res = r.current().await.expect("resolve");
    assert_eq!(res.origin, Origin::Heuristic);
    assert_eq!(res.ranked_ids(), vec!["z-ai/glm-5.2:free"]);
}

#[tokio::test]
async fn empty_catalog_after_a_success_does_not_serve_stale() {
    struct FlipToEmpty {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl FreeModelDiscovery for FlipToEmpty {
        async fn discover(&self) -> Result<Vec<FreeModel>, String> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Ok(vec![fm("a/m:free", 0, true)])
            } else {
                Ok(vec![])
            }
        }
    }
    let r = resolver(
        Arc::new(FlipToEmpty {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }),
        Arc::new(FailingBenchmarks),
        Arc::new(FixedScrapes(vec![])),
    );
    r.current().await.expect("first resolve succeeds");
    assert!(matches!(
        r.force_refresh().await,
        Err(ResolveError::NoFreeModels)
    ));
    assert!(
        matches!(r.current().await, Err(ResolveError::NoFreeModels)),
        "cleared snapshot must not keep routing yesterday's slugs"
    );
}

#[tokio::test]
async fn api_scores_are_not_overwritten_by_scraped_rows_for_the_same_model() {
    let r = resolver(
        Arc::new(FixedDiscovery(vec![fm(
            "deepseek/deepseek-r1:free",
            0,
            true,
        )])),
        Arc::new(ApiBenchmarks(vec![(
            "deepseek/deepseek-r1:free".into(),
            ModelScores {
                coding_index: Some(45.0),
                ..Default::default()
            },
        )])),
        Arc::new(FixedScrapes(vec![(
            "DeepSeek R1".into(),
            ModelScores {
                scraped_percent: Some(99.9),
                ..Default::default()
            },
        )])),
    );
    let res = r.current().await.expect("resolve");
    assert_eq!(res.origin, Origin::BenchmarksApi);
    assert_eq!(res.ranked_ids(), vec!["deepseek/deepseek-r1:free"]);
}

#[test]
fn resolution_helpers_report_contents() {
    let res = Resolution {
        ranked: vec![fm("a/m", 0, true)],
        origin: Origin::Heuristic,
        resolved_at: Instant::now(),
    };
    assert!(res.contains("a/m"));
    assert!(!res.contains("b/m"));
    assert_eq!(res.ranked_ids(), vec!["a/m"]);
    assert_eq!(Origin::Scraped.label(), "scraped-leaderboards");
}

/// Wire-level tests for [`DefaultSources`] itself — the trait fixtures above prove the
/// resolver's logic; these prove the production sources speak the real endpoints.
mod default_sources_wire {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn free_model_json(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "context_length": 262144,
            "pricing": { "prompt": "0", "completion": "0" },
            "supported_parameters": ["tools", "temperature"]
        })
    }

    #[tokio::test]
    async fn discover_parses_the_public_models_listing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [free_model_json("z-ai/glm-5.2:free")]
            })))
            .mount(&server)
            .await;
        let src = DefaultSources::new(format!("{}/api/v1", server.uri()), None, None);
        let models = src.discover().await.expect("discover succeeds");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "z-ai/glm-5.2:free");
        assert!(models[0].supports_tools);
    }

    /// Kills the inverted/deleted status check in `get_json`: an error status must be an
    /// error, not a silently-empty listing.
    #[tokio::test]
    async fn discover_reports_error_statuses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/models"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let src = DefaultSources::new(format!("{}/api/v1", server.uri()), None, None);
        let err = src.discover().await.expect_err("must fail");
        assert!(err.contains("503"), "{err}");
    }

    #[tokio::test]
    async fn benchmarks_rows_are_parsed_from_the_authenticated_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/benchmarks"))
            .and(query_param("task_type", "coding"))
            .and(query_param("max_results", "100"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer sk-live",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "source": "artificial-analysis",
                           "model_permaslug": "z-ai/glm-5.2", "coding_index": 71.4 }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let src = DefaultSources::new(
            format!("{}/api/v1", server.uri()),
            Some("sk-live".into()),
            None,
        );
        let rows = src.coding_benchmark_rows().await.expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "z-ai/glm-5.2");
        assert_eq!(rows[0].1.coding_index, Some(71.4));
    }

    #[tokio::test]
    async fn benchmarks_without_a_key_is_an_error_not_an_empty_list() {
        let src = DefaultSources::new("http://unused.invalid/api/v1".to_string(), None, None);
        let err = src.coding_benchmark_rows().await.expect_err("must fail");
        assert!(err.contains("OPENROUTER_API_KEY"), "{err}");
    }

    /// Kills the deleted status check on the benchmarks call specifically (the happy-path
    /// mock above pins success; this pins that failure stays failure).
    #[tokio::test]
    async fn benchmarks_error_status_stays_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/benchmarks"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let src = DefaultSources::new(
            format!("{}/api/v1", server.uri()),
            Some("sk-wrong".into()),
            None,
        );
        let err = src.coding_benchmark_rows().await.expect_err("must fail");
        assert!(err.contains("401"), "{err}");
    }

    #[tokio::test]
    async fn scrapes_are_disabled_without_a_spider_client() {
        let src = DefaultSources::new("http://unused.invalid".to_string(), None, None);
        assert!(src.scraped_leaderboard_rows().await.is_empty());
    }

    /// An unrecognised first page must fall through to the next source rather than end the
    /// search — kills both the inverted emptiness check and any mutant returning a fixed
    /// row list.
    #[tokio::test]
    async fn scrapes_fall_through_to_the_next_source_on_an_unrecognised_page() {
        let spider_server = MockServer::start().await;
        let openrouter_md = ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "data": { "markdown": "totally unrecognisable page shape" }
        }));
        let aider_md = ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "data": { "markdown": "| Model   | Percent correct |\n\
                                   |o3-r192  | 81.2%           |\n" }
        }));
        Mock::given(method("POST"))
            .and(path("/v1/scrape"))
            .and(body_partial_json(serde_json::json!(
                {"url": ScrapeSource::OpenRouterBenchmarks.url()}
            )))
            .respond_with(openrouter_md)
            .mount(&spider_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/scrape"))
            .and(body_partial_json(serde_json::json!(
                {"url": ScrapeSource::AiderLeaderboard.url()}
            )))
            .respond_with(aider_md)
            .mount(&spider_server)
            .await;

        let src = DefaultSources::new(
            "http://unused.invalid".to_string(),
            None,
            Some(SpiderClient::new(spider_server.uri(), None)),
        );
        let rows = src.scraped_leaderboard_rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "o3-r192");
        assert_eq!(rows[0].1.scraped_percent, Some(81.2));
    }

    /// The integration contract in miniature: stand up mock OpenRouter endpoints, ask the
    /// resolver for its answer, receive the best-ranked free model as data — and prove the
    /// inference endpoint was never touched. Discovery and ranking are read-only acts; the
    /// chat-completions mock is mounted with `expect(0)`, so a single stray completion call
    /// fails this test when the mock verifies on drop.
    #[tokio::test]
    async fn resolution_hands_back_the_best_model_without_invoking_it() {
        let server = MockServer::start().await;
        let base = format!("{}/api/v1", server.uri());

        // Two tool-capable free models; the bigger-context one wins on *heuristics*.
        Mock::given(method("GET"))
            .and(path("/api/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    { "id": "vendor/small", "context_length": 100_000,
                      "pricing": { "prompt": "0", "completion": "0" },
                      "supported_parameters": ["tools"] },
                    { "id": "vendor/large", "context_length": 500_000,
                      "pricing": { "prompt": "0", "completion": "0" },
                      "supported_parameters": ["tools"] }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        // …but the coding-benchmark score hands the win to the small one.
        Mock::given(method("GET"))
            .and(path("/api/v1/benchmarks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    { "source": "artificial-analysis",
                      "model_permaslug": "vendor/small", "coding_index": 82.5 }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        // The point of the test: this must stay at zero hits forever.
        Mock::given(method("POST"))
            .and(path("/api/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let resolver = BestFreeModelResolver::with_defaults(
            Arc::new(DefaultSources::new(
                base.clone(),
                Some("sk-test".into()),
                None,
            )),
            3600,
        );
        let resolution = resolver.current().await.expect("resolves");

        // Ranking came from the API, overrode the context-size heuristic…
        assert_eq!(resolution.origin, Origin::BenchmarksApi);
        assert_eq!(
            resolution.ranked_ids(),
            vec!["vendor/small", "vendor/large"]
        );
        // …and the answer is usable as data: who won, and that it is servable.
        assert_eq!(
            resolution.ranked.first().map(|m| m.id.as_str()),
            Some("vendor/small")
        );
        assert!(resolution.contains("vendor/small"));

        // No completion request left the process: `expect(0)` verifies on drop.
    }
}

/// Kills the `||` → `&&` in the API-precedence guard: a slug holding *either* API field must
/// keep its standing without a scraped percentage, and because every stronger key is tied
/// here, the difference is visible in the final order alone.
///
/// Both models carry the same Design Arena elo from the API; the scrape offers one of them
/// a leaderboard percent. Under the correct logic the merge is refused and the larger
/// context window wins; under the mutation the scraped field lands and flips the order.
#[tokio::test]
async fn api_scored_slugs_keep_their_order_against_scraped_offers() {
    struct DaOnly(Vec<(String, ModelScores)>);
    #[async_trait]
    impl CodingBenchmarkSource for DaOnly {
        async fn coding_benchmark_rows(&self) -> Result<Vec<(String, ModelScores)>, String> {
            Ok(self
                .0
                .iter()
                .map(|(slug, _)| {
                    (
                        slug.clone(),
                        ModelScores {
                            design_arena_elo: Some(1200.0),
                            ..Default::default()
                        },
                    )
                })
                .collect())
        }
    }

    let r = BestFreeModelResolver::new(
        Arc::new(FixedDiscovery(vec![
            fm("v/zeta-model", 100_000, true),
            fm("w/yonder-model", 200_000, true),
        ])),
        Arc::new(DaOnly(vec![
            ("v/zeta-model".into(), ModelScores::default()),
            ("w/yonder-model".into(), ModelScores::default()),
        ])),
        Arc::new(FixedScrapes(vec![(
            "Zeta Model".into(),
            ModelScores {
                scraped_percent: Some(80.0),
                ..Default::default()
            },
        )])),
        std::time::Duration::from_secs(3600),
    );
    let res = r.current().await.expect("resolve");
    assert_eq!(res.origin, Origin::BenchmarksApi);
    // Equal elo, equal everything else observable → context breaks the tie, NOT the offer.
    assert_eq!(res.ranked_ids(), vec!["w/yonder-model", "v/zeta-model"]);
}
