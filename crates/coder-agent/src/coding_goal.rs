//! Typed boundary for the coding pack's otherwise opaque goal payload.
//!
//! [`liberado_session::GoalSpec`] deliberately keeps `payload` domain-neutral. This type is the
//! coding pack's owned interpretation of the stable dispatch fields. Unknown fields are retained:
//! they remain available to pack-owned extensions, but a malformed known field now fails before a
//! coding session can select or write a workspace.

use liberado_coder_core::DispatchWriteScope;
use serde::{Deserialize, Serialize};

/// Stable, surface-supplied fields of a coding goal payload.
///
/// Keep this in the coding pack rather than the session kernel. A goal session may belong to any
/// domain; only coding knows what a project, workspace, or worker model means.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodingGoalPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interactive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    force_host_local: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fanout_child: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    write_scope: Option<DispatchWriteScope>,
    /// Pack-owned extensions. This preserves forwards compatibility without making the session
    /// kernel understand coding controls.
    #[serde(flatten)]
    extensions: serde_json::Map<String, serde_json::Value>,
}

impl CodingGoalPayload {
    /// Decode a goal payload at the coding-domain boundary.
    ///
    /// `GoalSpec::payload` defaults to JSON null. Treat it as an empty coding payload, while
    /// rejecting every other non-object shape and mistyped stable field.
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone())
            .map_err(|e| format!("invalid coding goal payload: {e}"))
    }

    /// Serialize this payload for the domain-neutral goal session kernel.
    pub fn into_value(self) -> serde_json::Value {
        serde_json::to_value(self).expect("CodingGoalPayload always serializes")
    }

    /// Borrow this payload as its domain-neutral JSON representation.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("CodingGoalPayload always serializes")
    }

    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    pub fn workspace_root(&self) -> Option<&str> {
        self.workspace_root.as_deref()
    }

    pub fn interactive(&self) -> Option<bool> {
        self.interactive
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn force_host_local(&self) -> bool {
        self.force_host_local.unwrap_or(false)
    }

    pub fn fanout_child(&self) -> bool {
        self.fanout_child.unwrap_or(false)
    }

    pub fn write_scope(&self) -> Option<&DispatchWriteScope> {
        self.write_scope.as_ref()
    }

    /// Record the configuration-resolved project root. A client path never survives this rewrite.
    pub fn set_authorized_workspace(&mut self, project: String, workspace_root: String) {
        self.project = Some(project);
        self.workspace_root = Some(workspace_root);
    }
}

#[cfg(test)]
mod tests {
    use super::CodingGoalPayload;
    use serde_json::json;

    #[test]
    fn null_payload_is_an_empty_coding_payload() {
        let payload = CodingGoalPayload::parse(&serde_json::Value::Null).expect("valid payload");
        assert_eq!(payload.workspace_root(), None);
        assert_eq!(payload.into_value(), json!({}));
    }

    #[test]
    fn preserves_pack_extensions_while_typing_dispatch_fields() {
        let mut payload = CodingGoalPayload::parse(&json!({
            "project": "untrusted",
            "workspace_root": "C:/untrusted",
            "model": "test-model",
            "preflight": { "steps": ["cargo test"] },
        }))
        .expect("valid payload");
        payload.set_authorized_workspace("life-os".into(), "C:/approved".into());

        assert_eq!(payload.project(), Some("life-os"));
        assert_eq!(payload.workspace_root(), Some("C:/approved"));
        assert_eq!(payload.model(), Some("test-model"));
        assert_eq!(
            payload.into_value(),
            json!({
                "project": "life-os",
                "workspace_root": "C:/approved",
                "model": "test-model",
                "preflight": { "steps": ["cargo test"] },
            })
        );
    }

    #[test]
    fn rejects_mistyped_workspace_before_a_pack_can_use_it() {
        let error = CodingGoalPayload::parse(&json!({ "workspace_root": 42 }))
            .expect_err("workspace must be a string");
        assert!(error.contains("expected a string"), "error: {error}");
    }

    #[test]
    fn rejects_malformed_write_scope_before_policy_resolution() {
        let error = CodingGoalPayload::parse(&json!({
            "write_scope": { "allow": ["docs/**"] }
        }))
        .expect_err("write_scope must use its typed fields");
        assert!(error.contains("unknown field `allow`"), "error: {error}");
    }

    /// Every dispatch flag survives the boundary with its real value — a stub here would
    /// silently flip interactivity, fan-out nesting, or write policy for every goal.
    #[test]
    fn dispatch_flags_round_trip_with_their_real_values() {
        let payload = CodingGoalPayload::parse(&json!({
            "interactive": true,
            "fanout_child": true,
            "force_host_local": true,
            "write_scope": { "allow_globs": ["src/**"], "deny_globs": ["docs/**"] },
        }))
        .expect("valid payload");
        assert_eq!(payload.interactive(), Some(true));
        assert!(payload.fanout_child());
        assert!(payload.force_host_local());
        let scope = payload.write_scope().expect("scope present");
        assert_eq!(scope.allow_globs, vec!["src/**".to_string()]);
        assert_eq!(scope.deny_globs, vec!["docs/**".to_string()]);

        let empty = CodingGoalPayload::parse(&json!({})).expect("valid empty");
        assert_eq!(empty.interactive(), None);
        assert!(!empty.fanout_child());
        assert!(!empty.force_host_local());
        assert!(empty.write_scope().is_none());
    }
}
