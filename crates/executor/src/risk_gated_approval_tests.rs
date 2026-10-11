//! Chat-origin permission requests. Split out of `risk_gated_tests.rs` so that file stays put.

use std::sync::Arc;

use liberado_common::Capability;
use liberado_test_support::InvocationRecordingRuntime;

use super::*;

fn vault_descriptor() -> McpDescriptor {
    McpDescriptor {
        name: "vault".into(),
        description: "git-tracked vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: Some("tasks".into()),
        tool_zones: vec![("write_review".into(), Some("reviews".into()))],
        zone_from_arg: None,
        write_tools: Vec::new(),
    }
}

struct RecordingSink(std::sync::Mutex<Vec<String>>);

#[async_trait::async_trait]
impl crate::PermissionSink for RecordingSink {
    async fn notify_permission(&self, proposal_id: &str, _message: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(proposal_id.to_string());
        Ok(())
    }
}

#[tokio::test]
async fn a_web_chat_raises_a_permission_request_without_telegram() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["vault:write_review"],
        Ok("wrote".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner,
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        vec![vault_descriptor()],
        vec![("reviews".to_string(), WriteClass::AgentWritable)],
        dir.path().to_path_buf(),
        "write a review note".into(),
        "test-web-perm".into(),
        ProposalSigner::random(),
        "default",
    )
    .with_approval(crate::ApprovalStamp {
        session_id: "sess-web".into(),
        origin: liberado_common::ApprovalOrigin::Web,
    });

    let call = ToolInvocation::new("c1", "vault:write_review", serde_json::json!({"c": "..."}));
    let msg = rt
        .invoke(&call)
        .await
        .expect("a stamped chat asks even with no notifier");
    assert!(msg.contains("PERMISSION REQUESTED"), "{msg}");
    assert!(rt.took_deferral_to_human());

    let note = std::fs::read_dir(dir.path().join("proposals"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let proposal =
        liberado_common::Proposal::from_note(&std::fs::read_to_string(note.path()).unwrap())
            .unwrap();
    assert_eq!(proposal.session_id.as_deref(), Some("sess-web"));
    assert_eq!(proposal.origin, Some(liberado_common::ApprovalOrigin::Web));
}

#[tokio::test]
async fn a_telegram_chat_sends_scope_buttons_through_the_sink() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["vault:write_review"],
        Ok("wrote".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]);
    let sink = Arc::new(RecordingSink(std::sync::Mutex::new(Vec::new())));
    let rt = RiskGatedToolRuntime::new(
        inner,
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        vec![vault_descriptor()],
        vec![("reviews".to_string(), WriteClass::AgentWritable)],
        dir.path().to_path_buf(),
        "write a review note".into(),
        "test-tg-perm".into(),
        ProposalSigner::random(),
        "default",
    )
    .with_approval(crate::ApprovalStamp {
        session_id: "sess-tg".into(),
        origin: liberado_common::ApprovalOrigin::Channel(liberado_common::ChannelKind::Telegram),
    })
    .with_permission_sink(sink.clone());

    let call = ToolInvocation::new("c1", "vault:write_review", serde_json::json!({"c": "..."}));
    rt.invoke(&call).await.expect("telegram chat asks");
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    let proposal = liberado_common::Proposal::from_note(
        &std::fs::read_to_string(
            std::fs::read_dir(dir.path().join("proposals"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        proposal.origin,
        Some(liberado_common::ApprovalOrigin::Channel(
            liberado_common::ChannelKind::Telegram
        ))
    );
    assert_eq!(proposal.session_id.as_deref(), Some("sess-tg"));
}
