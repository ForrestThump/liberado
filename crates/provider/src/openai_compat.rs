//! Shared translation logic for OpenAI-compatible chat-completions APIs (DeepSeek, OpenRouter, and
//! any future backend that speaks the same wire format). Pure and HTTP-free, matching this crate's
//! own "no HTTP stack" constraint: map a normalized [`CompletionRequest`] to the OpenAI wire shape,
//! map a response body back, and assemble a streamed SSE response's deltas. Concrete backends stay
//! responsible for the actual HTTP round-trip (the POST call, status handling, byte-stream
//! consumption) and only their own `DEFAULT_BASE_URL`/`DEFAULT_MODEL`, env-var names, and
//! status-code mapping quirks (e.g. OpenRouter's extra `402` for insufficient credits).
//!
//! Before this was extracted, `liberado-provider-deepseek` and `liberado-provider-openrouter` had
//! byte-for-byte identical copies of everything in this module (found via `cargo dupes`,
//! `docs/roadmap/hygiene-audit-2026-07-04.md`) — a bug in this translation logic needed fixing
//! twice, and a third OpenAI-compatible backend would have been a third copy.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::{
    CompletionRequest, CompletionResponse, FinishReason, Message, ProviderError, ProviderResult,
    ResponseFormat, Role, ToolDef, ToolInvocation, Usage,
};

/// Bidirectional mapping between original (internal `mcp:tool` convention) and sanitized
/// (API-compatible `[a-zA-Z0-9_-]+`) tool names for a single request.
#[derive(Debug, Clone, Default)]
pub struct ToolNameMap {
    /// original → sanitized (for outgoing serialization).
    forward: HashMap<String, String>,
    /// sanitized → original (for incoming deserialization).
    reverse: HashMap<String, String>,
}

/// Sanitize a single tool name: replace every character outside `[a-zA-Z0-9_-]` with `_`.
pub fn basic_sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Build the per-request [`ToolNameMap`] from a tool catalog. Collisions are resolved by
/// appending `_1`, `_2`, … to the sanitized name, so the `reverse` map is always bijective.
pub fn build_tool_name_map(tools: &[ToolDef]) -> ToolNameMap {
    let mut forward = HashMap::new();
    let mut reverse = HashMap::new();
    let mut used = HashSet::new();

    for tool in tools {
        let base = basic_sanitize(&tool.name);
        let mut sanitized = base.clone();
        let mut suffix = 1u32;
        while !used.insert(sanitized.clone()) {
            sanitized = format!("{base}_{suffix}");
            suffix += 1;
        }
        forward.insert(tool.name.clone(), sanitized.clone());
        reverse.insert(sanitized, tool.name.clone());
    }

    ToolNameMap { forward, reverse }
}

/// Map an OpenAI `finish_reason` string to the normalized enum.
pub fn map_finish_reason(s: &str) -> FinishReason {
    match s {
        "tool_calls" => FinishReason::ToolCalls,
        "length" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

/// Partial tool call assembled across streamed deltas.
#[derive(Default)]
pub struct ToolAcc {
    id: String,
    name: String,
    arguments: String,
}

impl ToolAcc {
    pub fn into_invocation(self, name_map: &ToolNameMap) -> Option<ToolInvocation> {
        if self.name.is_empty() {
            return None;
        }
        let original = name_map
            .reverse
            .get(&self.name)
            .cloned()
            .unwrap_or(self.name);
        let arguments = serde_json::from_str(&self.arguments).unwrap_or_else(|_| json!({}));
        Some(ToolInvocation::new(self.id, original, arguments))
    }
}

/// Fold a chunk's `tool_calls` deltas into the accumulator, keyed by `index`.
pub fn accumulate_tool_deltas(acc: &mut Vec<ToolAcc>, deltas: &[Value]) {
    for d in deltas {
        let idx = d["index"].as_u64().unwrap_or(0) as usize;
        while acc.len() <= idx {
            acc.push(ToolAcc::default());
        }
        let slot = &mut acc[idx];
        if let Some(id) = d["id"].as_str()
            && !id.is_empty()
        {
            slot.id = id.to_string();
        }
        let func = &d["function"];
        if let Some(name) = func["name"].as_str()
            && !name.is_empty()
        {
            slot.name = name.to_string();
        }
        if let Some(args) = func["arguments"].as_str() {
            slot.arguments.push_str(args);
        }
    }
}

/// Translate a normalized request into the OpenAI chat-completions request body.
pub fn to_openai_request(model: &str, req: &CompletionRequest, name_map: &ToolNameMap) -> Value {
    let mut body = json!({
        "model": model,
        "messages": req.messages.iter().map(|m| message_to_json(m, name_map)).collect::<Vec<_>>(),
    });

    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| tool_to_json(t, name_map))
                .collect(),
        );
    }
    if matches!(req.response_format, ResponseFormat::Json { .. }) {
        // Most OpenAI-compatible backends support this "type": "json_object" hint; a backend whose
        // routed model doesn't honor it ignores or rejects it — picking a capable model is the
        // caller's job (Decision 13's role-tiered model floors).
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

fn message_to_json(m: &Message, name_map: &ToolNameMap) -> Value {
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
                    let sanitized = name_map
                        .forward
                        .get(&tc.name)
                        .cloned()
                        .unwrap_or_else(|| basic_sanitize(&tc.name));
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": { "name": sanitized, "arguments": tc.arguments.to_string() },
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

fn tool_to_json(t: &ToolDef, name_map: &ToolNameMap) -> Value {
    let sanitized = name_map
        .forward
        .get(&t.name)
        .cloned()
        .unwrap_or_else(|| basic_sanitize(&t.name));
    json!({
        "type": "function",
        "function": {
            "name": sanitized,
            "description": t.description,
            "parameters": t.parameters,
        },
    })
}

/// Translate an OpenAI chat-completions response body into a normalized [`CompletionResponse`].
pub fn from_openai_response(v: &Value, name_map: &ToolNameMap) -> ProviderResult<CompletionResponse> {
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
                    let parsed = parse_tool_call(tc, name_map);
                    if parsed.is_none() {
                        tracing::warn!(tool_call = %tc, "dropping unparseable tool call");
                    }
                    parsed
                })
                .collect()
        })
        .unwrap_or_default();

    let finish_reason = choice["finish_reason"]
        .as_str()
        .map(map_finish_reason)
        .unwrap_or(FinishReason::Stop);

    Ok(CompletionResponse {
        content,
        tool_calls,
        finish_reason,
        usage: parse_usage(&v["usage"]),
    })
}

fn parse_tool_call(tc: &Value, name_map: &ToolNameMap) -> Option<ToolInvocation> {
    let id = tc["id"].as_str()?.to_string();
    let function = &tc["function"];
    let raw_name = function["name"].as_str()?.to_string();
    // OpenAI-compatible backends encode arguments as a JSON *string*; parse it back to a value
    // (empty on failure).
    let arguments = function["arguments"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!({}));
    let original = name_map.reverse.get(&raw_name).cloned().unwrap_or(raw_name);
    Some(ToolInvocation::new(id, original, arguments))
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

    fn empty_name_map() -> ToolNameMap {
        ToolNameMap::default()
    }

    #[test]
    fn basic_sanitize_replaces_invalid_chars() {
        assert_eq!(basic_sanitize("vault:read"), "vault_read");
        assert_eq!(basic_sanitize("my.tool"), "my_tool");
        assert_eq!(basic_sanitize("path/to/tool"), "path_to_tool");
        assert_eq!(basic_sanitize("valid_name-123"), "valid_name-123");
        assert_eq!(basic_sanitize(""), "");
    }

    #[test]
    fn build_map_preserves_colon_names() {
        let tools = vec![
            ToolDef::new("vault:read", "", json!({})),
            ToolDef::new("vault:write", "", json!({})),
        ];
        let map = build_tool_name_map(&tools);
        assert_eq!(map.forward.get("vault:read").unwrap(), "vault_read");
        assert_eq!(map.forward.get("vault:write").unwrap(), "vault_write");
        assert_eq!(map.reverse.get("vault_read").unwrap(), "vault:read");
        assert_eq!(map.reverse.get("vault_write").unwrap(), "vault:write");
    }

    #[test]
    fn build_map_handles_collisions() {
        let tools = vec![
            ToolDef::new("vault:read", "", json!({})),
            ToolDef::new("vault_read", "", json!({})),
        ];
        let map = build_tool_name_map(&tools);
        // Both sanitize to "vault_read" base; second gets a suffix.
        let first = map.forward.get("vault:read").unwrap();
        let second = map.forward.get("vault_read").unwrap();
        assert!(first == "vault_read" || first == "vault_read_1");
        assert!(second == "vault_read_1" || second == "vault_read");
        assert_ne!(first, second);
    }

    #[test]
    fn request_maps_messages_temperature_and_json_mode() {
        let req = CompletionRequest::new(vec![Message::system("be terse"), Message::user("hi")])
            .with_temperature(0.0)
            .with_max_tokens(64)
            .with_json_schema(json!({ "type": "object" }));

        let body = to_openai_request("test-model", &req, &empty_name_map());
        assert_eq!(body["model"], "test-model");
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

        let name_map = build_tool_name_map(&req.tools);
        let body = to_openai_request("test-model", &req, &name_map);
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
    fn request_sanitizes_colon_tool_names() {
        let req = CompletionRequest::new(vec![Message::user("hello")])
            .with_tools(vec![ToolDef::new("mcp:tool", "desc", json!({}))]);

        let name_map = build_tool_name_map(&req.tools);
        let body = to_openai_request("test-model", &req, &name_map);
        assert_eq!(body["tools"][0]["function"]["name"], "mcp_tool");
    }

    #[test]
    fn request_sanitizes_tool_call_names_in_messages() {
        let assistant = Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolInvocation::new("c1", "mcp:tool", json!({}))],
            tool_call_id: None,
        };
        let req = CompletionRequest::new(vec![assistant, Message::tool_result("c1", "ok")])
            .with_tools(vec![ToolDef::new("mcp:tool", "desc", json!({}))]);

        let name_map = build_tool_name_map(&req.tools);
        let body = to_openai_request("test-model", &req, &name_map);
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["name"],
            "mcp_tool"
        );
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
        let resp = from_openai_response(&v, &empty_name_map()).unwrap();
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
        let resp = from_openai_response(&v, &empty_name_map()).unwrap();
        assert!(resp.content.is_none());
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "add");
        assert_eq!(resp.tool_calls[0].arguments["x"], 1);
    }

    #[test]
    fn response_reverse_maps_sanitized_tool_names() {
        let tools = vec![
            ToolDef::new("vault:read", "", json!({})),
            ToolDef::new("vault:write", "", json!({})),
        ];
        let name_map = build_tool_name_map(&tools);

        let v = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": { "name": "vault_read", "arguments": "{}" }
                    }, {
                        "id": "call-2",
                        "type": "function",
                        "function": { "name": "vault_write", "arguments": "{}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let resp = from_openai_response(&v, &name_map).unwrap();
        assert_eq!(resp.tool_calls.len(), 2);
        assert_eq!(resp.tool_calls[0].name, "vault:read");
        assert_eq!(resp.tool_calls[1].name, "vault:write");
    }

    #[test]
    fn empty_choices_is_empty_response() {
        let v = json!({ "choices": [] });
        assert!(matches!(
            from_openai_response(&v, &empty_name_map()),
            Err(ProviderError::EmptyResponse)
        ));
    }
}
