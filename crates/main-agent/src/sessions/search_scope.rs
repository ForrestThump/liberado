//! Stamp the current conversation onto `search_conversations` when the model
//! asks for this chat's project.
//!
//! The chat-search process does not know which chat invoked it. `scope: "project"`
//! with no conversation id is filled with this turn's session id before the call
//! leaves the daemon. An explicit `project` or `conversation` is left alone.

use async_trait::async_trait;
use liberado_common::bare_tool_name;
use liberado_executor::ToolRuntime;
use liberado_provider::ToolInvocation;
use serde_json::Value;

pub(super) fn wrap(
    session: impl Into<String>,
    inner: Box<dyn ToolRuntime>,
) -> Box<dyn ToolRuntime> {
    Box::new(SearchScopeRuntime {
        session: session.into(),
        inner,
    })
}

/// Arguments to forward, when this call is a project-scoped search that still
/// needs the current conversation id.
pub(super) fn stamp_search_scope(name: &str, arguments: &Value, session: &str) -> Option<Value> {
    if bare_tool_name(name) != "search_conversations" {
        return None;
    }
    let object = arguments.as_object()?;
    let scope = object.get("scope").and_then(Value::as_str).unwrap_or("");
    if !scope.eq_ignore_ascii_case("project") {
        return None;
    }
    if filled(object.get("project")) || filled(object.get("conversation")) {
        return None;
    }
    let mut next = object.clone();
    next.insert("conversation".into(), Value::String(session.to_string()));
    Some(Value::Object(next))
}

fn filled(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

struct SearchScopeRuntime {
    session: String,
    inner: Box<dyn ToolRuntime>,
}

#[async_trait]
impl ToolRuntime for SearchScopeRuntime {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        self.inner.catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if let Some(arguments) = stamp_search_scope(&call.name, &call.arguments, &self.session) {
            let stamped = ToolInvocation {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments,
            };
            return self.inner.invoke(&stamped).await;
        }
        self.inner.invoke(call).await
    }

    fn is_read_only(&self, tool_name: &str) -> bool {
        self.inner.is_read_only(tool_name)
    }

    fn parks_for_human(&self, tool_name: &str) -> bool {
        self.inner.parks_for_human(tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scope_project_receives_this_chat_and_an_explicit_project_does_not() {
        let session = "01CHAT";
        let stamped = stamp_search_scope(
            "chat-search:search_conversations",
            &json!({"query": "dark", "scope": "project"}),
            session,
        )
        .unwrap();
        assert_eq!(stamped["conversation"], session);
        assert!(
            stamp_search_scope(
                "search_conversations",
                &json!({"query": "dark", "scope": "project", "project": "alpha"}),
                session,
            )
            .is_none()
        );
        assert!(
            stamp_search_scope("search_conversations", &json!({"query": "dark"}), session,)
                .is_none()
        );
        assert!(stamp_search_scope("other_tool", &json!({"scope": "project"}), session).is_none());
    }
}
