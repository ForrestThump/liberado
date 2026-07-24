//! # liberado-main-agent
//!
//! The conversational **human interface** the user talks to. By default (config
//! `topology.main_agent.delegation_mode = true`) it is a face agent: it holds the human's intent,
//! asks clarifying questions, and calls a single built-in [`face::DELEGATE_TOOL_NAME`] tool that
//! hands goals to the dispatcher/orchestrator — so tool schemas and raw tool results never
//! pollute chat context. Operators can still grant extra MCPs to the `"main-agent"` policy
//! component if they want a thicker surface.
//!
//! Delegate handoffs write **dispatch journals** under `<LIBERADO_DATA_DIR>/dispatches/` (linked from
//! the `delegate` tool result footer by correlation id + parent chat session). Not model context.
//!
//! [`Conversation`] is the in-memory primitive — one history, no I/O. Durability and per-session
//! routing layer on top via [`ChatSessions`]. Long histories are compacted at the turn boundary
//! ([`CompactionConfig`], module [`compaction`]): older turns are rolled into a persisted summary
//! marker so the model-visible context stays under the context window.

use liberado_executor::{AgentEvent, ExecError, Executor, ToolRuntime};
use liberado_provider::Message;
use tokio::sync::mpsc::Sender;

mod compaction;
mod dispatch_journal;
mod face;
mod sessions;

pub use compaction::{
    COMPACTION_AUTHOR, CompactionConfig, SUMMARY_HEADER, estimate_tokens,
};
pub use dispatch_journal::{dispatches_dir, journal_path};
pub use face::{DELEGATE_TOOL_NAME, DispatchBridge, FaceRuntime};
pub use sessions::{ChatSessions, SessionError, SessionResult, default_conversation_title};

/// Short legacy prompt (used when `delegation_mode = false` and no custom prompt is set).
pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are Liberado, a personal AI assistant with access to the user's tools. Hold a natural, helpful \
conversation. When a tool would help answer or act, use it; otherwise just reply. Be concise and \
direct, and never invent tool results.";

/// Default system prompt when the main agent is a human interfacer (delegation mode).
///
/// Models are trained to pick from in-context tools; this prompt must be strong enough that the
/// agent treats `delegate` as proxy access to Liberado's full capabilities and never assumes it
/// must see tool definitions itself.
pub const HUMAN_INTERFACE_SYSTEM_PROMPT: &str = "\
You are Liberado — the human interface for a personal AI life OS.

# Your role (non-negotiable)

You are a **face agent**, not a tool user. Your job is to:

1. Talk with the human in natural language.
2. Hold and refine **their intent** across the conversation.
3. Ask the **right clarifying questions** when intent is incomplete or ambiguous.
4. When the human wants real-world action, lookup, multi-step work, or any capability beyond pure \
conversation, call the `delegate` tool with a clear, self-contained goal.
5. Relay the results back to the human in plain language (summaries, next steps, questions).

# What you must assume about capabilities

You have **proxy access** to Liberado's full set of capabilities through `delegate` only. Those \
capabilities can include vault/memory (TurboVault-backed), tasks, research, files, external \
services, code work, and more. \
**Do not** try to enumerate tools from your own context. You will usually see only `delegate` (and \
possibly a tiny set of extras the human explicitly enabled). That is intentional.

- If you need something done: **delegate** a well-specified goal.
- If delegate returns clarifying questions: ask the human, then delegate again with the answers.
- If delegate reports a capability is missing: tell the human honestly; the system may need to create \
or wire a tool — still do not invent tool results.
- Never invent tool outputs, file contents, or actions you did not receive via `delegate` (or a \
rare extra tool result).

# What you must NOT do

- Do not claim you lack capability just because you do not see a long tool list.
- Do not dump raw tool JSON or internal system reasoning at the human unless they ask for detail.
- Do not skip clarifying questions when critical details are missing (which account, which file, \
which date, what \"done\" means).
- Do not call `delegate` for pure chit-chat or for questions you can answer from the conversation \
alone.

# Style

Be concise, direct, and collaborative. Prefer short turns that surface intent over long monologues. \
When work is delegated, say so briefly, then present the result clearly.

You are the human's partner for understanding what they want. Delegation is how work gets done.";

/// A multi-turn conversation: the system prompt plus every exchanged message, in order.
pub struct Conversation {
    messages: Vec<Message>,
}

impl Conversation {
    /// Start a conversation with a custom system prompt.
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            messages: vec![Message::system(system_prompt)],
        }
    }

    /// Resume a conversation from an existing, ordered history (system prompt first) — used when
    /// rehydrating from the store. Unlike `new`, it injects no system prompt; the caller supplies
    /// the full history.
    pub fn from_history(messages: Vec<Message>) -> Self {
        Self { messages }
    }

    /// One user turn: append the user's message, drive the executor's conversational loop over the
    /// **full** history (model + tools until it replies in prose), and return the reply. The model's
    /// turns — including any tool calls and their results — are appended to the history, so the next
    /// turn sees everything that happened.
    pub async fn turn(
        &mut self,
        executor: &Executor,
        runtime: &dyn ToolRuntime,
        user: &str,
    ) -> Result<String, ExecError> {
        self.messages.push(Message::user(user));
        executor
            .converse_messages(runtime, &mut self.messages)
            .await
    }

    /// Streaming variant of [`turn`](Self::turn).
    pub async fn turn_stream(
        &mut self,
        executor: &Executor,
        runtime: &dyn ToolRuntime,
        user: &str,
        events: &Sender<AgentEvent>,
    ) -> Result<(), ExecError> {
        let checkpoint = self.messages.len();
        self.messages.push(Message::user(user));

        let mut rollback = Rollback::arm(&mut self.messages, checkpoint);
        let result = executor
            .converse_stream(runtime, rollback.messages(), events)
            .await;
        rollback.disarm();
        result
    }

    /// Append a user message and a plain assistant reply directly, with no executor/tool
    /// involvement — used when a turn is answered outside the conversational tool loop.
    pub fn answer(&mut self, user: &str, reply: &str) {
        self.messages.push(Message::user(user));
        self.messages.push(Message::assistant(reply));
    }

    /// The full history, system prompt first.
    pub fn history(&self) -> &[Message] {
        &self.messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new(DEFAULT_SYSTEM_PROMPT)
    }
}

/// Rolls the message buffer back to `checkpoint` if dropped while still armed (cancellation).
struct Rollback<'a> {
    messages: &'a mut Vec<Message>,
    checkpoint: usize,
    armed: bool,
}

impl<'a> Rollback<'a> {
    fn arm(messages: &'a mut Vec<Message>, checkpoint: usize) -> Self {
        Self {
            messages,
            checkpoint,
            armed: true,
        }
    }

    fn messages(&mut self) -> &mut Vec<Message> {
        self.messages
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Rollback<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.messages.truncate(self.checkpoint);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use liberado_executor::{Budget, ToolRuntime};
    use liberado_provider::{
        CompletionRequest, CompletionResponse, MockProvider, Provider, ProviderResult, Role,
        ToolDef, ToolInvocation,
    };
    use std::sync::Arc;
    use tokio::sync::mpsc;

    struct NoTools;
    #[async_trait]
    impl ToolRuntime for NoTools {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Err("no tools".into())
        }
    }

    #[tokio::test]
    async fn carries_context_across_turns() {
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::text("Hi!"),
                CompletionResponse::text("You said hello."),
            ],
        ));
        let executor = Executor::new(provider.clone(), Budget::default());
        let mut convo = Conversation::new("sys");
        convo.turn(&executor, &NoTools, "hello").await.unwrap();
        convo
            .turn(&executor, &NoTools, "what did I say?")
            .await
            .unwrap();
        let second = &provider.received_requests()[1];
        assert!(second.messages.iter().any(|m| m.content == "hello"));
    }

    #[tokio::test]
    async fn cancelled_stream_turn_rolls_back_to_clean_history() {
        struct PendingProvider;
        #[async_trait]
        impl Provider for PendingProvider {
            fn model(&self) -> String {
                "pending".into()
            }
            async fn complete(
                &self,
                _request: CompletionRequest,
            ) -> ProviderResult<CompletionResponse> {
                std::future::pending().await
            }
        }
        let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
        let mut convo = Conversation::new("sys");
        let (tx, _rx) = mpsc::channel(1);
        let fut = convo.turn_stream(&executor, &NoTools, "hi", &tx);
        drop(fut);
        assert_eq!(convo.history().len(), 1);
        assert_eq!(convo.history()[0].role, Role::System);
    }

    #[tokio::test]
    async fn completed_stream_turn_keeps_its_history() {
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text("ok")],
        ));
        let executor = Executor::new(provider, Budget::default());
        let mut convo = Conversation::new("sys");
        let (tx, _rx) = mpsc::channel(8);
        convo
            .turn_stream(&executor, &NoTools, "hi", &tx)
            .await
            .unwrap();
        assert!(convo.history().len() >= 3);
    }
}
