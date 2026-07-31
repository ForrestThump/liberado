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

use futures::StreamExt;
use serde_json::{Value, json};

use crate::{
    CompletionRequest, CompletionResponse, CompletionStream, FinishReason, Message, ProviderError,
    ProviderResult, ResponseFormat, Role, StreamItem, ToolDef, ToolInvocation, Usage,
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

/// Drive an already-successful (status checked by the caller) OpenAI-compatible SSE response body
/// into a [`CompletionStream`]: parse each `data:` line as a chunk with a `delta`, emit content
/// deltas as [`StreamItem::Token`], accumulate tool-call deltas and the finish reason, then emit the
/// assembled response as the final [`StreamItem::Done`].
///
/// Extracted from `liberado-provider-deepseek`/`liberado-provider-openrouter`, which had this loop
/// duplicated verbatim (`docs/roadmap/hygiene-audit-2026-07-05.md`) — unlike the request/response
/// mapping functions above (already shared before this), the streaming loop is where chunk-boundary
/// bugs actually hide, so it's the part most worth not maintaining twice. Callers own the HTTP POST,
/// status-code check, and building `name_map` (via [`build_tool_name_map`]) — this only owns turning
/// a 200 response's body into normalized stream items.
pub fn stream_sse_response(response: reqwest::Response, name_map: ToolNameMap) -> CompletionStream {
    let stream = async_stream::try_stream! {
        let mut bytes = response.bytes_stream();
        let mut buf = String::new();
        let mut content = String::new();
        let mut tools: Vec<ToolAcc> = Vec::new();
        let mut finish = FinishReason::Stop;
        // Populated by the trailing usage chunk when the request sets `stream_options.include_usage`
        // (that chunk carries `usage` and an empty `choices`); `None` if the backend omits it.
        let mut usage: Option<Usage> = None;

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
                if let Some(u) = parse_usage(&v["usage"]) {
                    usage = Some(u);
                }
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
            usage,
        });
    };

    Box::pin(stream)
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
    if let ResponseFormat::Json { schema } = &req.response_format {
        // **Send the schema.** This used to send only `{"type": "json_object"}`, discarding the
        // schema every caller had gone to the trouble of building — `complete_json` takes one,
        // `CompletionRequest::with_json_schema` stores one, and it stopped here.
        //
        // The difference is not cosmetic. `json_object` asks for *syntactically valid JSON* and
        // nothing more: the shape is left to the model's goodwill, and on a small, fast router model
        // that goodwill is exactly what runs out. `json_schema` with `strict` makes the **backend**
        // constrain decoding, so a non-conforming token cannot be emitted in the first place. That
        // is the same mechanism tool-calling uses, without changing our call shape.
        //
        // Gated on the schema actually *describing* something. A schema with no `properties` —
        // `{"type":"object"}`, which is what `liberado-dispatcher`'s `decision_schema()` still
        // returns — constrains nothing, and sending it under `strict` would be rejected by backends
        // that (correctly) require `properties`/`required` there. So today this changes no request
        // on the wire; it activates the moment a caller writes a real schema.
        //
        // **If you are writing that schema**: `strict` obliges you to satisfy the backend's rules —
        // every property listed in `required`, and `additionalProperties: false` — or the request is
        // refused outright rather than merely under-constrained. Verify against the live backend
        // before relying on it; a refusal is a louder failure than the malformed replies this
        // replaces, and worth catching in staging rather than in a 06:55 cron.
        let constrains_shape = schema
            .get("properties")
            .is_some_and(serde_json::Value::is_object);
        if constrains_shape {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "decision",
                    // Refuse a reply that does not fit rather than silently returning prose we would
                    // fail to parse a layer later.
                    "strict": true,
                    "schema": schema,
                }
            });
        } else {
            body["response_format"] = json!({ "type": "json_object" });
        }
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
pub fn from_openai_response(
    v: &Value,
    name_map: &ToolNameMap,
) -> ProviderResult<CompletionResponse> {
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
    // Cached-prompt accounting, under whichever name the backend uses. DeepSeek reports
    // `prompt_cache_hit_tokens` at the top level; OpenAI (and OpenRouter passing it through) nests
    // `cached_tokens` under `prompt_tokens_details`. Absent entirely means the backend said nothing,
    // which stays `None` rather than collapsing to 0 — "we cannot see" and "nothing was cached" are
    // different answers and only one of them is a problem to fix.
    let cached_prompt_tokens = u
        .get("prompt_cache_hit_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            u.get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64)
        })
        .or_else(|| u.get("cached_tokens").and_then(Value::as_u64))
        .map(|v| v as u32);
    Some(Usage {
        prompt_tokens: field("prompt_tokens"),
        completion_tokens: field("completion_tokens"),
        total_tokens: field("total_tokens"),
        cached_prompt_tokens,
    })
}

/// Map an HTTP error status to a typed [`ProviderError`] (Decision 13: callers branch on these).
/// `extra_client_error_status` folds in a backend's own client-error codes beyond the common set
/// (e.g. OpenRouter's `402` for insufficient account credits) into the same `InvalidRequest`
/// bucket — the last per-backend difference between what used to be two separate `map_status`
/// copies in `liberado-provider-deepseek`/`liberado-provider-openrouter`
/// (`docs/roadmap/hygiene-audit-2026-07-05.md`), now just a data parameter.
pub fn map_status(status: u16, body: String, extra_client_error_status: &[u16]) -> ProviderError {
    match status {
        429 => ProviderError::RateLimited,
        400 | 401 | 403 | 404 | 422 => {
            ProviderError::InvalidRequest(format!("HTTP {status}: {body}"))
        }
        s if extra_client_error_status.contains(&s) => {
            ProviderError::InvalidRequest(format!("HTTP {status}: {body}"))
        }
        _ => ProviderError::Transport(format!("HTTP {status}: {body}")),
    }
}

/// Parse an OpenAI-compatible `GET /models` response body (`{"data": [{"id": "...", ...}, ...]}`)
/// into a plain list of model ids. Entries missing a string `id` are skipped rather than failing
/// the whole parse — a best-effort listing, not the authoritative model catalog.
pub fn parse_models_response(v: &Value) -> Vec<String> {
    v["data"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_name_map() -> ToolNameMap {
        ToolNameMap::default()
    }

    /// Backends disagree on where cached-prompt accounting lives, so read all three shapes. Getting
    /// this wrong is quiet: the number simply never appears and prompt caching stays unmeasurable.
    #[test]
    fn cached_prompt_tokens_are_read_under_each_backend_spelling() {
        // DeepSeek: top-level.
        let deepseek = json!({
            "prompt_tokens": 1000, "completion_tokens": 50, "total_tokens": 1050,
            "prompt_cache_hit_tokens": 900, "prompt_cache_miss_tokens": 100
        });
        assert_eq!(
            parse_usage(&deepseek).unwrap().cached_prompt_tokens,
            Some(900)
        );

        // OpenAI / OpenRouter passthrough: nested under prompt_tokens_details.
        let openai = json!({
            "prompt_tokens": 1000, "completion_tokens": 50, "total_tokens": 1050,
            "prompt_tokens_details": { "cached_tokens": 768 }
        });
        assert_eq!(
            parse_usage(&openai).unwrap().cached_prompt_tokens,
            Some(768)
        );

        // A backend that says nothing stays None — NOT zero. "We cannot see" and "nothing was
        // cached" are different answers, and only the second is a problem to go fix.
        let silent =
            json!({ "prompt_tokens": 1000, "completion_tokens": 50, "total_tokens": 1050 });
        let u = parse_usage(&silent).unwrap();
        assert_eq!(u.cached_prompt_tokens, None);
        assert_eq!(u.cache_hit_rate(), None);
    }

    #[test]
    fn cache_hit_rate_is_a_fraction_of_prompt_tokens() {
        let u = parse_usage(&json!({
            "prompt_tokens": 1000, "completion_tokens": 10, "total_tokens": 1010,
            "prompt_cache_hit_tokens": 750
        }))
        .unwrap();
        assert_eq!(u.cache_hit_rate(), Some(0.75));
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
        // A shapeless schema still asks only for `json_object` — see `to_openai_request`.
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["max_tokens"], 64);
        assert!(body.get("tools").is_none());
    }

    /// A schema that actually describes a shape must reach the wire.
    ///
    /// It used not to: every caller's schema was dropped and replaced with `{"type":"json_object"}`,
    /// which asks for valid JSON and says nothing about the shape. That left a small, fast router
    /// model's output shape resting entirely on prompt text.
    #[test]
    fn a_schema_that_constrains_shape_is_sent_as_json_schema() {
        let schema = json!({
            "type": "object",
            "properties": { "action": { "type": "string" } },
            "required": ["action"],
            "additionalProperties": false,
        });
        let req =
            CompletionRequest::new(vec![Message::user("hi")]).with_json_schema(schema.clone());
        let body = to_openai_request("test-model", &req, &empty_name_map());
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(body["response_format"]["json_schema"]["schema"], schema);
    }

    #[cfg(test)]
    mod wire_body_seam_tests {
        use super::*;
        use crate::{CompletionRequest, Message, ResponseFormat, Role, ToolDef};

        fn req() -> CompletionRequest {
            CompletionRequest::new(vec![Message {
                role: Role::User,
                content: "hello".into(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }])
        }

        #[test]
        fn model_present() {
            let body = to_openai_request("gpt-4", &req(), &ToolNameMap::default());
            assert_eq!(body["model"], "gpt-4");
        }

        #[test]
        fn messages_present() {
            let body = to_openai_request("m", &req(), &ToolNameMap::default());
            assert_eq!(body["messages"][0]["role"], "user");
            assert_eq!(body["messages"][0]["content"], "hello");
        }

        #[test]
        fn tools_present_when_non_empty() {
            let mut r = req();
            r.tools = vec![ToolDef {
                name: "vault:read".into(),
                description: "read".into(),
                parameters: serde_json::json!({}),
            }];
            let nm = build_tool_name_map(&r.tools);
            let body = to_openai_request("m", &r, &nm);
            assert!(body["tools"].is_array());
            assert!(
                body["tools"][0]["function"]["name"]
                    .as_str()
                    .unwrap()
                    .contains("vault")
            );
        }

        #[test]
        fn tools_absent_when_empty() {
            let body = to_openai_request("m", &req(), &ToolNameMap::default());
            assert!(body.get("tools").is_none());
        }

        #[test]
        fn temperature_present_when_set() {
            let mut r = req();
            r.temperature = Some(0.3);
            let body = to_openai_request("m", &r, &ToolNameMap::default());
            let temp = body["temperature"].as_f64().unwrap();
            assert!((temp - 0.3).abs() < 0.01);
        }

        #[test]
        fn temperature_absent_when_none() {
            assert!(
                to_openai_request("m", &req(), &ToolNameMap::default())
                    .get("temperature")
                    .is_none()
            );
        }

        #[test]
        fn max_tokens_present_when_set() {
            let mut r = req();
            r.max_tokens = Some(4096);
            let body = to_openai_request("m", &r, &ToolNameMap::default());
            assert_eq!(body["max_tokens"], 4096);
        }

        #[test]
        fn max_tokens_absent_when_none() {
            assert!(
                to_openai_request("m", &req(), &ToolNameMap::default())
                    .get("max_tokens")
                    .is_none()
            );
        }

        #[test]
        fn json_schema_when_constraining() {
            let mut r = req();
            r.response_format = ResponseFormat::Json {
                schema: serde_json::json!({"type":"object","properties":{"x":{"type":"string"}}}),
            };
            let body = to_openai_request("m", &r, &ToolNameMap::default());
            assert_eq!(body["response_format"]["type"], "json_schema");
            assert_eq!(body["response_format"]["json_schema"]["strict"], true);
            assert!(
                body["response_format"]["json_schema"]["schema"]["properties"]["x"]["type"]
                    == "string"
            );
        }

        #[test]
        fn json_object_when_empty_schema() {
            let mut r = req();
            r.response_format = ResponseFormat::Json {
                schema: serde_json::json!({"type":"object"}),
            };
            let body = to_openai_request("m", &r, &ToolNameMap::default());
            assert_eq!(body["response_format"]["type"], "json_object");
        }
    }

    /// ...and a shapeless one must not, or a backend enforcing `strict` refuses the request outright
    /// — a louder failure than the under-constrained reply it would be replacing.
    #[test]
    fn a_shapeless_schema_falls_back_to_json_object() {
        for shapeless in [json!({ "type": "object" }), json!({}), json!(null)] {
            let req = CompletionRequest::new(vec![Message::user("hi")]).with_json_schema(shapeless);
            let body = to_openai_request("test-model", &req, &empty_name_map());
            assert_eq!(body["response_format"]["type"], "json_object");
        }
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

    #[test]
    fn status_mapping_common_cases() {
        assert!(matches!(
            map_status(429, "x".into(), &[]),
            ProviderError::RateLimited
        ));
        assert!(matches!(
            map_status(401, "x".into(), &[]),
            ProviderError::InvalidRequest(_)
        ));
        assert!(matches!(
            map_status(500, "x".into(), &[]),
            ProviderError::Transport(_)
        ));
    }

    #[test]
    fn status_mapping_extra_client_error_status_is_invalid_request() {
        assert!(matches!(
            map_status(402, "insufficient credits".into(), &[402]),
            ProviderError::InvalidRequest(_)
        ));
        // Without it declared, the same code falls through to Transport.
        assert!(matches!(
            map_status(402, "x".into(), &[]),
            ProviderError::Transport(_)
        ));
    }

    #[test]
    fn parse_models_response_extracts_ids() {
        let v = json!({
            "data": [
                { "id": "deepseek-chat", "object": "model" },
                { "id": "deepseek-reasoner" },
                { "object": "model" } // missing id: skipped, not a hard error
            ]
        });
        assert_eq!(
            parse_models_response(&v),
            vec!["deepseek-chat".to_string(), "deepseek-reasoner".to_string()]
        );
    }

    #[test]
    fn parse_models_response_missing_data_is_empty() {
        assert_eq!(parse_models_response(&json!({})), Vec::<String>::new());
    }

    #[test]
    fn map_finish_reason_recognizes_length() {
        assert_eq!(map_finish_reason("length"), FinishReason::Length);
    }

    #[test]
    fn map_finish_reason_recognizes_content_filter() {
        assert_eq!(
            map_finish_reason("content_filter"),
            FinishReason::ContentFilter
        );
    }

    #[test]
    fn into_invocation_returns_none_for_empty_name() {
        let acc = ToolAcc::default();
        let map = ToolNameMap::default();
        assert!(acc.into_invocation(&map).is_none());
    }

    #[test]
    fn into_invocation_maps_name_through_reverse_map() {
        let tools = vec![ToolDef::new("original:a", "", json!({}))];
        let map = build_tool_name_map(&tools);
        let acc = ToolAcc {
            id: "call-1".into(),
            name: "original_a".into(),
            arguments: r#"{"x":1}"#.into(),
        };
        let inv = acc.into_invocation(&map).unwrap();
        assert_eq!(inv.name, "original:a");
        assert_eq!(inv.arguments["x"], 1);
    }

    #[test]
    fn into_invocation_uses_raw_name_when_not_in_map() {
        let map = ToolNameMap::default();
        let acc = ToolAcc {
            id: "c2".into(),
            name: "unmapped_name".into(),
            arguments: "{}".into(),
        };
        let inv = acc.into_invocation(&map).unwrap();
        assert_eq!(inv.name, "unmapped_name");
    }

    #[test]
    fn accumulate_tool_deltas_expands_slots_and_sets_fields() {
        let mut acc = Vec::new();
        let deltas: Vec<Value> = serde_json::from_str(
            r#"[{"index":0,"id":"c1","function":{"name":"search","arguments":"{\"q\":"}}]"#,
        )
        .unwrap();
        accumulate_tool_deltas(&mut acc, &deltas);
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0].id, "c1");
        assert_eq!(acc[0].name, "search");
        assert_eq!(acc[0].arguments, r#"{"q":"#);
    }

    #[test]
    fn accumulate_tool_deltas_does_not_overwrite_with_empty_id_or_name() {
        let mut acc = vec![ToolAcc {
            id: "existing".into(),
            name: "existing".into(),
            arguments: String::new(),
        }];
        let deltas: Vec<Value> = serde_json::from_str(
            r#"[{"index":0,"id":"","function":{"name":"","arguments":"more"}}]"#,
        )
        .unwrap();
        accumulate_tool_deltas(&mut acc, &deltas);
        assert_eq!(acc[0].id, "existing", "should not overwrite with empty id");
        assert_eq!(
            acc[0].name, "existing",
            "should not overwrite with empty name"
        );
        assert_eq!(acc[0].arguments, "more", "arguments should still append");
    }
}
