use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::StatusCode;
use chrono::Timelike;
use liberado_common::{
    Capability, DecisionVia, Proposal, ProposalStatus, ProposedAction, ToolCall,
};
use liberado_telegram_approvals::{PermissionResolver, ResolveOutcome};
use liberado_vault::Vault;
use serde_json::json;

use super::{apply_at, card_options, http_response, is_schedule_edit};

const POLICY: &str = r#"
[[zones]]
zone = "tasks"
write_class = "agent_writable"

[[zones]]
zone = "decisions"
write_class = "agent_writable"

[[zones]]
zone = "finance"
write_class = "human_only"

[[grants]]
component = "agent"
capabilities = [
    { Read = { Vault = "tasks" } },
    { Write = { Vault = "tasks" } },
    { ExecuteMcp = "memory-mcp" },
]
"#;

const TOPOLOGY: &str = r#"
vault_path = "/tmp/vault"

[[mcps]]
name = "memory-mcp"
description = "store and recall memories"
consequence = "reversible"
transport = { kind = "stdio", command = "memory-mcp", args = [] }
writes_vault = false

[[schedules]]
name = "morning"
enabled = true
cron_expr = "0 0 8 * * * *"
goal = "review the inbox"

[[schedules]]
name = "evening"
enabled = true
cron_expr = "0 0 20 * * * *"
goal = "close the day"
"#;

fn config_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("topology.toml"), TOPOLOGY).unwrap();
    std::fs::write(dir.path().join("policy.toml"), POLICY).unwrap();
    dir
}

fn topology(dir: &Path) -> PathBuf {
    dir.join("topology.toml")
}

fn card(tool: &str, args: serde_json::Value) -> Proposal {
    Proposal::pending(
        "sched-1",
        "sched-1",
        "automation-editor",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: tool.into(),
            args,
        }]),
        "edit",
    )
    .with_requested_grant(Capability::ExecuteTool(format!("automation-editor:{tool}")))
}

fn noon() -> serde_json::Value {
    json!({
        "name": "noon",
        "enabled": true,
        "cron_expr": "0 0 12 * * * *",
        "goal": "lunch"
    })
}

#[test]
fn schedule_cards_offer_approve_and_deny_only() {
    let proposal = card("create_schedule", noon());
    let options = card_options(&proposal);
    let actions: Vec<_> = options
        .iter()
        .map(|option| option.action.as_str())
        .collect();
    assert_eq!(actions, vec!["approve", "deny"]);
    let other = Proposal::pending(
        "perm",
        "perm",
        "chat",
        ProposedAction::ToolCalls(vec![]),
        "write",
    )
    .with_requested_grant(Capability::ExecuteTool("other:tool".into()));
    assert!(!is_schedule_edit(&other));
    let wide = card_options(&other);
    assert!(wide.iter().any(|option| option.action == "once"));
    assert!(wide.iter().any(|option| option.action == "everywhere"));
}

#[test]
fn approve_create_update_and_delete_each_write_once() {
    let dir = config_dir();
    let path = topology(dir.path());
    let created = apply_at(&path, &card("create_schedule", noon()));
    assert!(created.contains("The file is on disk"), "{created}");
    assert!(created.contains("process is recreated"), "{created}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("noon"), "{text}");
    let backups = backup_count(dir.path());
    assert_eq!(backups, 1);

    let updated = apply_at(
        &path,
        &card(
            "update_schedule",
            json!({"name": "morning", "cron_expr": "0 0 8 * * * *", "goal": "new goal"}),
        ),
    );
    assert!(updated.contains("Updated schedule 'morning'"), "{updated}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("new goal"), "{text}");

    let deleted = apply_at(&path, &card("delete_schedule", json!({"name": "evening"})));
    assert!(deleted.contains("Deleted schedule 'evening'"), "{deleted}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("evening"), "{text}");
    assert!(text.contains("noon"), "{text}");
}

#[test]
fn a_bad_cron_leaves_the_original_file() {
    let dir = config_dir();
    let path = topology(dir.path());
    let original = std::fs::read_to_string(&path).unwrap();
    let reply = apply_at(
        &path,
        &card(
            "create_schedule",
            json!({"name": "bad", "cron_expr": "not-a-cron", "goal": "nope"}),
        ),
    );
    assert!(
        reply.contains("original file was not changed") || reply.contains("wrote nothing"),
        "{reply}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn a_shape_error_names_the_problem_and_leaves_the_file() {
    let dir = config_dir();
    let path = topology(dir.path());
    let original = std::fs::read_to_string(&path).unwrap();
    let reply = apply_at(&path, &card("create_schedule", json!({"name": "only"})));
    assert!(
        reply.contains("Could not read the schedule edit"),
        "{reply}"
    );
    assert!(
        reply.contains("exactly one of cron_expr or days and at"),
        "{reply}"
    );
    assert!(reply.contains("topology.toml was not changed"), "{reply}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert_eq!(backup_count(dir.path()), 0);
}

#[test]
fn a_days_and_at_card_writes_the_structured_row() {
    let dir = config_dir();
    let path = topology(dir.path());
    let reply = apply_at(
        &path,
        &card(
            "create_schedule",
            json!({
                "name": "trash",
                "days": ["tue"],
                "at": "20:00",
                "goal": "bins"
            }),
        ),
    );
    assert!(reply.contains("The file is on disk"), "{reply}");
    assert!(
        !reply.contains("Could not read the schedule edit"),
        "{reply}"
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("name = \"trash\""), "{text}");
    assert!(text.contains("days = [\"tue\"]"), "{text}");
    assert!(text.contains("at = \"20:00\""), "{text}");
    assert!(text.contains("morning"), "{text}");
    assert_eq!(backup_count(dir.path()), 1);
}

#[tokio::test]
async fn the_card_approve_path_writes_once_and_deny_writes_nothing() {
    let dir = config_dir();
    let path = topology(dir.path());
    let original = std::fs::read_to_string(&path).unwrap();
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Vault::open("test", vault_dir.path()).await.unwrap();
    let mut proposal = card("create_schedule", noon());
    proposal.id = "sched-apply".into();
    proposal.correlation_id = "sched-apply".into();
    vault
        .write(
            "proposals/sched-apply.md",
            &proposal.to_note(),
            None,
            &liberado_common::WriteProvenance::human(),
        )
        .await
        .unwrap();
    let resolver = PermissionResolver::new(vault.clone(), None);
    let calls = AtomicUsize::new(0);
    let file = path.clone();
    let first = resolver
        .finish_schedule_card("sched-apply", "approve", DecisionVia::Webui, |proposal| {
            calls.fetch_add(1, Ordering::SeqCst);
            apply_at(&file, proposal)
        })
        .await;
    let second = resolver
        .finish_schedule_card("sched-apply", "once", DecisionVia::Webui, |proposal| {
            calls.fetch_add(1, Ordering::SeqCst);
            apply_at(&file, proposal)
        })
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    match first {
        liberado_telegram_approvals::ResolveOutcome::Decided { rationale, .. } => {
            assert!(rationale.contains("The file is on disk"), "{rationale}");
        }
        other => panic!("expected Decided, got {other:?}"),
    }
    assert!(matches!(
        second,
        liberado_telegram_approvals::ResolveOutcome::AlreadyDecided { .. }
    ));
    let note = Proposal::from_note(&vault.read("proposals/sched-apply.md").await.unwrap()).unwrap();
    assert_eq!(note.status, ProposalStatus::Done);
    assert!(note.approved_scope.is_none());
    assert_eq!(backup_count(dir.path()), 1);
    assert!(std::fs::read_to_string(&path).unwrap().contains("noon"));

    let mut denied = card("delete_schedule", json!({"name": "morning"}));
    denied.id = "sched-deny".into();
    denied.correlation_id = "sched-deny".into();
    vault
        .write(
            "proposals/sched-deny.md",
            &denied.to_note(),
            None,
            &liberado_common::WriteProvenance::human(),
        )
        .await
        .unwrap();
    let before_deny = std::fs::read_to_string(&path).unwrap();
    let deny_calls = AtomicUsize::new(0);
    let outcome = resolver
        .finish_schedule_card("sched-deny", "deny", DecisionVia::Webui, |_| {
            deny_calls.fetch_add(1, Ordering::SeqCst);
            "no".into()
        })
        .await;
    assert_eq!(deny_calls.load(Ordering::SeqCst), 0);
    match outcome {
        liberado_telegram_approvals::ResolveOutcome::Decided { rationale, .. } => {
            assert_eq!(rationale, "Denied. topology.toml was not changed.");
        }
        other => panic!("expected Decided, got {other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before_deny);
    assert_ne!(original, before_deny);
}

/// Each `ResolveOutcome` arm of `http_response`, including both sides of the
/// shared 404 and 500 patterns. `reply` sees `Some` and `None` for decision,
/// label, and error.
#[tokio::test]
async fn http_response_maps_each_resolve_outcome() {
    let cases: [(ResolveOutcome, StatusCode, &str); 7] = [
        (
            ResolveOutcome::Decided {
                action: "approve",
                label: "Approve".into(),
                emoji: "ok",
                rationale: "The file is on disk.".into(),
            },
            StatusCode::OK,
            r#"{"status":"decided","decision":"approve","label":"Approve"}"#,
        ),
        (
            ResolveOutcome::AlreadyDecided {
                action: "deny",
                label: "Deny".into(),
            },
            StatusCode::CONFLICT,
            r#"{"status":"already_decided","decision":"deny","label":"Deny","error":"already decided: Deny"}"#,
        ),
        (
            ResolveOutcome::NotFound,
            StatusCode::NOT_FOUND,
            r#"{"status":"not_found","error":"not found"}"#,
        ),
        (
            ResolveOutcome::NotAPermissionRequest,
            StatusCode::NOT_FOUND,
            r#"{"status":"not_found","error":"not found"}"#,
        ),
        (
            ResolveOutcome::UnknownAction,
            StatusCode::BAD_REQUEST,
            r#"{"status":"unknown_decision","error":"unknown decision"}"#,
        ),
        (
            ResolveOutcome::Unreadable,
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"status":"save_failed","error":"could not save the decision"}"#,
        ),
        (
            ResolveOutcome::SaveFailed,
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"status":"save_failed","error":"could not save the decision"}"#,
        ),
    ];

    for (outcome, expect_status, expect_body) in cases {
        let response = http_response(outcome);
        assert_eq!(response.status(), expect_status);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert_eq!(body, expect_body, "{expect_status}");
    }
}

#[test]
fn schedules_json_with_local_clock_stamps_the_file_timezone() {
    let file: super::ScheduleFile = toml::from_str(
        r#"
timezone = "America/Chicago"
[[schedules]]
name = "morning"
cron_expr = "0 30 12 * * * *"
goal = "ping"
"#,
    )
    .unwrap();
    let json = super::schedules_json_with_local_clock(&file).unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "morning");
    assert_eq!(rows[0]["cron_expr"], "0 30 12 * * * *");
    assert_eq!(rows[0]["timezone"], "America/Chicago");
    let english = liberado_cron::cron_to_english("0 30 12 * * * *", "America/Chicago");
    assert_eq!(rows[0]["local_time"], english);
    assert_eq!(rows[0]["schedule_text"], english);
    assert_eq!(rows[0]["category"], "automation");
    assert_eq!(rows[0]["cron_expr"], "0 30 12 * * * *");
    let shown = rows[0]["next_fires"].as_array().expect("next_fires");
    let computed = liberado_cron::next_fires("0 30 12 * * * *", "America/Chicago", 5).unwrap();
    assert_eq!(shown.len(), computed.len());
    assert_eq!(computed.len(), 5);
    for (row, fire) in shown.iter().zip(computed) {
        assert_eq!(row["weekday"], fire.weekday_name());
        assert_eq!(row["display"], fire.display());
        assert_eq!(row["utc"], fire.utc.to_rfc3339());
        assert_eq!(row["local"], fire.local.to_rfc3339());
    }
}

#[test]
fn a_structured_row_lists_civil_english_and_zoned_fires() {
    let file: super::ScheduleFile = toml::from_str(
        r#"
timezone = "America/Chicago"
[[schedules]]
name = "trash"
days = ["tue"]
at = "20:00"
goal = "bins"
"#,
    )
    .unwrap();
    let json = super::schedules_json_with_local_clock(&file).unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(rows[0]["days"][0], "tue");
    assert_eq!(rows[0]["at"], "20:00");
    assert_eq!(
        rows[0]["schedule_text"],
        "Every Tuesday at 8:00 PM America/Chicago"
    );
    let spec = liberado_common::parse_civil(&["tue".into()], "20:00").unwrap();
    let computed = liberado_cron::next_fires_zoned(
        &liberado_common::compile_cron(&spec),
        "America/Chicago",
        5,
    )
    .unwrap();
    let shown = rows[0]["next_fires"].as_array().expect("next_fires");
    assert_eq!(shown.len(), computed.len());
    for (row, fire) in shown.iter().zip(computed) {
        assert_eq!(row["utc"], fire.utc.to_rfc3339());
        assert_eq!(row["local"], fire.local.to_rfc3339());
        assert_eq!(fire.local.hour(), 20);
        assert_eq!(fire.weekday_name(), "Tuesday");
    }
}

#[test]
fn a_reminder_tick_row_is_marked_system() {
    let file: super::ScheduleFile = toml::from_str(
        r#"
timezone = "UTC"
[[schedules]]
name = "vault-reminder-tick"
cron_expr = "0 * * * * *"
job = "vault-reminder-tick"
"#,
    )
    .unwrap();
    let json = super::schedules_json_with_local_clock(&file).unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(rows[0]["job"], "vault-reminder-tick");
    assert_eq!(rows[0]["category"], "system");
}

fn backup_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .ok()
                .and_then(|entry| entry.file_name().into_string().ok())
                .is_some_and(|name| name.contains(".bak-"))
        })
        .count()
}
