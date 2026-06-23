//! # liberado-provider-deepseek
//!
//! A concrete [`Provider`] backed by DeepSeek's OpenAI-compatible chat-completions API. It is a
//! thin translation layer: map the normalized [`CompletionRequest`] to the OpenAI wire shape,
//! POST it, and map the response back to a [`CompletionResponse`]. All orchestration, safety, and
//! routing live above the [`Provider`] boundary — this crate only speaks HTTP.
//!
//! The translation is split into pure functions ([`to_openai_request`], [`from_openai_response`])
//! so the mapping is unit-tested deterministically without a network or an API key; a single
//! `#[ignore]`d smoke test exercises the real endpoint.

use async_trait::async_trait;
use liberado_provider::{
    CompletionRequest, CompletionResponse, FinishReason, Message, Provider, ProviderError,
    ProviderResult, ResponseFormat, Role, ToolDef, ToolInvocation, Usage,
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
        let body = to_openai_request(&self.model, &request);

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
            // Preserve the diagnostic even if reading the body itself fails.
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
        from_openai_response(&value)
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

/// Translate a normalized request into the OpenAI chat-completions request body.
pub fn to_openai_request(model: &str, req: &CompletionRequest) -> Value {
    let mut body = json!({
        "model": model,
        "messages": req.messages.iter().map(message_to_json).collect::<Vec<_>>(),
    });

    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(tool_to_json).collect());
    }
    if matches!(req.response_format, ResponseFormat::Json { .. }) {
        // DeepSeek supports JSON object mode (the prompt must mention "json", which the
        // dispatcher's does); the JSON Schema itself is not sent.
        body["response_format"] = json!({ "type": "json_object" });
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = req.max_tokens {
        body["max_tokens"] = json!(m);
    }
    body
}

fn message_to_json(m: &Message) -> Value {
    let role = match m.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let mut v = json!({ "role": role, "content": m.content });
    if !m.tool_calls.is_empty() {
        v["tool_calls"] = Value::Array(
            m.tool_calls
                .iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": { "name": tc.name, "arguments": tc.arguments.to_string() },
                    })
                })
                .collect(),
        );
    }
    if let Some(id) = &m.tool_call_id {
        v["tool_call_id"] = json!(id);
    }
    v
}

fn tool_to_json(t: &ToolDef) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.parameters,
        },
    })
}

/// Translate an OpenAI chat-completions response body into a normalized [`CompletionResponse`].
pub fn from_openai_response(v: &Value) -> ProviderResult<CompletionResponse> {
    // Some OpenAI-compatible servers return 2xx with a top-level `error` object instead of
    // `choices`; surface it rather than the misleading "empty response".
    if let Some(error) = v.get("error") {
        return Err(ProviderError::InvalidRequest(error.to_string()));
    }

    let choice = v["choices"].get(0).ok_or(ProviderError::EmptyResponse)?;
    let message = &choice["message"];

    let content = message["content"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // A present-but-unparseable tool call is a silent data-loss path; warn rather than drop quietly.
    let tool_calls = message["tool_calls"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let parsed = parse_tool_call(tc);
                    if parsed.is_none() {
                        tracing::warn!(tool_call = %tc, "dropping unparseable tool call");
                    }
                    parsed
                })
                .collect()
        })
        .unwrap_or_default();

    let finish_reason = match choice["finish_reason"].as_str() {
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("length") => FinishReason::Length,
        Some("content_filter") => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    };

    Ok(CompletionResponse {
        content,
        tool_calls,
        finish_reason,
        usage: parse_usage(&v["usage"]),
    })
}

fn parse_tool_call(tc: &Value) -> Option<ToolInvocation> {
    let id = tc["id"].as_str()?.to_string();
    let function = &tc["function"];
    let name = function["name"].as_str()?.to_string();
    // OpenAI encodes arguments as a JSON *string*; parse it back to a value (empty on failure).
    let arguments = function["arguments"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!({}));
    Some(ToolInvocation::new(id, name, arguments))
}

fn parse_usage(u: &Value) -> Option<Usage> {
    if !u.is_object() {
        return None;
    }
    let field = |k: &str| u[k].as_u64().unwrap_or(0) as u32;
    Some(Usage {
        prompt_tokens: field("prompt_tokens"),
        completion_tokens: field("completion_tokens"),
        total_tokens: field("total_tokens"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_maps_messages_temperature_and_json_mode() {
        let req = CompletionRequest::new(vec![Message::system("be terse"), Message::user("hi")])
            .with_temperature(0.0)
            .with_max_tokens(64)
            .with_json_schema(json!({ "type": "object" }));

        let body = to_openai_request("deepseek-chat", &req);
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["max_tokens"], 64);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn request_maps_tools_and_tool_messages() {
        // An assistant message that requested a tool call...
        let assistant = Message {
            role: Role::Assistant,
            content: "calling".into(),
            tool_calls: vec![ToolInvocation::new("call-1", "add", json!({ "x": 1 }))],
            tool_call_id: None,
        };
        let req = CompletionRequest::new(vec![assistant, Message::tool_result("call-1", "42")])
            .with_tools(vec![ToolDef::new(
                "search",
                "search the vault",
                json!({ "type": "object" }),
            )]);

        let body = to_openai_request("deepseek-chat", &req);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "search");
        // The assistant's tool call is serialized with stringified arguments (OpenAI shape).
        assert_eq!(body["messages"][0]["tool_calls"][0]["id"], "call-1");
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["arguments"],
            "{\"x\":1}"
        );
        // The tool-result message carries its tool_call_id.
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "call-1");
    }

    #[test]
    fn response_parses_text_and_usage() {
        let v = json!({
            "choices": [{
                "message": { "role": "assistant", "content": "pong" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        });
        let resp = from_openai_response(&v).unwrap();
        assert_eq!(resp.content.as_deref(), Some("pong"));
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.unwrap().total_tokens, 12);
    }

    #[test]
    fn response_parses_tool_calls() {
        let v = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": { "name": "add", "arguments": "{\"x\":1}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let resp = from_openai_response(&v).unwrap();
        assert!(resp.content.is_none());
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "add");
        assert_eq!(resp.tool_calls[0].arguments["x"], 1);
    }

    #[test]
    fn empty_choices_is_empty_response() {
        let v = json!({ "choices": [] });
        assert!(matches!(
            from_openai_response(&v),
            Err(ProviderError::EmptyResponse)
        ));
    }

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
