//! The resolver tells only the surface that showed the card.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_common::{
    ApprovalOrigin, Capability, ChannelKind, DecisionVia, Proposal, ProposedAction,
    WriteProvenance, Zone,
};
use liberado_messaging::{ResolvedElsewhere, permission_receipt};
use liberado_vault::Vault;

use super::PermissionResolver;
use crate::proposal_path;

struct Heard {
    id: &'static str,
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl ResolvedElsewhere for Heard {
    fn surface_id(&self) -> &'static str {
        self.id
    }

    async fn on_resolved_elsewhere(&self, proposal_id: &str, receipt: &str) {
        self.calls
            .lock()
            .unwrap()
            .push((proposal_id.to_string(), receipt.to_string()));
    }
}

async fn told(
    origin: Option<ApprovalOrigin>,
    via: DecisionVia,
    push: Option<&str>,
) -> Vec<(String, String)> {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::open("test", dir.path()).await.unwrap();
    let mut proposal = Proposal::pending(
        "perm-hook",
        "corr",
        "liberado-chat",
        ProposedAction::ToolCalls(vec![]),
        "needs Write on zone work",
    )
    .with_requested_grant(Capability::Write(Zone::vault("work")));
    proposal.origin = origin;
    vault
        .write(
            &proposal_path(&proposal.id),
            &proposal.to_note(),
            None,
            &WriteProvenance::human(),
        )
        .await
        .unwrap();

    let resolver = PermissionResolver::new(vault, None);
    if let Some(push) = push {
        resolver.set_push_surface(push);
    }
    let telegram = Arc::new(Mutex::new(Vec::new()));
    let matrix = Arc::new(Mutex::new(Vec::new()));
    resolver.add_resolved_elsewhere(Arc::new(Heard {
        id: "telegram",
        calls: Arc::clone(&telegram),
    }));
    resolver.add_resolved_elsewhere(Arc::new(Heard {
        id: "matrix",
        calls: Arc::clone(&matrix),
    }));
    resolver.resolve("perm-hook", "once", via).await;

    let mut out = Vec::new();
    for (surface, calls) in [("telegram", telegram), ("matrix", matrix)] {
        for (id, receipt) in calls.lock().unwrap().iter() {
            assert_eq!(id, "perm-hook");
            out.push((surface.to_string(), receipt.clone()));
        }
    }
    out
}

fn surfaces(calls: &[(String, String)]) -> Vec<&str> {
    calls.iter().map(|(surface, _)| surface.as_str()).collect()
}

#[tokio::test]
async fn a_webui_decision_tells_only_the_channel_that_showed_the_card() {
    let calls = told(
        Some(ApprovalOrigin::Channel(ChannelKind::Telegram)),
        DecisionVia::Webui,
        Some("telegram"),
    )
    .await;
    assert_eq!(surfaces(&calls), vec!["telegram"]);
    assert_eq!(
        calls[0].1,
        permission_receipt("✅", "Approved once", "needs Write on zone work")
    );

    let matrix = told(
        Some(ApprovalOrigin::Channel(ChannelKind::Matrix)),
        DecisionVia::Webui,
        Some("telegram"),
    )
    .await;
    assert_eq!(surfaces(&matrix), vec!["matrix"]);
}

#[tokio::test]
async fn the_deciding_surface_and_a_web_origin_are_not_told() {
    let same = told(
        Some(ApprovalOrigin::Channel(ChannelKind::Telegram)),
        DecisionVia::Channel(ChannelKind::Telegram),
        Some("telegram"),
    )
    .await;
    assert!(same.is_empty());

    let web = told(
        Some(ApprovalOrigin::Web),
        DecisionVia::Webui,
        Some("telegram"),
    )
    .await;
    assert!(web.is_empty());

    let missing = told(None, DecisionVia::Webui, Some("telegram")).await;
    assert!(missing.is_empty());
}

#[tokio::test]
async fn a_background_card_is_told_on_the_push_surface_only() {
    let pushed = told(
        Some(ApprovalOrigin::Background),
        DecisionVia::Webui,
        Some("telegram"),
    )
    .await;
    assert_eq!(surfaces(&pushed), vec!["telegram"]);

    let same = told(
        Some(ApprovalOrigin::Background),
        DecisionVia::Channel(ChannelKind::Telegram),
        Some("telegram"),
    )
    .await;
    assert!(same.is_empty());

    let unset = told(Some(ApprovalOrigin::Background), DecisionVia::Webui, None).await;
    assert!(unset.is_empty());
}
