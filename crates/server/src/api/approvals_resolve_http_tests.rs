//! HTTP contract for `POST /api/approvals/{id}/resolve` and `GET /api/conversations/{id}/approvals`.
//!
//! The shared resolver test covers the Telegram hand-off. These tests pin each decision the
//! WebUI can send, and the status the handler must return when the note cannot be decided.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use liberado_common::{
    Capability, Proposal, ProposalSigner, ProposalStatus, ProposedAction, WriteProvenance, Zone,
};
use liberado_session_store::SessionStore;
use liberado_telegram_approvals::PermissionResolver;
use liberado_vault::Vault;
use tower::ServiceExt;

use super::{list_approvals, resolve_approval};
use crate::approvals::ApprovalHub;
use crate::state::AppState;
use crate::sticky::StickySession;

struct World {
    app: Router,
    vault: Vault,
    _dir: tempfile::TempDir,
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

async fn world(notes: Vec<Proposal>) -> World {
    let dir = tempfile::tempdir().unwrap();
    let vault_dir = dir.path().join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    let vault = Vault::open("test", &vault_dir).await.unwrap();
    let signer = ProposalSigner::random();
    for note in &notes {
        let signed = signer.sign(note.clone());
        vault
            .write(
                &format!("proposals/{}.md", note.id),
                &signed.to_note(),
                None,
                &WriteProvenance::human(),
            )
            .await
            .unwrap();
    }
    let resolver = Arc::new(PermissionResolver::new(vault.clone(), None));
    let sessions = Arc::new(SessionStore::open(dir.path().join("sessions")).await);
    let mut state = AppState::for_test(sessions, None, dir.path().to_path_buf());
    state.approval_hub = Some(Arc::new(ApprovalHub::new(
        resolver,
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
    World {
        app,
        vault,
        _dir: dir,
    }
}

async fn post(app: &Router, id: &str, body: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/approvals/{id}/resolve"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
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

async fn get_list(app: &Router, id: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/conversations/{id}/approvals"))
                .body(Body::empty())
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

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("body is not json ({err}): {body}"))
}

async fn stored(world: &World, id: &str) -> Proposal {
    let raw = world
        .vault
        .read(&format!("proposals/{id}.md"))
        .await
        .unwrap();
    Proposal::from_note(&raw).unwrap()
}

#[tokio::test]
async fn approve_once_writes_the_decision_on_the_note() {
    let world = world(vec![permission("perm-once", Some("sess-a"))]).await;
    let (status, body) = post(&world.app, "perm-once", r#"{"decision":"once"}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = json(&body);
    assert_eq!(body["status"], "decided");
    assert_eq!(body["decision"], "once");
    assert_eq!(body["label"], "Approved once");
    assert!(body["error"].is_null(), "{body}");

    let note = stored(&world, "perm-once").await;
    assert_eq!(note.status, ProposalStatus::Approved);
    assert_eq!(note.approved_scope, Some(liberado_common::GrantScope::Once));
    assert_eq!(note.decided_via, Some(liberado_common::DecisionVia::Webui));
}

#[tokio::test]
async fn deny_writes_a_rejection_on_the_note() {
    let world = world(vec![permission("perm-deny", Some("sess-a"))]).await;
    let (status, body) = post(&world.app, "perm-deny", r#"{"decision":"deny"}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = json(&body);
    assert_eq!(body["status"], "decided");
    assert_eq!(body["decision"], "deny");
    assert_eq!(body["label"], "Denied");
    assert!(body["error"].is_null(), "{body}");

    let note = stored(&world, "perm-deny").await;
    assert_eq!(note.status, ProposalStatus::Rejected);
    assert_eq!(note.approved_scope, None);
    assert_eq!(note.decided_via, Some(liberado_common::DecisionVia::Webui));
}

#[tokio::test]
async fn a_second_resolve_after_deny_is_a_conflict() {
    let world = world(vec![permission("perm-twice", Some("sess-a"))]).await;
    let (status, body) = post(&world.app, "perm-twice", r#"{"decision":"deny"}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = post(&world.app, "perm-twice", r#"{"decision":"once"}"#).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let body = json(&body);
    assert_eq!(body["status"], "already_decided");
    assert_eq!(body["decision"], "deny");
    assert_eq!(body["label"], "Denied");
    assert_eq!(body["error"], "already decided: Denied");

    let note = stored(&world, "perm-twice").await;
    assert_eq!(note.status, ProposalStatus::Rejected);
    assert_eq!(note.approved_scope, None);
}

#[tokio::test]
async fn an_expired_request_cannot_be_approved() {
    let mut expired = permission("perm-expired", Some("sess-exp"));
    expired.expires = Some(chrono::Utc::now() - chrono::Duration::hours(2));
    let world = world(vec![expired]).await;

    let (status, body) = post(&world.app, "perm-expired", r#"{"decision":"once"}"#).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let body = json(&body);
    assert_eq!(body["status"], "already_decided");
    assert_eq!(body["decision"], "expired");
    assert_eq!(body["label"], "Expired");
    assert_eq!(body["error"], "already decided: Expired");

    let note = stored(&world, "perm-expired").await;
    assert_eq!(note.status, ProposalStatus::Pending);
    assert_eq!(note.approved_scope, None);
    assert!(note.decided_via.is_none());
}

#[tokio::test]
async fn an_unknown_approval_is_not_found() {
    let world = world(vec![]).await;
    let (status, body) = post(&world.app, "missing-approval", r#"{"decision":"once"}"#).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let body = json(&body);
    assert_eq!(body["status"], "not_found");
    assert_eq!(body["error"], "not found");
    assert!(body["decision"].is_null(), "{body}");
}

#[tokio::test]
async fn a_resolve_body_that_is_not_json_is_rejected() {
    let world = world(vec![permission("perm-bad", Some("sess-a"))]).await;
    let (status, body) = post(&world.app, "perm-bad", "{not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let note = stored(&world, "perm-bad").await;
    assert_eq!(note.status, ProposalStatus::Pending);
}

#[tokio::test]
async fn a_resolve_body_without_a_decision_is_rejected() {
    let world = world(vec![permission("perm-empty", Some("sess-a"))]).await;
    let (status, body) = post(&world.app, "perm-empty", "{}").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let note = stored(&world, "perm-empty").await;
    assert_eq!(note.status, ProposalStatus::Pending);
}

#[tokio::test]
async fn an_unknown_conversation_lists_no_approval_cards() {
    let world = world(vec![permission("perm-known", Some("sess-known"))]).await;

    let (status, body) = get_list(&world.app, "sess-known").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = json(&body);
    let ids: Vec<&str> = listed["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|card| card["id"].as_str())
        .collect();
    assert_eq!(ids, vec!["perm-known"]);

    let (status, body) = get_list(&world.app, "sess-missing").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = json(&body);
    assert_eq!(listed["approvals"].as_array().unwrap().len(), 0);
}
