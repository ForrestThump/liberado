//! Pack-neutral tool extensions: how a composition root adds tools to a coding run.
//!
//! The coding pack builds its own `ToolRuntime` (sandboxed file/command/git tools) deep
//! inside [`crate::LiberadoLoopBackend`], and the request type that reaches the backend is
//! wire-shaped — serialized, cloned, compared. A trait object cannot ride along, so roots
//! that need run-specific tools attach them here instead: an extension hangs off the
//! backend instance and contributes catalog entries plus invoke handling. The worker uses
//! this for `ask_delegator`; nothing else in the pack knows delegation exists.

use async_trait::async_trait;
use liberado_provider::{ToolDef, ToolInvocation};

/// One extra capability offered to the model for a run.
///
/// Extensions are consulted *before* the base runtime: the first one claiming a call wins,
/// and unclaimed calls fall through untouched. Catalog entries are appended after the
/// base's, so base tools keep their positions in prompt rendering.
#[async_trait]
pub trait RuntimeExtension: Send + Sync {
    /// Tools to offer in addition to the base runtime's catalog.
    fn tools(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    /// Handle one model-requested call. `None` means "not mine" — the base runtime
    /// answers it. Returning `Err(text)` surfaces in-band as the tool result, exactly
    /// like a base-tool failure, so the model can adapt.
    async fn invoke(&self, call: &ToolInvocation) -> Option<Result<String, String>> {
        let _ = call;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Noop;

    #[async_trait]
    impl RuntimeExtension for Noop {}

    struct Echo;

    #[async_trait]
    impl RuntimeExtension for Echo {
        fn tools(&self) -> Vec<ToolDef> {
            vec![ToolDef::new("echo", "repeat", json!({"type": "object"}))]
        }

        async fn invoke(&self, call: &ToolInvocation) -> Option<Result<String, String>> {
            match call.name.as_str() {
                "echo" => Some(Ok(format!("echo: {}", call.arguments))),
                _ => None,
            }
        }
    }

    /// Defaults are inert: no tools, every call falls through.
    #[tokio::test]
    async fn default_extension_offers_nothing_and_claims_nothing() {
        let ext = Noop;
        assert!(ext.tools().is_empty());
        let call = ToolInvocation::new("1", "read_file", json!({}));
        assert!(ext.invoke(&call).await.is_none());
    }

    #[tokio::test]
    async fn extension_answers_its_own_tool_and_only_its_own() {
        let ext = Echo;
        assert_eq!(ext.tools().len(), 1);
        let claimed = ToolInvocation::new("1", "echo", json!({"x": 1}));
        assert_eq!(
            ext.invoke(&claimed).await.unwrap().unwrap(),
            "echo: {\"x\":1}"
        );
        let foreign = ToolInvocation::new("2", "read_file", json!({}));
        assert!(ext.invoke(&foreign).await.is_none());
    }
}
