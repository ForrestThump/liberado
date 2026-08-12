//! # liberado-provider-openai-compat
//!
//! One [`Provider`] implementation for every backend that speaks the OpenAI-compatible
//! chat-completions wire format (DeepSeek, OpenRouter, and — without touching this crate again —
//! OpenAI direct, Groq, Together, or anything else shaped the same way). It replaces what used to
//! be `liberado-provider-deepseek` and `liberado-provider-openrouter`: two crates that were ~90%
//! byte-for-byte identical scaffolding around the shared `liberado_provider::openai_compat` module,
//! differing only in base URL, default model, env var names, and one status-code quirk
//! (OpenRouter's extra `402` for insufficient credits). See `docs/future-work/archive/hygiene-audit-2026-07-05.md`
//! for the audit finding that named this, and `crates/config-loader/src/model.rs`'s
//! `ProviderProfile`/`Topology.providers` for how a *new* backend gets added from here on — a TOML
//! entry, not a new crate.
//!
//! This crate only owns the actual HTTP round-trip (the POST/GET calls, status-code handling,
//! byte-stream consumption) and the small set of things that differ per backend. Everything else
//! (tool-name sanitization, request/response mapping, SSE stream assembly, status-code mapping,
//! `/models` response parsing) lives in `liberado_provider::openai_compat` — see that module's own
//! doc comment for why it's shared rather than duplicated.

use async_trait::async_trait;
use liberado_provider::openai_compat::{
    build_tool_name_map, from_openai_response, map_status, parse_models_response,
    stream_sse_response, to_openai_request,
};
use liberado_provider::{
    CompletionRequest, CompletionResponse, CompletionStream, Provider, ProviderError,
    ProviderResult,
};
use serde_json::{Value, json};

/// A [`Provider`] backed by any OpenAI-compatible chat-completions API, parameterized by the
/// small set of things that actually differ between backends.
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    api_key: String,
    /// Active model id — `RwLock` so chat can hot-swap without restarting the daemon (TUI `/model`).
    model: std::sync::RwLock<String>,
    base_url: String,
    /// Status codes beyond the common set (429/400/401/403/404/422) this backend's own API treats
    /// as a client error rather than a generic transport failure — e.g. OpenRouter's `402`.
    extra_client_error_status: Vec<u16>,
    /// Per-role sampling temperature (config-driven). When `Some`, it **overrides** the per-request
    /// temperature on every call this provider makes — so a role can be tuned from config.
    temperature: Option<f32>,
    /// Per-role reasoning ("thinking") effort — `"off"`, `"low"`, `"medium"`, or `"high"`. Mapped to
    /// the OpenAI-compatible `reasoning` body field when `Some`.
    reasoning_effort: Option<String>,
}

impl OpenAiCompatibleProvider {
    /// DeepSeek's API base URL.
    pub const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
    /// DeepSeek's default chat model.
    pub const DEEPSEEK_DEFAULT_MODEL: &str = "deepseek-chat";
    /// OpenRouter's API base URL.
    pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
    /// A broadly-available, cheap default for OpenRouter — arbitrary, meant to be overridden.
    /// Kept as a concrete model string (not OpenRouter's `"auto"` routing) so a given run's model
    /// choice is reproducible, which matters for tuning/evaluation.
    pub const OPENROUTER_DEFAULT_MODEL: &str = "openai/gpt-4o-mini";

    /// Build a provider with an explicit key, model, and base URL. `extra_client_error_status`
    /// defaults to empty — use [`with_extra_client_error_status`](Self::with_extra_client_error_status)
    /// for a backend that needs one.
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            model: std::sync::RwLock::new(model.into()),
            base_url: base_url.into(),
            extra_client_error_status: Vec::new(),
            temperature: None,
            reasoning_effort: None,
        }
    }

    /// Set a per-role sampling temperature that overrides the per-request value (config-driven).
    /// `None` leaves per-call behavior unchanged.
    pub fn with_temperature(mut self, temperature: Option<f32>) -> Self {
        self.temperature = temperature;
        self
    }

    /// Set a per-role reasoning effort (`"off"`/`"low"`/`"medium"`/`"high"`), mapped to the
    /// `reasoning` body field. `None` uses the provider/model default.
    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// Inject the per-role sampling overrides into an already-built request body. Applied on both
    /// the blocking and streaming paths so the two never drift.
    fn apply_role_tuning(&self, body: &mut Value) {
        if let Some(t) = self.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(effort) = &self.reasoning_effort {
            // OpenAI-compatible reasoning control: `off` disables thinking; otherwise pass the
            // effort level. OpenRouter and OpenAI both accept the `reasoning` object shape.
            body["reasoning"] = match effort.as_str() {
                "off" | "none" | "disabled" => json!({ "enabled": false }),
                other => json!({ "effort": other }),
            };
        }
    }

    /// Declare extra status codes this backend's API treats as client errors (see
    /// [`Self::extra_client_error_status`]'s field doc comment).
    pub fn with_extra_client_error_status(mut self, codes: Vec<u16>) -> Self {
        self.extra_client_error_status = codes;
        self
    }

    /// Override the API base URL (e.g. to point at a mock server in tests).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Generic env-based constructor: reads `api_key_env`; if `model_env` is `Some` and set in
    /// the environment, it overrides `default_model`. Plain string/slice parameters (not a shared
    /// config type) so this crate stays with zero dependency on `liberado-config-loader` — the
    /// same reason `liberado-provider-deepseek`/`liberado-provider-openrouter` never depended on
    /// it either. `crates/bootstrap/src/lib.rs`'s `provider_from_config` is the config-driven
    /// caller; [`Self::deepseek_from_env`]/[`Self::openrouter_from_env`] below are the two
    /// well-known backends' ergonomic one-line wrappers over this.
    pub fn from_env(
        api_key_env: &str,
        model_env: Option<&str>,
        default_model: &str,
        base_url: &str,
        extra_client_error_status: Vec<u16>,
    ) -> Result<Self, std::env::VarError> {
        let api_key = std::env::var(api_key_env)?;
        let model = model_env
            .and_then(|var| std::env::var(var).ok())
            .unwrap_or_else(|| default_model.to_string());
        Ok(Self::new(api_key, model, base_url)
            .with_extra_client_error_status(extra_client_error_status))
    }

    /// Build a provider from the `DEEPSEEK_API_KEY` environment variable, using
    /// [`Self::DEEPSEEK_DEFAULT_MODEL`]. `DEEPSEEK_MODEL` overrides the model if present.
    pub fn deepseek_from_env() -> Result<Self, std::env::VarError> {
        Self::from_env(
            "DEEPSEEK_API_KEY",
            Some("DEEPSEEK_MODEL"),
            Self::DEEPSEEK_DEFAULT_MODEL,
            Self::DEEPSEEK_BASE_URL,
            Vec::new(),
        )
    }

    /// Build a provider from the `OPENROUTER_API_KEY` environment variable, using
    /// [`Self::OPENROUTER_DEFAULT_MODEL`]. `OPENROUTER_MODEL` overrides the model if present.
    pub fn openrouter_from_env() -> Result<Self, std::env::VarError> {
        Self::from_env(
            "OPENROUTER_API_KEY",
            Some("OPENROUTER_MODEL"),
            Self::OPENROUTER_DEFAULT_MODEL,
            Self::OPENROUTER_BASE_URL,
            vec![402],
        )
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    fn models_endpoint(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }

    /// `GET {base}/models` — used by [`Provider::list_models`].
    async fn fetch_model_ids(&self) -> ProviderResult<Vec<String>> {
        let response = self
            .client
            .get(self.models_endpoint())
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let detail = response
                .text()
                .await
                .unwrap_or_else(|e| format!("<error body unavailable: {e}>"));
            return Err(map_status(
                status.as_u16(),
                detail,
                &self.extra_client_error_status,
            ));
        }

        let value: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Transport(format!("malformed response body: {e}")))?;
        Ok(parse_models_response(&value))
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn model(&self) -> String {
        self.model.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_model(&self, model: String) {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        *self.model.write().unwrap_or_else(|e| e.into_inner()) = model.to_string();
    }

    async fn list_models(&self) -> ProviderResult<Vec<String>> {
        self.fetch_model_ids().await
    }

    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        let name_map = build_tool_name_map(&request.tools);
        let model = self.model();
        let mut body = to_openai_request(&model, &request, &name_map);
        self.apply_role_tuning(&mut body);

        let response = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let detail = response
                .text()
                .await
                .unwrap_or_else(|e| format!("<error body unavailable: {e}>"));
            return Err(map_status(
                status.as_u16(),
                detail,
                &self.extra_client_error_status,
            ));
        }

        let value: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Transport(format!("malformed response body: {e}")))?;
        from_openai_response(&value, &name_map)
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> ProviderResult<CompletionStream> {
        let name_map = build_tool_name_map(&request.tools);
        let model = self.model();
        let mut body = to_openai_request(&model, &request, &name_map);
        self.apply_role_tuning(&mut body);
        body["stream"] = json!(true);
        // Ask for the trailing usage chunk so streamed calls report token counts (latency journal).
        body["stream_options"] = json!({ "include_usage": true });

        let response = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let detail = response
                .text()
                .await
                .unwrap_or_else(|e| format!("<error body unavailable: {e}>"));
            return Err(map_status(
                status.as_u16(),
                detail,
                &self.extra_client_error_status,
            ));
        }

        Ok(stream_sse_response(response, name_map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_provider::Message;

    #[test]
    fn constructor_sets_fields() {
        let provider = OpenAiCompatibleProvider::new("sk-abc", "my-model", "https://example.com");
        assert_eq!(provider.model(), "my-model");
        assert_eq!(provider.api_key, "sk-abc");
        assert_eq!(provider.base_url, "https://example.com");
        assert!(provider.extra_client_error_status.is_empty());
    }

    #[test]
    fn with_extra_client_error_status_sets_codes() {
        let provider = OpenAiCompatibleProvider::new("k", "m", "https://example.com")
            .with_extra_client_error_status(vec![402]);
        assert_eq!(provider.extra_client_error_status, vec![402]);
    }

    #[test]
    fn with_base_url_overrides_default() {
        let provider = OpenAiCompatibleProvider::new("k", "m", "https://a.example.com")
            .with_base_url("http://localhost:8080");
        assert_eq!(provider.base_url, "http://localhost:8080");
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let provider = OpenAiCompatibleProvider::new("k", "m", "https://api.example.com/");
        assert_eq!(
            provider.endpoint(),
            "https://api.example.com/chat/completions"
        );
        assert_eq!(provider.models_endpoint(), "https://api.example.com/models");
    }

    #[test]
    fn endpoint_without_trailing_slash() {
        let provider = OpenAiCompatibleProvider::new("k", "m", "https://api.example.com");
        assert_eq!(
            provider.endpoint(),
            "https://api.example.com/chat/completions"
        );
    }

    #[test]
    fn model_getter_returns_configured_model() {
        let provider = OpenAiCompatibleProvider::new("k", "custom-model-v2", "https://example.com");
        assert_eq!(provider.model(), "custom-model-v2");
    }

    #[test]
    fn set_model_hot_swaps_active_id() {
        let provider = OpenAiCompatibleProvider::new("k", "deepseek-chat", "https://example.com");
        assert_eq!(provider.model(), "deepseek-chat");
        provider.set_model("deepseek-v4-pro".into());
        assert_eq!(provider.model(), "deepseek-v4-pro");
        provider.set_model("  ".into()); // empty/whitespace ignored
        assert_eq!(provider.model(), "deepseek-v4-pro");
    }

    #[test]
    fn deepseek_from_env_uses_environment_variables() {
        let result = OpenAiCompatibleProvider::deepseek_from_env();
        if std::env::var("DEEPSEEK_API_KEY").is_ok() {
            let provider = result.expect("from_env should succeed when DEEPSEEK_API_KEY is set");
            assert_eq!(provider.api_key, std::env::var("DEEPSEEK_API_KEY").unwrap());
            assert_eq!(
                provider.base_url,
                OpenAiCompatibleProvider::DEEPSEEK_BASE_URL
            );
            assert!(provider.extra_client_error_status.is_empty());
        } else {
            assert!(
                result.is_err(),
                "from_env should fail when DEEPSEEK_API_KEY is unset"
            );
        }
    }

    #[test]
    fn openrouter_from_env_uses_environment_variables() {
        let result = OpenAiCompatibleProvider::openrouter_from_env();
        if std::env::var("OPENROUTER_API_KEY").is_ok() {
            let provider = result.expect("from_env should succeed when OPENROUTER_API_KEY is set");
            assert_eq!(
                provider.api_key,
                std::env::var("OPENROUTER_API_KEY").unwrap()
            );
            assert_eq!(
                provider.base_url,
                OpenAiCompatibleProvider::OPENROUTER_BASE_URL
            );
            assert_eq!(provider.extra_client_error_status, vec![402]);
        } else {
            assert!(
                result.is_err(),
                "from_env should fail when OPENROUTER_API_KEY is unset"
            );
        }
    }

    #[test]
    fn generic_from_env_works_for_an_arbitrary_new_backend() {
        // Exercise the generic constructor directly with a made-up backend/env var pair — proves
        // a brand new provider (no dedicated Rust wrapper, no `deepseek_from_env`-style helper)
        // still goes through this exact same path, which is the whole point of collapsing the two
        // old crates into this one. Doesn't mutate env vars (races under parallel test runs, same
        // reason `deepseek_from_env_uses_environment_variables` above only asserts conditionally on
        // whatever the real environment happens to be) — asserts the clean failure shape instead,
        // which is just as real a proof the generic path is wired correctly.
        let result = OpenAiCompatibleProvider::from_env(
            "LIBERADO_TEST_PROVIDER_KEY_DOES_NOT_EXIST",
            Some("LIBERADO_TEST_PROVIDER_MODEL_DOES_NOT_EXIST"),
            "some-default-model",
            "https://example.invalid",
            vec![418],
        );
        assert!(
            result.is_err(),
            "from_env should fail when its api_key_env isn't set"
        );
    }

    #[tokio::test]
    #[ignore = "requires DEEPSEEK_API_KEY + network access"]
    async fn deepseek_live_smoke() {
        let provider =
            OpenAiCompatibleProvider::deepseek_from_env().expect("DEEPSEEK_API_KEY not set");
        let resp = provider
            .complete(CompletionRequest::new(vec![Message::user(
                "Reply with exactly one word: pong",
            )]))
            .await
            .expect("live call failed");
        assert!(
            resp.content.is_some(),
            "expected text content from DeepSeek"
        );
    }

    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY + network access"]
    async fn openrouter_live_smoke() {
        let provider =
            OpenAiCompatibleProvider::openrouter_from_env().expect("OPENROUTER_API_KEY not set");
        let resp = provider
            .complete(CompletionRequest::new(vec![Message::user(
                "Reply with exactly one word: pong",
            )]))
            .await
            .expect("live call failed");
        assert!(
            resp.content.is_some(),
            "expected text content from OpenRouter"
        );
    }

    #[tokio::test]
    #[ignore = "requires DEEPSEEK_API_KEY + network access"]
    async fn deepseek_list_models_live_smoke() {
        let provider =
            OpenAiCompatibleProvider::deepseek_from_env().expect("DEEPSEEK_API_KEY not set");
        let models = provider.list_models().await.expect("live call failed");
        assert!(!models.is_empty(), "expected at least one model id");
    }
}

/// Seam tests: what this provider actually puts on the wire.
///
/// The tests above this module assert that constructors set struct fields. That is a different
/// question from whether a set field reaches the request body, and the gap between those two
/// questions is where a real bug lived: every caller of `complete_json` built a correct JSON
/// schema and `to_openai_request` dropped it, because the only tests were of the callers and of
/// the fields. So these assert on the serialized body captured off an HTTP listener — the last
/// point before the bytes are someone else's problem.
///
/// Deliberately paired across `complete` and `complete_stream`: [`apply_role_tuning`]'s doc claims
/// it is "applied on both the blocking and streaming paths so the two never drift", and an
/// untested claim about drift is exactly the kind that comes true.
#[cfg(test)]
mod wire_seam {
    use super::*;
    use liberado_provider::{Message, ResponseFormat, ToolDef};
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// Captures each request body it serves, so a test can assert on what was sent.
    pub(crate) struct Capture {
        bodies: Arc<Mutex<Vec<Value>>>,
        response: ResponseTemplate,
    }

    impl Respond for Capture {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&request.body)
                .expect("outgoing request body should be valid JSON");
            self.bodies.lock().unwrap().push(body);
            self.response.clone()
        }
    }

    /// A chat-completions server that records what it was sent. Returns the server (which must be
    /// kept alive for the duration of the call) and the handle the bodies land in.
    pub(crate) async fn recording_server(
        response: ResponseTemplate,
    ) -> (MockServer, Arc<Mutex<Vec<Value>>>) {
        let server = MockServer::start().await;
        let bodies = Arc::new(Mutex::new(Vec::new()));
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(Capture {
                bodies: Arc::clone(&bodies),
                response,
            })
            .mount(&server)
            .await;
        (server, bodies)
    }

    pub(crate) fn ok_reply() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "ok" } }]
        }))
    }

    fn sse_reply() -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string("data: [DONE]\n\n")
    }

    fn provider_at(server: &MockServer) -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new("sk-test", "test-model", server.uri())
    }

    /// Drive one blocking call through `provider` and hand back the body it sent.
    async fn sent_by_complete(
        build: impl FnOnce(OpenAiCompatibleProvider) -> OpenAiCompatibleProvider,
        request: CompletionRequest,
    ) -> Value {
        let (server, bodies) = recording_server(ok_reply()).await;
        let provider = build(provider_at(&server));
        provider.complete(request).await.expect("call failed");
        let sent = bodies.lock().unwrap();
        sent.first().cloned().expect("no request was captured")
    }

    /// Same, for the streaming path.
    async fn sent_by_stream(
        build: impl FnOnce(OpenAiCompatibleProvider) -> OpenAiCompatibleProvider,
        request: CompletionRequest,
    ) -> Value {
        let (server, bodies) = recording_server(sse_reply()).await;
        let provider = build(provider_at(&server));
        let _stream = provider
            .complete_stream(request)
            .await
            .expect("stream call failed");
        let sent = bodies.lock().unwrap();
        sent.first().cloned().expect("no request was captured")
    }

    fn one_turn() -> CompletionRequest {
        CompletionRequest::new(vec![Message::user("hi")])
    }

    /// Temperatures are `f32` in the request type and `f64` once serialized, so `0.7` arrives as
    /// `0.699999988079071`. Compare the value, not the representation.
    fn assert_temperature(body: &Value, expected: f32) {
        let actual = body["temperature"]
            .as_f64()
            .unwrap_or_else(|| panic!("no temperature in {body}"));
        assert!(
            (actual - expected as f64).abs() < 1e-6,
            "expected temperature ~{expected}, got {actual}"
        );
    }

    // ---- the model id ----

    #[tokio::test]
    async fn the_active_model_is_what_gets_sent() {
        let (server, bodies) = recording_server(ok_reply()).await;
        let provider = provider_at(&server);
        provider.set_model("deepseek/deepseek-v4-flash".into());
        provider.complete(one_turn()).await.unwrap();
        assert_eq!(
            bodies.lock().unwrap()[0]["model"],
            json!("deepseek/deepseek-v4-flash"),
            "a hot-swapped model must reach the wire, not just the RwLock"
        );
    }

    // ---- role temperature ----

    #[tokio::test]
    async fn role_temperature_overrides_the_per_request_value() {
        // The field doc promises an override, not a default. A role pinned to 0 exists precisely
        // to beat a caller that asked for something warmer.
        let mut request = one_turn();
        request.temperature = Some(0.7);
        let body = sent_by_complete(|p| p.with_temperature(Some(0.0)), request).await;
        assert_temperature(&body, 0.0);
    }

    #[tokio::test]
    async fn without_a_role_temperature_the_request_value_survives() {
        let mut request = one_turn();
        request.temperature = Some(0.7);
        let body = sent_by_complete(|p| p, request).await;
        assert_temperature(&body, 0.7);
    }

    #[tokio::test]
    async fn no_temperature_anywhere_sends_no_temperature_field() {
        // Distinct from sending `null` — some backends reject an explicit null where they would
        // happily apply their own default for an absent key.
        let body = sent_by_complete(|p| p, one_turn()).await;
        assert!(
            body.get("temperature").is_none(),
            "expected the key to be absent, got {:?}",
            body.get("temperature")
        );
    }

    // ---- reasoning effort ----

    #[tokio::test]
    async fn reasoning_off_disables_thinking_on_the_wire() {
        let body =
            sent_by_complete(|p| p.with_reasoning_effort(Some("off".into())), one_turn()).await;
        assert_eq!(body["reasoning"], json!({ "enabled": false }));
    }

    #[tokio::test]
    async fn the_other_disable_spellings_mean_the_same_thing() {
        for spelling in ["none", "disabled"] {
            let body = sent_by_complete(
                |p| p.with_reasoning_effort(Some(spelling.into())),
                one_turn(),
            )
            .await;
            assert_eq!(
                body["reasoning"],
                json!({ "enabled": false }),
                "{spelling} should disable reasoning like `off` does"
            );
        }
    }

    #[tokio::test]
    async fn a_reasoning_level_is_sent_as_an_effort() {
        for level in ["low", "medium", "high"] {
            let body =
                sent_by_complete(|p| p.with_reasoning_effort(Some(level.into())), one_turn()).await;
            assert_eq!(body["reasoning"], json!({ "effort": level }));
        }
    }

    #[tokio::test]
    async fn no_reasoning_override_sends_no_reasoning_field() {
        let body = sent_by_complete(|p| p, one_turn()).await;
        assert!(
            body.get("reasoning").is_none(),
            "an unset role reasoning level must leave the provider/model default alone"
        );
    }

    // ---- tools ----

    #[tokio::test]
    async fn tools_reach_the_wire_with_their_schemas_intact() {
        let mut request = one_turn();
        request.tools = vec![ToolDef {
            name: "vault_search".into(),
            description: "search the vault".into(),
            parameters: json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
            }),
        }];
        let body = sent_by_complete(|p| p, request).await;
        let tools = body["tools"].as_array().expect("tools should be an array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], json!("vault_search"));
        // The parameter schema is the part a model actually needs; dropping it is silent and fatal.
        assert_eq!(
            tools[0]["function"]["parameters"]["required"],
            json!(["query"]),
            "a tool's parameter schema must survive the trip"
        );
    }

    #[tokio::test]
    async fn no_tools_sends_no_tools_field() {
        // An empty `tools: []` is not the same request as one with no tools — some backends
        // error on it, and it changes how others prompt.
        let body = sent_by_complete(|p| p, one_turn()).await;
        assert!(
            body.get("tools").is_none(),
            "expected no tools key, got {:?}",
            body.get("tools")
        );
    }

    // ---- response format ----

    #[tokio::test]
    async fn a_shape_constraining_schema_is_sent_as_strict_json_schema() {
        // The regression this whole module exists for: four callers built this schema correctly
        // and the request builder threw it away.
        let schema = json!({
            "type": "object",
            "properties": { "kind": { "type": "string" } },
            "required": ["kind"],
        });
        let mut request = one_turn();
        request.response_format = ResponseFormat::Json {
            schema: schema.clone(),
        };
        let body = sent_by_complete(|p| p, request).await;
        assert_eq!(body["response_format"]["type"], json!("json_schema"));
        assert_eq!(
            body["response_format"]["json_schema"]["strict"],
            json!(true)
        );
        assert_eq!(
            body["response_format"]["json_schema"]["schema"], schema,
            "the caller's schema must be the one sent"
        );
    }

    #[tokio::test]
    async fn a_shapeless_schema_falls_back_to_plain_json_mode() {
        // `strict` mode cannot express "an object, contents unspecified", so a schema that
        // constrains nothing must degrade to json_object rather than be sent and rejected.
        let mut request = one_turn();
        request.response_format = ResponseFormat::Json {
            schema: json!({ "type": "object" }),
        };
        let body = sent_by_complete(|p| p, request).await;
        assert_eq!(body["response_format"], json!({ "type": "json_object" }));
    }

    // ---- max_tokens ----

    #[tokio::test]
    async fn max_tokens_reaches_the_wire() {
        let mut request = one_turn();
        request.max_tokens = Some(512);
        let body = sent_by_complete(|p| p, request).await;
        assert_eq!(body["max_tokens"], json!(512));
    }

    #[tokio::test]
    async fn no_max_tokens_sends_no_max_tokens_field() {
        let body = sent_by_complete(|p| p, one_turn()).await;
        assert!(
            body.get("max_tokens").is_none(),
            "an absent cap must not become an explicit one"
        );
    }

    #[tokio::test]
    async fn plain_text_requests_send_no_response_format() {
        let body = sent_by_complete(|p| p, one_turn()).await;
        assert!(
            body.get("response_format").is_none(),
            "a plain completion must not be silently constrained to JSON"
        );
    }

    // ---- the streaming path carries everything the blocking path does ----

    #[tokio::test]
    async fn streaming_sends_the_same_role_tuning_as_blocking() {
        let build = |p: OpenAiCompatibleProvider| {
            p.with_temperature(Some(0.25))
                .with_reasoning_effort(Some("high".into()))
        };
        let blocking = sent_by_complete(build, one_turn()).await;
        let streaming = sent_by_stream(build, one_turn()).await;

        assert_eq!(streaming["temperature"], blocking["temperature"]);
        assert_eq!(streaming["reasoning"], blocking["reasoning"]);
        assert_temperature(&streaming, 0.25);
        assert_eq!(streaming["reasoning"], json!({ "effort": "high" }));
    }

    #[tokio::test]
    async fn streaming_asks_for_the_trailing_usage_chunk() {
        // Without this the latency journal records zero tokens for every streamed call, which is
        // the kind of quiet wrongness that reads as a real measurement.
        let body = sent_by_stream(|p| p, one_turn()).await;
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));
    }

    #[tokio::test]
    async fn the_blocking_path_does_not_ask_to_stream() {
        let body = sent_by_complete(|p| p, one_turn()).await;
        assert!(
            body.get("stream").is_none() || body["stream"] == json!(false),
            "blocking calls must not set stream=true"
        );
        assert!(body.get("stream_options").is_none());
    }

    // ---- auth ----

    #[tokio::test]
    async fn the_api_key_is_sent_as_a_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer sk-test",
            ))
            .respond_with(ok_reply())
            .expect(1)
            .mount(&server)
            .await;
        provider_at(&server).complete(one_turn()).await.unwrap();
        // `expect(1)` is verified on drop — an unauthenticated request would not have matched.
    }
}

/// CH4: a session profile's model must reach the wire without mutating the shared provider.
#[cfg(test)]
mod per_request_model {
    use super::wire_seam::*;
    use super::*;
    use liberado_provider::Message;

    #[tokio::test]
    async fn a_request_model_overrides_the_providers_own() {
        let (server, bodies) = recording_server(ok_reply()).await;
        let provider = OpenAiCompatibleProvider::new("sk-test", "daemon-default", server.uri());
        let request = CompletionRequest::new(vec![Message::user("hi")])
            .with_model(Some("deepseek/deepseek-v4-flash".into()));

        provider.complete(request).await.unwrap();

        assert_eq!(
            bodies.lock().unwrap()[0]["model"],
            json!("deepseek/deepseek-v4-flash"),
            "a profile naming a model must beat the daemon default"
        );
        assert_eq!(
            provider.model(),
            "daemon-default",
            "and must not mutate the provider every other session shares"
        );
    }

    #[tokio::test]
    async fn without_a_request_model_the_provider_still_decides() {
        let (server, bodies) = recording_server(ok_reply()).await;
        let provider = OpenAiCompatibleProvider::new("sk-test", "daemon-default", server.uri());
        provider
            .complete(CompletionRequest::new(vec![Message::user("hi")]))
            .await
            .unwrap();
        assert_eq!(bodies.lock().unwrap()[0]["model"], json!("daemon-default"));
    }

    /// The hot-swap (`/model` in the TUI) and a profile can disagree. The profile wins: naming a
    /// model in config is an explicit per-session choice, the swap is a daemon-wide default.
    #[tokio::test]
    async fn a_request_model_also_beats_a_hot_swapped_one() {
        let (server, bodies) = recording_server(ok_reply()).await;
        let provider = OpenAiCompatibleProvider::new("sk-test", "original", server.uri());
        provider.set_model("hot-swapped".into());

        provider
            .complete(
                CompletionRequest::new(vec![Message::user("hi")])
                    .with_model(Some("profile-model".into())),
            )
            .await
            .unwrap();

        assert_eq!(bodies.lock().unwrap()[0]["model"], json!("profile-model"));
    }
}
