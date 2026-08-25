//! The routing policy, separated from HTTP plumbing so it can be exercised without axum.
//!
//! [`ProxyService`] turns an incoming OpenAI-shaped chat-completions body into an upstream call
//! against exactly one of the ranked free models:
//!
//! - `model` absent / `"auto"` / `""` → best-ranked free model;
//! - `model` naming a slug in the current free set → honoured as-is (an explicit choice *inside*
//!   the mandate);
//! - anything else → refused with 400 and the nearest ranked alternatives. Silently remapping a
//!   named model would hide a paid-model intent; refusing says it out loud.
//!
//! Failover walks down the ranking when upstream refuses a candidate for reasons that are about
//! *the candidate* — rate limits, quota, unknown/no-endpoint models. A payload problem (400 with
//! an unrecognized shape) must NOT trigger failover: retrying a broken request on five models
//! just spends five free quotas instead of one.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::resolver::{BestFreeModelResolver, Resolution};

/// Upstream error statuses that say "this model specifically cannot serve you right now" —
/// worth spending another candidate on.
const FAILOVER_STATUSES: [u16; 4] = [402, 404, 408, 429];

/// Phrases in an upstream error body that mean the *model* is unusable (rather than our payload
/// being wrong). Matched case-insensitively on the raw text.
const MODEL_ERROR_MARKERS: &[&str] = &[
    "no endpoints found",
    "no allowed providers",
    "not a valid model",
    "no auth provider found",
    "rate limit exceeded",
];

#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    /// The caller named a model outside the current free set.
    #[error(
        "requested model {requested:?} is not a currently-free model; ranked free options: {alternatives}"
    )]
    NotFree {
        requested: String,
        alternatives: String,
    },
    /// No resolution exists yet (first discovery failed).
    #[error(transparent)]
    Resolve(#[from] crate::resolver::ResolveError),
}

/// One attempt's verdict: done (response ready to relay) or try the next candidate.
pub enum AttemptOutcome {
    Ready(reqwest::Response),
    Failed(String),
}

#[derive(Clone)]
pub struct ProxyService {
    pub resolver: Arc<BestFreeModelResolver>,
    pub http: reqwest::Client,
    pub config: ProxyConfig,
}

/// Static wiring for the proxy.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// OpenAI-compatible base URL of the upstream (…/api/v1).
    pub upstream_base: String,
    /// Bearer key sent upstream (free models still require an OpenRouter account).
    pub upstream_api_key: String,
    /// How many ranked candidates one request may walk through before giving up.
    pub max_attempts: u32,
}

impl ProxyService {
    pub fn new(resolver: Arc<BestFreeModelResolver>, config: ProxyConfig) -> Self {
        Self {
            resolver,
            http: reqwest::Client::new(),
            config,
        }
    }

    /// Resolve the ordered candidate slugs for one incoming request body.
    ///
    /// Public for tests; the HTTP layer wraps this in status codes.
    pub async fn candidates_for(&self, body: &Value) -> Result<Vec<String>, RouteError> {
        let requested = body["model"].as_str().unwrap_or("").trim();
        let explicit = !requested.is_empty() && requested != "auto";
        if !explicit {
            return self.top_candidates().await;
        }

        let resolution = self.resolution().await?;
        // An explicitly-named slug gets no failover beyond itself unless it too fails over —
        // but it MUST be in the free set, whatever it is.
        if resolution.contains(requested) {
            return Ok(vec![requested.to_string()]);
        }
        Err(RouteError::NotFree {
            requested: requested.to_string(),
            alternatives: top_list(&resolution, 3).join(", "),
        })
    }

    /// Ranked candidates for auto-routing: up to `max_attempts`, best first.
    pub async fn top_candidates(&self) -> Result<Vec<String>, RouteError> {
        let resolution = self.resolution().await?;
        Ok(resolution
            .ranked_ids()
            .into_iter()
            .take(self.config.max_attempts.max(1) as usize)
            .collect())
    }

    async fn resolution(&self) -> Result<Resolution, RouteError> {
        Ok(self.resolver.current().await?)
    }

    /// Issue the upstream call for one candidate and classify the result.
    pub fn classify_response(&self, response: reqwest::Response) -> AttemptOutcome {
        let status = response.status();
        if status.is_success() {
            return AttemptOutcome::Ready(response);
        }
        AttemptOutcome::Failed(format!("HTTP {status}"))
    }

    /// Decide whether an upstream failure justifies trying the next-ranked model.
    ///
    /// The body text decides borderline cases: a 400 whose message names the model or its
    /// endpoints is a candidate problem (fail over); any other 400 is a payload problem (do not).
    pub fn should_fail_over(&self, status: u16, body_text: &str) -> bool {
        if FAILOVER_STATUSES.contains(&status) {
            return true;
        }
        if status == 400 {
            let lowered = body_text.to_ascii_lowercase();
            return MODEL_ERROR_MARKERS.iter().any(|m| lowered.contains(m));
        }
        false
    }

    /// The upstream URL for chat completions.
    pub fn chat_endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.upstream_base.trim_end_matches('/')
        )
    }

    /// Rewrite the body's model field to `slug`.
    pub fn rewrite_model(body: &mut Value, slug: &str) {
        body["model"] = json!(slug);
    }
}

fn top_list(resolution: &Resolution, n: usize) -> Vec<String> {
    resolution.ranked_ids().into_iter().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::free::FreeModel;
    use crate::rank::ModelScores;

    struct FixedDiscovery(Vec<FreeModel>);
    #[async_trait::async_trait]
    impl crate::resolver::FreeModelDiscovery for FixedDiscovery {
        async fn discover(&self) -> Result<Vec<FreeModel>, String> {
            Ok(self.0.clone())
        }
    }

    struct FailingBenchmarks;
    #[async_trait::async_trait]
    impl crate::resolver::CodingBenchmarkSource for FailingBenchmarks {
        async fn coding_benchmark_rows(&self) -> Result<Vec<(String, ModelScores)>, String> {
            Err("unavailable".into())
        }
    }

    struct EmptyScrapes;
    #[async_trait::async_trait]
    impl crate::resolver::ScrapeRankingSource for EmptyScrapes {
        async fn scraped_leaderboard_rows(&self) -> Vec<(String, ModelScores)> {
            Vec::new()
        }
    }

    fn fm(id: &str, ctx: u64, tools: bool) -> FreeModel {
        FreeModel {
            id: id.into(),
            context_length: ctx,
            supports_tools: tools,
        }
    }

    fn service() -> ProxyService {
        let resolver = Arc::new(BestFreeModelResolver::new(
            Arc::new(FixedDiscovery(vec![
                fm("best/m", 100_000, true),
                fm("second/m", 64_000, true),
                fm("third/m", 32_000, false),
            ])),
            Arc::new(FailingBenchmarks),
            Arc::new(EmptyScrapes),
            std::time::Duration::from_secs(3600),
        ));
        ProxyService::new(
            resolver,
            ProxyConfig {
                upstream_base: "https://up.example/api/v1/".into(),
                upstream_api_key: "sk".into(),
                max_attempts: 2,
            },
        )
    }

    #[tokio::test]
    async fn auto_bodies_get_the_ranked_top_candidates() {
        for model in [None, Some("auto"), Some("  ")] {
            let body = match model {
                None => json!({"messages": []}),
                Some(m) => json!({"model": m, "messages": []}),
            };
            let got = service().candidates_for(&body).await.expect("candidates");
            assert_eq!(got, vec!["best/m", "second/m"], "model={model:?}");
        }
    }

    #[tokio::test]
    async fn an_explicit_free_slug_is_honoured_alone() {
        let body = json!({"model": "third/m", "messages": []});
        assert_eq!(
            service().candidates_for(&body).await.expect("candidates"),
            vec!["third/m"]
        );
    }

    #[tokio::test]
    async fn an_explicit_paid_or_unknown_slug_is_refused_with_alternatives() {
        let body = json!({"model": "openai/gpt-4o", "messages": []});
        let err = service()
            .candidates_for(&body)
            .await
            .expect_err("must refuse");
        let RouteError::NotFree {
            requested,
            alternatives,
        } = err
        else {
            panic!("expected NotFree");
        };
        assert_eq!(requested, "openai/gpt-4o");
        assert!(alternatives.starts_with("best/m"), "{alternatives}");
    }

    #[test]
    fn candidate_failure_statuses_fail_over() {
        let svc = test_service();
        for s in [402u16, 404, 408, 429] {
            assert!(svc.should_fail_over(s, ""), "{s} should fail over");
        }
        assert!(!svc.should_fail_over(500, "internal"));
        assert!(!svc.should_fail_over(401, "bad key"));
    }

    #[test]
    fn a_400_about_the_model_fails_over_but_a_payload_400_does_not() {
        let svc = test_service();
        assert!(svc.should_fail_over(
            400,
            "{\"error\":{\"message\":\"No endpoints found for z-ai/gone\"}}"
        ));
        assert!(svc.should_fail_over(400, "Rate limit exceeded for this model"));
        assert!(!svc.should_fail_over(400, "'messages' must be an array"));
    }

    #[test]
    fn model_field_is_rewritten_in_place() {
        let mut body = json!({"model": "auto", "messages": []});
        ProxyService::rewrite_model(&mut body, "z-ai/glm-5.2:free");
        assert_eq!(body["model"], json!("z-ai/glm-5.2:free"));
        assert!(
            body.get("messages").is_some(),
            "the rest of the body survives"
        );
    }

    #[tokio::test]
    async fn chat_endpoint_joins_paths_and_strips_trailing_slash() {
        assert_eq!(
            service().chat_endpoint(),
            "https://up.example/api/v1/chat/completions"
        );
    }

    fn test_service() -> ProxyService {
        // The failover-classifier tests never touch the network or the resolver, so a resolver
        // over empty fixtures is exactly right here.
        let resolver = Arc::new(BestFreeModelResolver::new(
            Arc::new(FixedDiscovery(vec![])),
            Arc::new(FailingBenchmarks),
            Arc::new(EmptyScrapes),
            std::time::Duration::from_secs(3600),
        ));
        ProxyService::new(
            resolver,
            ProxyConfig {
                upstream_base: "https://up.example/api/v1".into(),
                upstream_api_key: "sk".into(),
                max_attempts: 3,
            },
        )
    }
}
