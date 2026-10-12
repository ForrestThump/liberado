//! HTTP resolve: 200, 404, 409, and the same resolver the Telegram callback uses.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use liberado_common::{
    Capability, DecisionVia, Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall,
    WriteProvenance, Zone,
};
use liberado_messaging::{ActionButton, InboundEvent, MessagingChannel, MessagingError};
use liberado_provider::MockProvider;
use liberado_session_store::SessionStore;
use liberado_telegram_approvals::{ApprovalBot, PermissionResolver, ResolveOutcome};
use liberado_vault::Vault;
use tower::ServiceExt;

use super::{list_approvals, list_pending_approvals, resolve_approval};
use crate::approvals::ApprovalHub;
use crate::state::AppState;
use crate::sticky::StickySession;

struct Quiet;

#[async_trait]
impl MessagingChannel for Quiet {
    fn name(&self) -> &str {
        "quiet"
    }
    async fn send_text(&self, _: &str) -> Result<(), MessagingError> {
        Ok(())
    }
    async fn send_with_actions(
        &self,
        _: &str,
        _: &[Vec<ActionButton>],
    ) -> Result<(), MessagingError> {
        Ok(())
    }
    async fn request_reply(&self, _: &str) -> Result<String, MessagingError> {
        Ok("prompt".into())
    }
    async fn acknowledge(&self, _: &str, _: &str) -> Result<(), MessagingError> {
        Ok(())
    }
    async fn receive(&self, _: &mut String) -> Result<Vec<InboundEvent>, MessagingError> {
        Ok(vec![])
    }
}

async fn post(app: &Router, id: &str, decision: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/approvals/{id}/resolve"))
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"decision":"{decision}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn permission(id: &str, session: Option<&str>) -> Proposal {
    let mut proposal = Proposal::pending(
        id,
        "corr",
        "liberado-chat",
        ProposedAction::ToolCalls(vec![]),
        "needs Write on zone work",
    )
    .with_requested_grant(Capability::Write(Zone::vault("work")));
    proposal.session_id = session.map(str::to_string);
    proposal.origin = Some(liberado_common::ApprovalOrigin::Web);
    proposal
}

#[tokio::test]
async fn post_resolves_and_telegram_then_http_share_the_resolver() {
    let dir = tempfile::tempdir().unwrap();
    let vault_dir = dir.path().join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    let vault = Vault::open("test", &vault_dir).await.unwrap();
    let signer = ProposalSigner::random();
    let mut pending = permission("perm-http", Some("sess-web"));
    pending.origin = Some(liberado_common::ApprovalOrigin::Web);
    let signed = signer.sign(pending);
    vault
        .write(
            "proposals/perm-http.md",
            &signed.to_note(),
            None,
            &WriteProvenance::human(),
        )
        .await
        .unwrap();
    let mut shared = permission("perm-shared", Some("sess-web"));
    shared.origin = Some(liberado_common::ApprovalOrigin::Telegram);
    let signed = signer.sign(shared);
    vault
        .write(
            "proposals/perm-shared.md",
            &signed.to_note(),
            None,
            &WriteProvenance::human(),
        )
        .await
        .unwrap();

    let resolver = Arc::new(PermissionResolver::new(
        vault.clone(),
        Some(liberado_common::ApprovalLedger::new(dir.path())),
    ));
    let tuning = liberado_bootstrap::Config::default()
        .tuning
        .telegram_approvals
        .clone();
    let bot = ApprovalBot::new(
        Arc::new(Quiet),
        vault.clone(),
        signer,
        Arc::new(MockProvider::new("mock")),
        tuning,
    )
    .with_resolver(Arc::clone(&resolver));

    let sessions = Arc::new(SessionStore::open(dir.path().join("sessions")).await);
    let mut state = AppState::for_test(sessions, None, dir.path().to_path_buf());
    state.approval_hub = Some(Arc::new(ApprovalHub::new(
        Arc::clone(&resolver),
        StickySession::ephemeral(),
    )));
    let app = Router::new()
        .route(
            "/api/approvals/{id}/resolve",
            axum::routing::post(resolve_approval),
        )
        .route(
            "/api/conversations/{id}/approvals",
            axum::routing::get(list_approvals),
        )
        .with_state(Arc::new(state));

    let (status, body) = post(&app, "perm-http", "once").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"status\":\"decided\""), "{body}");
    assert!(body.contains("Approved once"), "{body}");

    let (status, body) = post(&app, "perm-http", "everywhere").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("already decided: Approved once"), "{body}");

    let (status, _) = post(&app, "missing", "once").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = post(&app, "perm-http", "revise").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    bot.handle_action("session", "perm-shared", "evt", None)
        .await;
    let (status, body) = post(&app, "perm-shared", "deny").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body.contains("already decided: Approved for this session"),
        "{body}"
    );
    let note = Proposal::from_note(&vault.read("proposals/perm-shared.md").await.unwrap()).unwrap();
    assert_eq!(note.status, ProposalStatus::Approved);
    assert_eq!(
        note.approved_scope,
        Some(liberado_common::GrantScope::Session)
    );
    assert_eq!(note.decided_via, Some(DecisionVia::Telegram));

    let listed = app
        .oneshot(
            Request::builder()
                .uri("/api/conversations/sess-web/approvals")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(listed.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("perm-http"), "{text}");
    assert!(text.contains("perm-shared"), "{text}");
    for option in liberado_messaging::permission_card_options() {
        assert!(
            text.contains(option.label),
            "{text} missing {}",
            option.label
        );
    }

    let direct = resolver
        .resolve("perm-http", "deny", DecisionVia::Telegram)
        .await;
    assert_eq!(
        direct,
        ResolveOutcome::AlreadyDecided {
            action: "once",
            label: "Approved once".into(),
        }
    );
}

fn open_permission(id: &str, session: Option<&str>, tool: &str, zone: &str) -> Proposal {
    let mut proposal = Proposal::pending(
        id,
        "corr",
        "liberado-chat",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: tool.to_string(),
            args: serde_json::json!({}),
        }]),
        "needs write",
    )
    .with_requested_grant(Capability::Write(Zone::vault(zone)));
    proposal.session_id = session.map(str::to_string);
    proposal
}

#[tokio::test]
async fn get_pending_lists_open_permission_requests_only() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::open("test", dir.path()).await.unwrap();
    let mut older = open_permission(
        "perm-old",
        Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        "turbovault:edit_note",
        "tasks",
    );
    older.created = Utc::now() - Duration::seconds(120);
    let newer = open_permission("perm-new", None, "turbovault:move_note", "work");
    let mut decided = open_permission(
        "perm-done",
        Some("sess-web"),
        "turbovault:edit_note",
        "tasks",
    );
    decided.status = ProposalStatus::Approved;
    let ordinary = Proposal::pending(
        "prop-plain",
        "corr",
        "liberado-chat",
        ProposedAction::External {
            description: "send".into(),
        },
        "ordinary",
    );
    for proposal in [&older, &newer, &decided, &ordinary] {
        vault
            .write(
                &format!("proposals/{}.md", proposal.id),
                &proposal.to_note(),
                None,
                &WriteProvenance::human(),
            )
            .await
            .unwrap();
    }

    let resolver = Arc::new(PermissionResolver::new(vault, None));
    let sessions = Arc::new(SessionStore::open(dir.path().join("sessions")).await);
    let mut state = AppState::for_test(sessions, None, dir.path().to_path_buf());
    state.approval_hub = Some(Arc::new(ApprovalHub::new(
        resolver,
        StickySession::ephemeral(),
    )));
    let app = Router::new()
        .route(
            "/api/approvals/pending",
            axum::routing::get(list_pending_approvals),
        )
        .with_state(Arc::new(state));

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/approvals/pending")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(listed.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let rows = body["approvals"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{body}");
    assert_eq!(rows[0]["id"], "perm-old");
    assert_eq!(rows[0]["conversation"], "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    assert_eq!(rows[0]["tool"], "turbovault:edit_note");
    assert_eq!(rows[0]["zone"], "tasks");
    assert!(rows[0]["created_at"].as_str().unwrap().contains("T"));
    assert_eq!(rows[1]["id"], "perm-new");
    assert!(rows[1]["conversation"].is_null());
    assert_eq!(rows[1]["tool"], "turbovault:move_note");
    assert_eq!(rows[1]["zone"], "work");

    let posted = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/approvals/pending")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"decision":"once"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(posted.status(), StatusCode::OK);
}
