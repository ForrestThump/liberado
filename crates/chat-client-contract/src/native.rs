//! Native (non-WASM) chat client trait and SSE framing decoder.
//!
//! Gated behind `#[cfg(not(target_arch = "wasm32"))]` in `lib.rs` so the WebUI can
//! depend on `chat-client-contract` without pulling in `tokio`/`futures`/`async-trait`.

use async_trait::async_trait;
use std::pin::Pin;
use ulid::Ulid;

use crate::wire::{ChatError, ChatEvent, ChatResponse};

/// A chat client trait — implemented by HTTP/SSE clients that talk to `liberado serve`.
#[async_trait]
pub trait ChatClient {
    /// Send a message non-streaming, returning the reply and session id.
    async fn send(&self, message: &str, session: Option<Ulid>) -> Result<ChatResponse, ChatError>;

    /// Send a message and stream events back.
    async fn stream(
        &self,
        message: &str,
        session: Option<Ulid>,
    ) -> Result<
        Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, ChatError>> + Send>>,
        ChatError,
    >;
}
