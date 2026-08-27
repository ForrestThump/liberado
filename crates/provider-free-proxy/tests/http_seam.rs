//! Wire-seam tests for the proxy's HTTP surface.
//!
//! The unit tests prove the pieces; these prove the assembled router speaks the OpenAI
//! contract end to end against a real HTTP upstream — model rewriting reaching the wire,
//! free-only refusals surfacing as 400s, and failover walking the ranking on quota refusals
//! but not on payload errors.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use liberado_provider_free_proxy::free::FreeModel;
use liberado_provider_free_proxy::providers::{BillingKind, Upstream, UpstreamRegistry};
use liberado_provider_free_proxy::quota::QuotaBudget;
use liberado_provider_free_proxy::rank::ModelScores;
use liberado_provider_free_proxy::resolver::{
    BestFreeModelResolver, CodingBenchmarkSource, DefaultSources, FreeModelDiscovery,
    ScrapeRankingSource,
};
use liberado_provider_free_proxy::service::{ProxyConfig, ProxyService};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct FixedDiscovery(Vec<FreeModel>);
#[async_trait::async_trait]
impl FreeModelDiscovery for FixedDiscovery {
    async fn discover(&self) -> Result<Vec<FreeModel>, String> {
        Ok(self.0.clone())
    }
}

struct FailingBenchmarks;
#[async_trait::async_trait]
impl CodingBenchmarkSource for FailingBenchmarks {
    async fn coding_benchmark_rows(&self) -> Result<Vec<(String, ModelScores)>, String> {
        Err("unavailable".into())
    }
}

struct EmptyScrapes;
#[async_trait::async_trait]
impl ScrapeRankingSource for EmptyScrapes {
    async fn scraped_leaderboard_rows(&self) -> Vec<(String, ModelScores)> {
        Vec::new()
    }
}

fn fm(id: &str, ctx: u64) -> FreeModel {
    FreeModel::fixture(id, ctx, true)
}

fn service_at(upstream_base: String) -> Arc<ProxyService> {
    service_with(
        vec![fm("best/m", 100_000), fm("second/m", 64_000)],
        ProxyConfig::single(upstream_base, "sk-upstream", 3),
    )
}

fn service_with(models: Vec<FreeModel>, config: ProxyConfig) -> Arc<ProxyService> {
    let resolver = Arc::new(BestFreeModelResolver::new(
        Arc::new(FixedDiscovery(models)),
        Arc::new(FailingBenchmarks),
        Arc::new(EmptyScrapes),
        Duration::from_secs(3600),
    ));
    Arc::new(ProxyService::new(resolver, config))
}

fn ok_completion() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "resp-1",
        "choices": [{ "message": { "role": "assistant", "content": "ok" } }]
    }))
}

async fn post_chat(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn auto_requests_reach_the_upstream_as_the_best_free_model() {
    let upstream = MockServer::start().await;
    let sent = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let sink = sent.clone();
    struct Capture(Arc<std::sync::Mutex<Vec<Value>>>);
    impl wiremock::Respond for Capture {
        fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&request.body).expect("json body");
            self.0.lock().unwrap().push(body);
            ok_completion()
        }
    }
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(Capture(sink))
        .expect(1)
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    let (status, reply) = post_chat(
        app,
        json!({"model": "auto", "messages": [{"role":"user","content":"hi"}]}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
    let bodies = sent.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0]["model"],
        json!("best/m"),
        "auto must resolve to the ranked-best slug"
    );
    assert!(
        bodies[0]["messages"].is_array(),
        "everything except `model` must survive the rewrite"
    );
}

#[tokio::test]
async fn the_upstream_bearer_key_is_forwarded() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer sk-upstream",
        ))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    let (status, _) = post_chat(app, json!({"messages": []})).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn naming_a_paid_model_is_refused_without_touching_the_upstream() {
    let upstream = MockServer::start().await;
    // No mocks mounted: any upstream hit would hang/fail the expect(0) verification below.
    Mock::given(method("POST"))
        .respond_with(ok_completion())
        .expect(0)
        .mount(&upstream)
        .await;

    let app = liberado_provider_free_proxy::http_router(service_at(upstream.uri()));
    let (status, err) = post_chat(app, json!({"model": "openai/gpt-4o", "messages": []})).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = err["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("openai/gpt-4o"), "{message}");
    assert!(
        message.contains("best/m"),
        "alternatives must be named: {message}"
    );
}

#[tokio::test]
async fn models_lists_only_free_models_ranked_first() {
    let app = liberado_provider_free_proxy::http_router(service_at("http://unused.invalid".into()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("answer");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let ids: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["best/m", "second/m"]);
}

#[tokio::test]
async fn quota_refusals_fail_over_to_the_next_ranked_model() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "best/m" })))
        .respond_with(ResponseTemplate::new(429).set_body_string(
            json!({"error":{"message":"Rate limit exceeded for best/m today"}}).to_string(),
        ))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "second/m" })))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    let (status, reply) = post_chat(app, json!({"model": "auto", "messages": []})).await;
    assert_eq!(status, StatusCode::OK, "second candidate must serve");
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
}

/// A 400 whose body names a model/quota problem must fail over. Emptying
/// `failure_body_text` would classify this as a payload 400 and relay it.
#[tokio::test]
async fn a_400_naming_rate_limit_fails_over_to_the_next_model() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "best/m" })))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            json!({"error":{"message":"Rate limit exceeded for this model"}}).to_string(),
        ))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "second/m" })))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    let (status, reply) = post_chat(app, json!({"model": "auto", "messages": []})).await;
    assert_eq!(status, StatusCode::OK, "second candidate must serve");
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
}

/// The 64 KiB error-body cap must still admit an 8 KiB rate-limit payload.
/// `64 * 1024` mutated to `64 + 1024` or `64 / 1024` would refuse this body,
/// drop the text, and treat the 400 as a payload error.
#[tokio::test]
async fn an_8kib_rate_limit_400_still_fails_over() {
    let upstream = MockServer::start().await;
    let mut body = json!({"error":{"message":"Rate limit exceeded for this model"}}).to_string();
    body.push_str(&"x".repeat(8 * 1024));
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "best/m" })))
        .respond_with(ResponseTemplate::new(400).set_body_string(body))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "second/m" })))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    let (status, reply) = post_chat(app, json!({"model": "auto", "messages": []})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "readable error text must still fail over"
    );
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
}

#[tokio::test]
async fn a_payload_400_is_relayed_without_spending_another_candidate() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "best/m" })))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            json!({"error":{"message":"'messages' must be an array"}}).to_string(),
        ))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "second/m" })))
        .respond_with(ok_completion())
        .expect(0) // must never be reached: our payload, not the candidate, was wrong
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    let (status, _) = post_chat(app, json!({"model": "auto", "messages": "not-an-array"})).await;
    assert_eq!(status.as_u16(), 400);
}

#[tokio::test]
async fn exhausted_candidates_answer_502() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    let (status, err) = post_chat(app, json!({"messages": []})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let message = err["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("all 2 attempted"),
        "the attempt counter must count candidates, not stay at 0*1: {message}"
    );
}

/// A non-object body previously reached serde_json's `IndexMut`, which panics on
/// arrays/strings/numbers — killing the connection with no HTTP response, remotely triggerable.
/// The boundary must refuse it politely instead.
#[tokio::test]
async fn non_object_bodies_get_a_400_not_a_dropped_connection() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ok_completion())
        .expect(0) // a malformed body must never reach any model
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    for bad in [json!([1, 2, 3]), json!("a string"), json!(42), json!(null)] {
        let (status, err) = post_chat(app.clone(), bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body {bad}");
        let message = err["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("JSON object"), "{message}");
    }
}

/// An unreadable (oversized) upstream error body must not wedge classification: the 429's
/// status alone still fails over, and the next candidate serves.
#[tokio::test]
async fn an_oversized_error_body_still_fails_over_on_status() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "best/m" })))
        .respond_with(ResponseTemplate::new(429).set_body_string("x".repeat(80 * 1024)))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "second/m" })))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    let (status, reply) = post_chat(app, json!({"model": "auto", "messages": []})).await;
    assert_eq!(status, StatusCode::OK, "the second candidate must serve");
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
}

#[tokio::test]
async fn healthz_answers_ok() {
    let app = liberado_provider_free_proxy::http_router(service_at("http://unused.invalid".into()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("answer");
    assert_eq!(response.status(), StatusCode::OK);
    // The exact body matters: process supervisors and scripts grep it.
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"ok");
}

/// The full "which endpoint do I get?" flow over real HTTP, with the models themselves left
/// untouched: discovery and ranking run against mock OpenRouter endpoints, `GET /v1/models`
/// hands the ranked answer back as data, and routing confirms what a request *would* target —
/// while the chat-completions route stays pinned at zero hits. A caller can integrate against
/// this proxy end to end without ever spending a token.
#[tokio::test]
async fn querying_models_hands_back_the_best_endpoint_without_any_inference() {
    let upstream = MockServer::start().await;
    let base = format!("{}/api/v1", upstream.uri());

    // Real discovery + ranking sources, pointed at the mocks.
    Mock::given(method("GET"))
        .and(path("/api/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "id": "vendor/small", "context_length": 100_000,
                  "pricing": { "prompt": "0", "completion": "0" },
                  "supported_parameters": ["tools"] },
                { "id": "vendor/large", "context_length": 500_000,
                  "pricing": { "prompt": "0", "completion": "0" },
                  "supported_parameters": ["tools"] },
                { "id": "vendor/paid", "context_length": 900_000,
                  "pricing": { "prompt": "0.000001", "completion": "0.000002" },
                  "supported_parameters": ["tools"] }
            ]
        })))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/benchmarks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "source": "artificial-analysis",
                  "model_permaslug": "vendor/small", "coding_index": 82.5 }
            ]
        })))
        .mount(&upstream)
        .await;
    // The assertion that matters: no model is ever invoked by this flow.
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ok_completion())
        .expect(0)
        .mount(&upstream)
        .await;

    let sources = Arc::new(DefaultSources::new(
        base.clone(),
        Some("sk-up".into()),
        None,
    ));
    let resolver = Arc::new(BestFreeModelResolver::with_defaults(sources, 3600));
    let state = Arc::new(ProxyService::new(
        resolver,
        ProxyConfig::single(base.clone(), "sk-up", 3),
    ));

    // 1. Ask the proxy which endpoints exist — the answer is the ranking itself, paid model
    //    excluded, benchmark winner first (overriding the context-size heuristic).
    let app = liberado_provider_free_proxy::http_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("answer");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let catalog: Value = serde_json::from_slice(&bytes).unwrap();
    let ids: Vec<&str> = catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["openrouter/vendor/small", "openrouter/vendor/large"]
    );

    // 2. The hand-off: naming the winner resolves to exactly that candidate, still without a
    //    completion call — callers learn their target before spending anything.
    let chosen = ids[0].to_string();
    let candidates = state
        .candidates_for(&json!({ "model": chosen, "messages": [] }))
        .await
        .expect("winner is servable");
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.public_id.as_str())
            .collect::<Vec<_>>(),
        vec!["openrouter/vendor/small"]
    );
}

#[tokio::test]
async fn a_403_fails_over_to_the_next_ranked_model() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "best/m" })))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "second/m" })))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    let (status, reply) = post_chat(app, json!({"model": "auto", "messages": []})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "second candidate must serve after 403"
    );
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
}

#[tokio::test]
async fn a_503_fails_over_to_the_next_ranked_model() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "best/m" })))
        .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "second/m" })))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&upstream)
        .await;

    let app =
        liberado_provider_free_proxy::http_router(service_at(format!("{}/api/v1", upstream.uri())));
    let (status, reply) = post_chat(app, json!({"model": "auto", "messages": []})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "second candidate must serve after 503"
    );
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
}

#[tokio::test]
async fn a_timeout_on_candidate_one_serves_candidate_two() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "best/m" })))
        .respond_with(ok_completion().set_delay(Duration::from_secs(2)))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(body_partial_json(json!({ "model": "second/m" })))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&upstream)
        .await;

    let mut config = ProxyConfig::single(format!("{}/api/v1", upstream.uri()), "sk-upstream", 3);
    config.attempt_timeout = Duration::from_millis(200);
    let app = liberado_provider_free_proxy::http_router(service_with(
        vec![fm("best/m", 100_000), fm("second/m", 64_000)],
        config,
    ));
    let (status, reply) = post_chat(app, json!({"model": "auto", "messages": []})).await;
    assert_eq!(status, StatusCode::OK, "timeout must fail over");
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
}

#[tokio::test]
async fn transport_failure_on_candidate_one_serves_candidate_two() {
    let good = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&good)
        .await;

    let first = fm("best/m", 100_000).with_provider("down");
    let second = fm("second/m", 64_000).with_provider("up");
    let mut config = ProxyConfig {
        max_attempts: 3,
        attempt_timeout: Duration::from_millis(200),
        registry: UpstreamRegistry::from_upstreams([
            Upstream::new_for_tests(
                "down",
                "http://127.0.0.1:9/v1",
                "sk-down",
                BillingKind::RateLimitedFree,
            ),
            Upstream::new_for_tests(
                "up",
                format!("{}/v1", good.uri()),
                "sk-up",
                BillingKind::RateLimitedFree,
            ),
        ]),
    };
    let _ = &mut config;
    let app = liberado_provider_free_proxy::http_router(service_with(vec![first, second], config));
    let (status, reply) = post_chat(app, json!({"model": "auto", "messages": []})).await;
    assert_eq!(status, StatusCode::OK, "transport must fail over");
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
}

#[tokio::test]
async fn quota_then_pay_is_skipped_before_a_billable_call() {
    let billed = MockServer::start().await;
    let free = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok_completion())
        .expect(0)
        .mount(&billed)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&free)
        .await;

    let mut billed_up = Upstream::new_for_tests(
        "billed",
        format!("{}/v1", billed.uri()),
        "sk-billed",
        BillingKind::QuotaThenPay,
    );
    billed_up.quota = QuotaBudget::remaining(0);
    let first = fm("best/m", 100_000).with_provider("billed");
    let second = fm("second/m", 64_000).with_provider("free");
    let config = ProxyConfig {
        max_attempts: 3,
        attempt_timeout: Duration::from_secs(5),
        registry: UpstreamRegistry::from_upstreams([
            billed_up,
            Upstream::new_for_tests(
                "free",
                format!("{}/v1", free.uri()),
                "sk-free",
                BillingKind::RateLimitedFree,
            ),
        ]),
    };
    let app = liberado_provider_free_proxy::http_router(service_with(vec![first, second], config));
    let (status, reply) = post_chat(app, json!({"model": "auto", "messages": []})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "quota-then-pay must be skipped, not billed"
    );
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
}

#[tokio::test]
async fn walking_the_ranking_can_change_provider() {
    let groq = MockServer::start().await;
    let nvidia = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/chat/completions"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer gsk-test",
        ))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limit"))
        .expect(1)
        .mount(&groq)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer nvapi-test",
        ))
        .respond_with(ok_completion())
        .expect(1)
        .mount(&nvidia)
        .await;

    // Larger context ranks first on the heuristic floor, so Groq is tried before NVIDIA.
    let first = fm("llama-fast", 200_000).with_provider("groq");
    let second = fm("nemotron", 32_000).with_provider("nvidia");
    let config = ProxyConfig {
        max_attempts: 3,
        attempt_timeout: Duration::from_secs(5),
        registry: UpstreamRegistry::from_upstreams([
            Upstream::new_for_tests(
                "groq",
                format!("{}/openai/v1", groq.uri()),
                "gsk-test",
                BillingKind::RateLimitedFree,
            ),
            Upstream::new_for_tests(
                "nvidia",
                format!("{}/v1", nvidia.uri()),
                "nvapi-test",
                BillingKind::RateLimitedFree,
            ),
        ]),
    };
    let app = liberado_provider_free_proxy::http_router(service_with(vec![first, second], config));
    let (status, reply) = post_chat(app, json!({"model": "auto", "messages": []})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["choices"][0]["message"]["content"], json!("ok"));
}
