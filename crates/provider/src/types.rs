//! Request/response types for the [`Provider`](crate::Provider) interface.
//!
//! A deliberately small, normalized, chat-completions-shaped vocabulary that maps cleanly onto
//! DeepSeek/OpenAI-style APIs while staying provider-agnostic. It is the *narrow waist* every
//! inference call passes through, so the concrete provider (and a mock) only ever translate to
//! and from these types.

use serde::{Deserialize, Serialize};

/// The author of a [`Message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    /// A tool result fed back to the model (paired with [`Message::tool_call_id`]).
    Tool,
}

/// One message in a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool calls the assistant requested (only on `Assistant` messages).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolInvocation>,
    /// For a `Tool` message, the id of the [`ToolInvocation`] this result answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::plain(Role::System, content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::plain(Role::User, content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::plain(Role::Assistant, content)
    }
    /// A tool-result message answering the invocation with id `tool_call_id`.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// A tool the model may call: name + description + JSON-Schema parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub parameters: serde_json::Value,
}

impl ToolDef {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// How the model should shape its reply. `Json` is the structured-output mode the dispatcher
/// requires (Decision 13).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    #[default]
    Text,
    /// Constrain the reply to JSON conforming to `schema`.
    Json { schema: serde_json::Value },
}

/// A single completion request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    #[serde(default)]
    pub response_format: ResponseFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// The model to run **this call** on. `None` = whatever the provider is configured with.
    ///
    /// Per-request rather than per-provider because for an OpenAI-compatible backend the model is
    /// just a body field, and a session profile choosing a model must not mutate a provider shared
    /// by every other session. Takes precedence over the provider's own model, including one
    /// hot-swapped via `set_model`: naming a model in a profile is an explicit choice and should
    /// beat the daemon default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl CompletionRequest {
    /// A request over `messages` with no tools and text output.
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            response_format: ResponseFormat::Text,
            temperature: None,
            max_tokens: None,
            model: None,
        }
    }

    /// Offer the model a tool catalog.
    pub fn with_tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = tools;
        self
    }

    /// Run this call on `model` instead of the provider's configured one. `None` leaves it alone.
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// Request structured JSON output conforming to `schema`.
    pub fn with_json_schema(mut self, schema: serde_json::Value) -> Self {
        self.response_format = ResponseFormat::Json { schema };
        self
    }

    /// Whether this request asks the backend to constrain decoding to a **shape**, as opposed to
    /// merely to valid JSON.
    ///
    /// The distinction the wire mapping draws: a schema with no `properties` describes nothing and is
    /// sent as `json_object`. Only a shape-constraining schema is worth falling back *from* when a
    /// backend rejects it — see `complete_json`.
    pub fn has_json_schema(&self) -> bool {
        match &self.response_format {
            ResponseFormat::Json { schema } => schema
                .get("properties")
                .is_some_and(serde_json::Value::is_object),
            ResponseFormat::Text => false,
        }
    }

    /// Drop the schema, keeping the request in plain JSON mode.
    ///
    /// The degraded retry for a backend that will not accept a `json_schema` response format. Keeps
    /// the JSON *hint* rather than clearing the format entirely, because the reply still has to
    /// deserialize — the model is simply back to being shaped by the prompt alone.
    pub fn without_json_schema(mut self) -> Self {
        if matches!(self.response_format, ResponseFormat::Json { .. }) {
            self.response_format = ResponseFormat::Json {
                schema: serde_json::json!({}),
            };
        }
        self
    }

    /// Set the sampling temperature (the dispatcher pins this to 0 for determinism).
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

/// A tool call the model emitted (or, on a `Message`, requested).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInvocation {
    /// Provider-assigned id used to correlate the eventual tool result.
    pub id: String,
    pub name: String,
    /// Parsed arguments object.
    pub arguments: serde_json::Value,
}

impl ToolInvocation {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Natural stop.
    Stop,
    /// Stopped to emit tool calls.
    ToolCalls,
    /// Hit the token limit.
    Length,
    /// Stopped by a content filter.
    ContentFilter,
}

/// Token accounting for a completion (when the provider reports it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// Prompt tokens served from the provider's cache, when it says so.
    ///
    /// Prompt caching is the largest unclaimed cost lever here: a `depth=deep` subagent resends the
    /// same system prompt and the same MCP tool schemas on all 30 turns, which is the most cacheable
    /// shape we produce. But whether any of it is *already* being cached was unknowable — this
    /// struct carried three totals and nothing else, so a cache hit and a cache miss looked
    /// identical from the outside.
    ///
    /// Providers disagree on both mechanism and field name. DeepSeek and OpenAI cache a stable
    /// prefix automatically and report it (`prompt_cache_hit_tokens`, `cached_tokens`); Anthropic
    /// requires explicit `cache_control` breakpoints. So this is measurement, not control: read
    /// whatever the backend volunteers, then decide whether anything needs doing. `None` means the
    /// provider said nothing, which is not the same as zero — see [`cache_hit_rate`](Self::cache_hit_rate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_prompt_tokens: Option<u32>,
}

impl Usage {
    /// Fraction of prompt tokens served from cache, or `None` when the provider reported nothing.
    ///
    /// Deliberately distinguishes "no cache data" from "0% hit rate": the first means we cannot see,
    /// the second means we looked and it is missing. Conflating them is how an unclaimed cost lever
    /// stays invisible.
    pub fn cache_hit_rate(&self) -> Option<f32> {
        let cached = self.cached_prompt_tokens?;
        if self.prompt_tokens == 0 {
            return None;
        }
        Some(cached as f32 / self.prompt_tokens as f32)
    }
}

/// A normalized completion response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// Text content, if any (absent when the turn is purely tool calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool calls the model requested this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolInvocation>,
    pub finish_reason: FinishReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

impl CompletionResponse {
    /// A plain text response (`finish_reason = Stop`). Handy for mocks/tests.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: Some(content.into()),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    /// A tool-calling response (`finish_reason = ToolCalls`). Handy for mocks/tests.
    pub fn tool_calls(calls: Vec<ToolInvocation>) -> Self {
        Self {
            content: None,
            tool_calls: calls,
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }
}

/// One item from a streamed completion ([`Provider::complete_stream`](crate::Provider::complete_stream)).
/// Tokens arrive first (incremental text), then exactly one `Done` carries the fully assembled
/// response (content + tool calls + finish reason) so the caller's loop can act on it.
#[derive(Debug, Clone)]
pub enum StreamItem {
    /// An incremental text delta of the assistant's content.
    Token(String),
    /// The turn is complete — the assembled response.
    Done(CompletionResponse),
}
