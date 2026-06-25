//! # liberado-main-agent
//!
//! The conversational front the user talks to. A [`Conversation`] holds the running message history
//! (system prompt + every prior turn) and, on each user message, drives the
//! [`Executor`](liberado_executor::Executor)'s conversational tool-calling loop over that whole
//! history — so context carries forward and the agent can use tools mid-answer — then returns the
//! prose reply.
//!
//! [`Conversation`] is the in-memory primitive — one history, no I/O. Durability and per-session
//! routing layer on top via [`ChatSessions`], which backs chat with a
//! [`ConversationStore`](liberado_conversation_store::ConversationStore): each turn rehydrates from
//! the store and persists its tail on success, so a host (the web server today, a TUI-hosting daemon
//! later) stays a thin, stateless adapter. No ContextPolicy header yet — that still layers on top.

use liberado_executor::{AgentEvent, ExecError, Executor, ToolRuntime};
use liberado_provider::Message;
use tokio::sync::mpsc::Sender;

mod sessions;
pub use sessions::{ChatSessions, SessionError, SessionResult};

/// The default persona/system prompt for the chat agent.
pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are Liberado, a personal AI assistant with access to the user's tools. Hold a natural, helpful \
conversation. When a tool would help answer or act, use it; otherwise just reply. Be concise and \
direct, and never invent tool results.";

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

    /// Streaming variant of [`turn`](Self::turn): append the user message and drive the executor's
    /// streaming loop over the full history, emitting [`AgentEvent`]s (answer tokens, tool starts)
    /// over `events` as they happen. The caller sends the terminal `Done`/`Error`.
    ///
    /// **Atomic under cancellation.** If this future is dropped before it completes — the client
    /// closed the stream (a "stop"), or the connection dropped — the turn's partial history is
    /// rolled back to before the user message. So a cancelled turn is a clean no-op: no orphan user
    /// message and, crucially, no assistant `tool_calls` left without their results (which would
    /// make the *next* turn's provider request invalid). On normal completion (`Ok` or `Err`) the
    /// turn's messages are kept.
    pub async fn turn_stream(
        &mut self,
        executor: &Executor,
        runtime: &dyn ToolRuntime,
        user: &str,
        events: &Sender<AgentEvent>,
    ) -> Result<(), ExecError> {
        let checkpoint = self.messages.len();
        self.messages.push(Message::user(user));

        // Armed until the turn completes; if dropped first (cancelled), it undoes the partial turn.
        let mut rollback = Rollback::arm(&mut self.messages, checkpoint);
        let result = executor
            .converse_stream(runtime, rollback.messages(), events)
            .await;
        rollback.disarm();
        result
    }

    /// The full message history (system prompt first).
    pub fn history(&self) -> &[Message] {
        &self.messages
    }

    /// Number of messages, including the system prompt.
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

/// Truncates a message history back to a checkpoint when dropped — unless [`disarm`](Self::disarm)ed
/// first. This makes a streaming turn atomic: hand the history to the turn through
/// [`messages`](Self::messages), and if the turn future is dropped mid-flight (cancellation) the
/// guard's `Drop` undoes whatever the turn appended, leaving the conversation as it was.
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

    /// The guarded history, to drive the turn over.
    fn messages(&mut self) -> &mut Vec<Message> {
        self.messages
    }

    /// The turn completed; keep its messages.
    fn disarm(mut self) {
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
    use liberado_executor::Budget;
    use liberado_provider::{
        CompletionRequest, CompletionResponse, MockProvider, Provider, ProviderResult, Role,
        ToolDef, ToolInvocation,
    };
    use std::sync::Arc;

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

    /// A provider whose completion never resolves — it lets a turn get *started* (the user message
    /// pushed, the request issued) and then hang, so a test can cancel it mid-flight by dropping the
    /// turn future.
    struct PendingProvider;
    #[async_trait]
    impl Provider for PendingProvider {
        fn model(&self) -> &str {
            "pending"
        }
        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> ProviderResult<CompletionResponse> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn carries_context_across_turns() {
        // Two scripted plain-prose replies — one per user turn.
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::text("Hi! How can I help?"),
                CompletionResponse::text("You said hello a moment ago."),
            ],
        ));
        let executor = Executor::new(provider.clone(), Budget::default());
        let mut convo = Conversation::default();

        let r1 = convo.turn(&executor, &NoTools, "hello").await.unwrap();
        assert_eq!(r1, "Hi! How can I help?");

        let r2 = convo
            .turn(&executor, &NoTools, "what did I just say?")
            .await
            .unwrap();
        assert_eq!(r2, "You said hello a moment ago.");

        // The history accumulated: system + (user, assistant) x2.
        assert_eq!(convo.len(), 5);
        assert_eq!(convo.history()[0].role, Role::System);
        // The second request the provider saw must have included the first exchange (context).
        let second_request = &provider.received_requests()[1];
        assert!(
            second_request.messages.iter().any(|m| m.content == "hello"),
            "second turn lost the first user message"
        );
    }

    #[tokio::test]
    async fn cancelled_stream_turn_rolls_back_to_clean_history() {
        let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
        let mut convo = Conversation::default();
        let before = convo.len(); // just the system prompt
        let (tx, _rx) = tokio::sync::mpsc::channel(8);

        // Start the turn and poll it once — enough to push the user message and issue the (hanging)
        // request — then drop the future to simulate the client stopping mid-turn.
        {
            let fut = convo.turn_stream(&executor, &NoTools, "do a thing", &tx);
            tokio::pin!(fut);
            assert!(
                futures::poll!(fut.as_mut()).is_pending(),
                "the pending provider should leave the turn in flight"
            );
        } // fut dropped here → the rollback guard fires

        // The cancelled turn left no trace: not even the user message survives.
        assert_eq!(
            convo.len(),
            before,
            "a cancelled turn must roll back its history"
        );
    }

    #[tokio::test]
    async fn completed_stream_turn_keeps_its_history() {
        let executor = Executor::new(
            Arc::new(MockProvider::with_script(
                "mock",
                [CompletionResponse::text("done")],
            )),
            Budget::default(),
        );
        let mut convo = Conversation::default();
        let before = convo.len();
        let (tx, _rx) = tokio::sync::mpsc::channel(8);

        convo
            .turn_stream(&executor, &NoTools, "hi", &tx)
            .await
            .unwrap();

        // A turn that runs to completion is *not* rolled back: user + assistant are retained.
        assert_eq!(convo.len(), before + 2);
    }
}
