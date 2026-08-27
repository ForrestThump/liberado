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
    fn apply_role_tuning(&self, body: &mut Value, request: &CompletionRequest) {
        if let Some(t) = self.temperature {
            body["temperature"] = json!(t);
        }
        let effort = request
            .reasoning
            .as_deref()
            .or(self.reasoning_effort.as_deref());
        if let Some(effort) = effort {
            // OpenAI-compatible reasoning control: `off` disables thinking; otherwise pass the
            // effort level. OpenRouter and OpenAI both accept the `reasoning` object shape.
            body["reasoning"] = match effort {
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
        self.apply_role_tuning(&mut body, &request);

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
        self.apply_role_tuning(&mut body, &request);
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
#[path = "lib_tests.rs"]
mod tests;

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
#[path = "lib_wire_seam.rs"]
mod wire_seam;

/// CH4: a session profile's model must reach the wire without mutating the shared provider.
#[cfg(test)]
#[path = "lib_per_request_model.rs"]
mod per_request_model;

#[cfg(test)]
mod list_models_tests;
