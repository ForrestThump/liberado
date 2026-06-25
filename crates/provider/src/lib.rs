//! # liberado-provider
//!
//! The provider-agnostic inference interface for Liberado (Decision 13). Every LLM call in the
//! system — main agent, dispatcher, subagents — goes through the [`Provider`] trait, so models
//! and providers are swappable from config and tests can inject a [`MockProvider`] (Decision
//! 16).
//!
//! The crate is intentionally minimal: a normalized request/response vocabulary
//! ([`CompletionRequest`]/[`CompletionResponse`]) supporting the two capabilities the
//! capability floor cares about — **tool-calling** and **structured (JSON) output** — plus the
//! [`complete_json`] helper that turns a structured reply into a typed value. Concrete backends
//! (a thin DeepSeek client, or a rig-backed one) implement [`Provider`] in their own crates;
//! nothing here pulls in an HTTP stack or commits to a framework.

mod error;
mod mock;
mod provider;
mod types;

pub use error::{ProviderError, ProviderResult};
pub use mock::MockProvider;
pub use provider::{CompletionStream, Provider, complete_json};
pub use types::{
    CompletionRequest, CompletionResponse, FinishReason, Message, ResponseFormat, Role, StreamItem,
    ToolDef, ToolInvocation, Usage,
};
