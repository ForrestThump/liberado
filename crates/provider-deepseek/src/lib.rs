//! # liberado-provider-deepseek
//!
//! A concrete [`Provider`] backed by DeepSeek's OpenAI-compatible chat-completions API. It is a
//! thin translation layer: map the normalized [`CompletionRequest`] to the OpenAI wire shape,
//! POST it, and map the response back to a [`CompletionResponse`]. All orchestration, safety, and
//! routing live above the [`Provider`] boundary — this crate only speaks HTTP.
//!
//! The wire-format translation itself (tool-name sanitization, request/response mapping, SSE
//! stream assembly) lives in `liberado_provider::openai_compat`, shared with
//! `liberado-provider-openrouter` since both backends speak the same OpenAI-compatible format —
//! see that module's doc comment for why (`docs/roadmap/hygiene-audit-2026-07-04.md`). This crate
//! only owns the actual HTTP round-trip and its own defaults/env-var names/status-code mapping.

use async_trait::async_trait;
use futures::StreamExt;
use liberado_provider::openai_compat::{
    ToolAcc, accumulate_tool_deltas, build_tool_name_map, from_openai_response, map_finish_reason,
    to_openai_request,
};
use liberado_provider::{
    CompletionRequest, CompletionResponse, CompletionStream, FinishReason, Provider, ProviderError,
    ProviderResult, StreamItem, ToolInvocation,
};
use serde_json::{Value, json};

/// DeepSeek's default chat model.
pub const DEFAULT_MODEL: &str = "deepseek-chat";
/// DeepSeek's API base URL (overridable via [`DeepSeekProvider::with_base_url`]).
pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

/// A DeepSeek-backed provider.
pub struct DeepSeekProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl DeepSeekProvider {
    /// Build a provider with an explicit key + model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            model: model.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Build a provider from the `DEEPSEEK_API_KEY` environment variable, using [`DEFAULT_MODEL`].
    /// `DEEPSEEK_MODEL` overrides the model if present.
    pub fn from_env() -> Result<Self, std::env::VarError> {
        let api_key = std::env::var("DEEPSEEK_API_KEY")?;
        let model = std::env::var("DEEPSEEK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Ok(Self::new(api_key, model))
    }

    /// Override the API base URL (e.g. to point at a mock server in tests).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl Provider for DeepSeekProvider {
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        let name_map = build_tool_name_map(&request.tools);
        let body = to_openai_request(&self.model, &request, &name_map);

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
            return Err(map_status(status.as_u16(), detail));
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
        let mut body = to_openai_request(&self.model, &request, &name_map);
        body["stream"] = json!(true);

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
            return Err(map_status(status.as_u16(), detail));
        }

        // Parse the OpenAI SSE stream: each `data:` line is a chunk with a `delta`. Emit content
        // deltas as tokens; accumulate tool-call deltas (id once, name once, arguments concatenated)
        // and the finish reason, then emit the assembled response as the final `Done`.
        let stream = async_stream::try_stream! {
            let mut bytes = response.bytes_stream();
            let mut buf = String::new();
            let mut content = String::new();
            let mut tools: Vec<ToolAcc> = Vec::new();
            let mut finish = FinishReason::Stop;

            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.map_err(|e| ProviderError::Transport(e.to_string()))?;
                buf.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(nl) = buf.find('\n') {
                    let line: String = buf.drain(..=nl).collect();
                    let Some(data) = line.trim().strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                    let choice = &v["choices"][0];
                    if let Some(fr) = choice["finish_reason"].as_str() {
                        finish = map_finish_reason(fr);
                    }
                    let delta = &choice["delta"];
                    if let Some(t) = delta["content"].as_str()
                        && !t.is_empty()
                    {
                        content.push_str(t);
                        yield StreamItem::Token(t.to_string());
                    }
                    if let Some(deltas) = delta["tool_calls"].as_array() {
                        accumulate_tool_deltas(&mut tools, deltas);
                    }
                }
            }

            let tool_calls: Vec<ToolInvocation> =
                tools.into_iter().filter_map(|acc| acc.into_invocation(&name_map)).collect();
            yield StreamItem::Done(CompletionResponse {
                content: (!content.is_empty()).then_some(content),
                tool_calls,
                finish_reason: finish,
                usage: None,
            });
        };

        Ok(Box::pin(stream))
    }
}

/// Map an HTTP error status to a typed [`ProviderError`] (Decision 13: callers branch on these).
fn map_status(status: u16, body: String) -> ProviderError {
    match status {
        429 => ProviderError::RateLimited,
        400 | 401 | 403 | 404 | 422 => {
            ProviderError::InvalidRequest(format!("HTTP {status}: {body}"))
        }
        _ => ProviderError::Transport(format!("HTTP {status}: {body}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_provider::Message;

    #[test]
    fn status_mapping() {
        assert!(matches!(
            map_status(429, "x".into()),
            ProviderError::RateLimited
        ));
        assert!(matches!(
            map_status(401, "x".into()),
            ProviderError::InvalidRequest(_)
        ));
        assert!(matches!(
            map_status(500, "x".into()),
            ProviderError::Transport(_)
        ));
    }

    #[test]
    fn constructor_sets_fields() {
        let provider = DeepSeekProvider::new("sk-abc", "my-model");
        assert_eq!(provider.model, "my-model");
        assert_eq!(provider.api_key, "sk-abc");
        assert_eq!(provider.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn with_base_url_overrides_default() {
        let provider = DeepSeekProvider::new("k", "m").with_base_url("http://localhost:8080");
        assert_eq!(provider.base_url, "http://localhost:8080");
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let provider = DeepSeekProvider::new("k", "m").with_base_url("https://api.example.com/");
        assert_eq!(
            provider.endpoint(),
            "https://api.example.com/chat/completions"
        );
    }

    #[test]
    fn endpoint_without_trailing_slash() {
        let provider = DeepSeekProvider::new("k", "m").with_base_url("https://api.example.com");
        assert_eq!(
            provider.endpoint(),
            "https://api.example.com/chat/completions"
        );
    }

    #[test]
    fn from_env_uses_environment_variables() {
        let result = DeepSeekProvider::from_env();
        if std::env::var("DEEPSEEK_API_KEY").is_ok() {
            let provider = result.expect("from_env should succeed when DEEPSEEK_API_KEY is set");
            assert_eq!(provider.api_key, std::env::var("DEEPSEEK_API_KEY").unwrap());
        } else {
            assert!(
                result.is_err(),
                "from_env should fail when DEEPSEEK_API_KEY is unset"
            );
        }
    }

    #[test]
    fn model_getter_returns_configured_model() {
        let provider = DeepSeekProvider::new("k", "custom-model-v2");
        assert_eq!(provider.model(), "custom-model-v2");
    }

    #[tokio::test]
    #[ignore = "requires DEEPSEEK_API_KEY + network access"]
    async fn live_smoke() {
        let provider = DeepSeekProvider::from_env().expect("DEEPSEEK_API_KEY not set");
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
}
