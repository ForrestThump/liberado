//! One Web Push POST.
//!
//! Ported from `origin/feat/weekly-spending-push` @ `ac9a7fa4` (PR #317). The week
//! topic and the JSON subscription file stayed on that branch. This file sends one
//! payload. 2xx is acceptance. 404 and 410 mean the subscription is gone.

use std::time::Duration;

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use super::aesgcm::{self, SealParts};
use super::vapid::VapidMaterial;

pub const REMINDER_TTL: u64 = 6 * 60 * 60;

/// Minutes for the tray Snooze button and for `POST /api/outbox/action` when
/// `minutes` is omitted. The detail page's short snooze uses this same length.
pub const DEFAULT_SNOOZE_MINUTES: i64 = 15;

/// `kind` value for a vault task. Matches `liberado_outbox::TASK_DUE`.
/// This crate does not depend on the outbox crate in production.
const TASK_DUE_KIND: &str = "task_due";

/// `source.type` for a note task. Matches `OutboxSource::VaultNote`.
const VAULT_NOTE_SOURCE: &str = "vault_note";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushDelivery {
    Accepted,
    Gone,
    Failed,
}

pub struct PushRequest<'a> {
    pub endpoint: &'a str,
    pub p256dh: &'a str,
    pub auth: &'a str,
    pub payload: &'a [u8],
    pub ttl_secs: u64,
    pub urgency: &'a str,
}

/// Ids for the worker log. The endpoint and payload are not logged.
pub struct PushAttemptIds<'a> {
    pub row_ids: &'a [i64],
    pub subscription_id: i64,
}

/// Landing URL for notification click navigation (service worker `payload.url`).
pub fn notification_url(kind: &str, outbox_id: i64) -> String {
    if kind == "digest" {
        return "/notifications".to_string();
    }
    format!("/notifications/{outbox_id}")
}

#[derive(Debug, Clone, Serialize)]
struct PayloadRow {
    id: i64,
    token: String,
}

#[derive(Debug, Clone, Serialize)]
struct PayloadDigestItem {
    outbox_id: i64,
    kind: String,
    title: String,
}

#[derive(Debug, Clone, Serialize)]
struct PayloadAction {
    action: &'static str,
    title: String,
}

/// Fields needed to build a v2 reminder push JSON object.
#[derive(Debug, Clone)]
pub struct ReminderPushInput {
    pub title: String,
    pub body: String,
    pub tag: String,
    pub rows: Vec<(i64, String)>,
    pub kind: String,
    pub outbox_id: Option<i64>,
    pub due_at: Option<DateTime<Utc>>,
    pub late: bool,
    pub source: Option<serde_json::Value>,
    pub items: Vec<(i64, String, String)>,
}

impl ReminderPushInput {
    pub fn url(&self) -> String {
        match self.outbox_id {
            Some(id) => notification_url(&self.kind, id),
            None => notification_url("digest", 0),
        }
    }
}

pub fn reminder_payload(input: &ReminderPushInput) -> String {
    let rows: Vec<PayloadRow> = input
        .rows
        .iter()
        .map(|(id, token)| PayloadRow {
            id: *id,
            token: token.clone(),
        })
        .collect();
    let items: Vec<PayloadDigestItem> = input
        .items
        .iter()
        .map(|(outbox_id, kind, title)| PayloadDigestItem {
            outbox_id: *outbox_id,
            kind: kind.clone(),
            title: title.clone(),
        })
        .collect();
    let mut value = serde_json::json!({
        "v": 2,
        "title": input.title,
        "body": input.body,
        "tag": input.tag,
        "rows": rows,
        "url": input.url(),
        "kind": input.kind,
        "outbox_id": input.outbox_id,
        "due_at": input.due_at.map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        "late": input.late,
    });
    if let Some(source) = &input.source {
        value["source"] = source.clone();
    }
    if !items.is_empty() {
        value["items"] = serde_json::to_value(items).unwrap_or(serde_json::Value::Array(vec![]));
    }
    attach_tray_actions(&mut value, input);
    value.to_string()
}

fn attach_tray_actions(value: &mut serde_json::Value, input: &ReminderPushInput) {
    let Some(actions) = tray_actions(input) else {
        return;
    };
    value["actions"] = serde_json::to_value(&actions).unwrap_or(serde_json::Value::Array(vec![]));
    value["snooze_minutes"] = serde_json::json!(DEFAULT_SNOOZE_MINUTES);
}

/// Done and Snooze belong on a single vault task. Digests, tests, events, and
/// approvals omit the field so older workers and non-task pushes stay unchanged.
fn tray_actions(input: &ReminderPushInput) -> Option<Vec<PayloadAction>> {
    if !completable_vault_task(input) {
        return None;
    }
    Some(vec![
        PayloadAction {
            action: "done",
            title: "Done".to_string(),
        },
        PayloadAction {
            action: "snooze",
            title: format!("Snooze {DEFAULT_SNOOZE_MINUTES}m"),
        },
    ])
}

fn completable_vault_task(input: &ReminderPushInput) -> bool {
    input.kind == TASK_DUE_KIND
        && input.rows.len() == 1
        && source_type(input) == Some(VAULT_NOTE_SOURCE)
}

fn source_type(input: &ReminderPushInput) -> Option<&str> {
    input.source.as_ref()?.get("type")?.as_str()
}

pub async fn post_push(
    keys: &VapidMaterial,
    request: PushRequest<'_>,
    attempt: PushAttemptIds<'_>,
) -> PushDelivery {
    let host = host_of(request.endpoint);
    let attempt_at = Utc::now();
    match post(keys, &request).await {
        Ok(status) => {
            let class = classify(status);
            log_push_http(&host, &attempt, attempt_at, &class, status.as_u16());
            class.delivery
        }
        Err(_) => {
            log_push_transport(&host, &attempt, attempt_at);
            PushDelivery::Failed
        }
    }
}

struct Classified {
    delivery: PushDelivery,
    outcome: &'static str,
    warn: bool,
}

fn classify(status: reqwest::StatusCode) -> Classified {
    let code = status.as_u16();
    if (200..300).contains(&code) {
        return Classified {
            delivery: PushDelivery::Accepted,
            outcome: "provider_accepted",
            warn: false,
        };
    }
    if code == 404 || code == 410 {
        return Classified {
            delivery: PushDelivery::Gone,
            outcome: "gone",
            warn: false,
        };
    }
    Classified {
        delivery: PushDelivery::Failed,
        outcome: "rejected",
        warn: true,
    }
}

fn log_push_http(
    host: &str,
    attempt: &PushAttemptIds<'_>,
    attempt_at: DateTime<Utc>,
    class: &Classified,
    status: u16,
) {
    let row_ids = join_row_ids(attempt.row_ids);
    let attempt_at = attempt_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    #[cfg(test)]
    let hold = tests::log_interest::hold();
    if class.warn {
        tracing::warn!(
            row_ids = %row_ids,
            subscription_id = attempt.subscription_id,
            push_host = %host,
            outcome = class.outcome,
            status,
            attempt_at = %attempt_at,
            "web_push_attempt"
        );
    } else {
        tracing::info!(
            row_ids = %row_ids,
            subscription_id = attempt.subscription_id,
            push_host = %host,
            outcome = class.outcome,
            status,
            attempt_at = %attempt_at,
            "web_push_attempt"
        );
    }
    #[cfg(test)]
    drop(hold);
}

fn log_push_transport(host: &str, attempt: &PushAttemptIds<'_>, attempt_at: DateTime<Utc>) {
    let row_ids = join_row_ids(attempt.row_ids);
    let attempt_at = attempt_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    #[cfg(test)]
    let hold = tests::log_interest::hold();
    tracing::warn!(
        row_ids = %row_ids,
        subscription_id = attempt.subscription_id,
        push_host = %host,
        outcome = "transport_error",
        attempt_at = %attempt_at,
        "web_push_attempt"
    );
    #[cfg(test)]
    drop(hold);
}

fn join_row_ids(ids: &[i64]) -> String {
    let mut out = String::new();
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&id.to_string());
    }
    out
}

struct Prepared {
    body: Vec<u8>,
    authorization: String,
    ttl: u64,
    urgency: String,
}

async fn post(
    keys: &VapidMaterial,
    request: &PushRequest<'_>,
) -> Result<reqwest::StatusCode, String> {
    let prepared = prepare(keys, request)?;
    let client = http_client()?;
    let response = client
        .post(request.endpoint)
        .header("Content-Type", "application/octet-stream")
        .header("Content-Encoding", "aes128gcm")
        .header("TTL", prepared.ttl.to_string())
        .header("Urgency", prepared.urgency)
        .header("Authorization", prepared.authorization)
        .body(prepared.body)
        .send()
        .await
        .map_err(transport_error)?;
    Ok(response.status())
}

fn prepare(keys: &VapidMaterial, request: &PushRequest<'_>) -> Result<Prepared, String> {
    let ua_public =
        super::b64::decode(request.p256dh).map_err(|_| "subscription key".to_string())?;
    let auth = super::b64::decode(request.auth).map_err(|_| "subscription auth".to_string())?;
    let salt = fresh_salt()?;
    let sealed = aesgcm::seal(SealParts {
        plaintext: request.payload,
        ua_public: &ua_public,
        auth: &auth,
        as_private: keys.private_scalar(),
        salt: &salt,
    })?;
    let origin = origin_of(request.endpoint)?;
    let authorization = keys.authorization(&origin, super::vapid::now_unix())?;
    Ok(Prepared {
        body: sealed.body,
        authorization,
        ttl: request.ttl_secs,
        urgency: request.urgency.to_string(),
    })
}

fn fresh_salt() -> Result<[u8; 16], String> {
    let mut salt = [0u8; 16];
    SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| "salt failed".to_string())?;
    Ok(salt)
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("liberado-web-push")
        .build()
        .map_err(|_| "web push client failed".to_string())
}

fn transport_error(err: reqwest::Error) -> String {
    liberado_common::redact_secrets(&err.without_url().to_string())
}

pub(crate) fn origin_of(endpoint: &str) -> Result<String, String> {
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return Err("push endpoint has no origin".into());
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || !matches!(scheme, "https" | "http") {
        return Err("push endpoint has no origin".into());
    }
    Ok(format!("{scheme}://{host}"))
}

pub(crate) fn host_of(endpoint: &str) -> String {
    origin_of(endpoint)
        .ok()
        .and_then(|origin| origin.split("://").nth(1).map(str::to_string))
        .unwrap_or_else(|| "push".into())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use liberado_outbox::{
        DIGEST, EVENT_LIST, EVENT_START, HABIT, JOB_ERROR, OutboxSource, REMINDER, TASK_DUE,
        TASK_LIST, TEST,
    };

    use super::*;
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const AS_PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const UA_PUBLIC: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

    fn keys() -> VapidMaterial {
        super::super::vapid::from_parts(AS_PUBLIC, AS_PRIVATE, "mailto:push@example.com")
            .expect("keys")
    }

    fn sample_input(kind: &str) -> ReminderPushInput {
        ReminderPushInput {
            title: "title".into(),
            body: "body".into(),
            tag: "row-1".into(),
            rows: vec![(1, "tok".into())],
            kind: kind.into(),
            outbox_id: Some(1),
            due_at: Some(Utc.with_ymd_and_hms(2026, 11, 1, 20, 0, 0).unwrap()),
            late: false,
            source: None,
            items: Vec::new(),
        }
    }

    fn parse_payload(json: &str) -> serde_json::Value {
        serde_json::from_str(json).expect("json")
    }

    #[tokio::test]
    async fn a_201_is_accepted_and_a_410_is_gone() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("content-encoding", "aes128gcm"))
            .and(header("ttl", "21600"))
            .and(header("urgency", "high"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        let endpoint = format!("{}/fake-subscription", server.uri());
        let payload = reminder_payload(&sample_input(TASK_DUE));
        let delivery = post_push(
            &keys(),
            PushRequest {
                endpoint: &endpoint,
                p256dh: UA_PUBLIC,
                auth: AUTH,
                payload: payload.as_bytes(),
                ttl_secs: REMINDER_TTL,
                urgency: "high",
            },
            PushAttemptIds {
                row_ids: &[1],
                subscription_id: 1,
            },
        )
        .await;
        assert_eq!(delivery, PushDelivery::Accepted);

        let gone = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(410))
            .expect(1)
            .mount(&gone)
            .await;
        let endpoint = format!("{}/fake-subscription", gone.uri());
        let delivery = post_push(
            &keys(),
            PushRequest {
                endpoint: &endpoint,
                p256dh: UA_PUBLIC,
                auth: AUTH,
                payload: payload.as_bytes(),
                ttl_secs: REMINDER_TTL,
                urgency: "high",
            },
            PushAttemptIds {
                row_ids: &[1],
                subscription_id: 1,
            },
        )
        .await;
        assert_eq!(delivery, PushDelivery::Gone);
    }

    #[test]
    fn legacy_keys_title_body_tag_rows_url_remain() {
        let json = reminder_payload(&ReminderPushInput {
            title: "Due now: Call dentist".into(),
            body: "Task due 2:00 PM · Tasks/Inbox.md".into(),
            tag: "row-42".into(),
            rows: vec![(42, "ab".into())],
            kind: TASK_DUE.into(),
            outbox_id: Some(42),
            due_at: Some(Utc.with_ymd_and_hms(2026, 11, 1, 20, 0, 0).unwrap()),
            late: false,
            source: Some(serde_json::json!({
                "type": "vault_note",
                "path": "Tasks/Inbox.md",
                "label": "due",
                "text": "Call dentist"
            })),
            items: Vec::new(),
        });
        let value = parse_payload(&json);
        let mut keys: Vec<_> = value.as_object().expect("object").keys().cloned().collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "actions",
                "body",
                "due_at",
                "kind",
                "late",
                "outbox_id",
                "rows",
                "snooze_minutes",
                "source",
                "tag",
                "title",
                "url",
                "v"
            ]
        );
        assert_eq!(value["snooze_minutes"], DEFAULT_SNOOZE_MINUTES);
        assert_eq!(
            value["actions"],
            serde_json::json!([
                {"action": "done", "title": "Done"},
                {"action": "snooze", "title": format!("Snooze {DEFAULT_SNOOZE_MINUTES}m")}
            ])
        );
        assert_eq!(value["title"], "Due now: Call dentist");
        assert_eq!(value["body"], "Task due 2:00 PM · Tasks/Inbox.md");
        assert_eq!(value["tag"], "row-42");
        assert_eq!(value["url"], "/notifications/42");
        assert_eq!(
            value["rows"],
            serde_json::json!([{"id": 42, "token": "ab"}])
        );
        assert!(!json.contains("p256dh"), "{json}");
        assert!(!json.contains("LIBERADO_"), "{json}");
        assert!(
            !json.contains("\"auth\""),
            "subscription auth must not appear in payload: {json}"
        );
    }

    #[test]
    fn digest_payload_exact_top_level_keys_and_rows() {
        let json = reminder_payload(&ReminderPushInput {
            title: "4 missed reminders".into(),
            body: "a\nb".into(),
            tag: "digest".into(),
            rows: vec![(1, "a".into()), (2, "b".into())],
            kind: DIGEST.into(),
            outbox_id: None,
            due_at: None,
            late: true,
            source: None,
            items: vec![(1, TASK_DUE.into(), "Due now: x".into())],
        });
        let value = parse_payload(&json);
        let mut keys: Vec<_> = value.as_object().expect("object").keys().cloned().collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "body",
                "due_at",
                "items",
                "kind",
                "late",
                "outbox_id",
                "rows",
                "tag",
                "title",
                "url",
                "v"
            ]
        );
        assert_eq!(
            value["rows"],
            serde_json::json!([{"id": 1, "token": "a"}, {"id": 2, "token": "b"}])
        );
        assert!(!json.contains("p256dh"), "{json}");
        assert!(!json.contains("LIBERADO_"), "{json}");
        assert!(
            !json.contains("\"auth\""),
            "subscription auth must not appear in payload: {json}"
        );
    }

    #[test]
    fn task_due_payload_matches_scope_shape() {
        let json = reminder_payload(&ReminderPushInput {
            title: "Due now: Call dentist".into(),
            body: "Task due 2:00 PM · Tasks/Inbox.md".into(),
            tag: "row-42".into(),
            rows: vec![(42, "tok".into())],
            kind: TASK_DUE.into(),
            outbox_id: Some(42),
            due_at: Some(Utc.with_ymd_and_hms(2026, 11, 1, 20, 0, 0).unwrap()),
            late: false,
            source: Some(
                serde_json::to_value(OutboxSource::VaultNote {
                    path: "Tasks/Inbox.md".into(),
                    label: "due".into(),
                    text: "Call dentist".into(),
                })
                .unwrap(),
            ),
            items: Vec::new(),
        });
        let value = parse_payload(&json);
        assert_eq!(value["v"], 2);
        assert_eq!(value["kind"], TASK_DUE);
        assert_eq!(value["outbox_id"], 42);
        assert_eq!(value["due_at"], "2026-11-01T20:00:00Z");
        assert_eq!(value["late"], false);
        assert_eq!(value["source"]["type"], "vault_note");
        assert!(value.get("actions").is_some());
    }

    #[test]
    fn tray_actions_are_only_on_a_single_completable_vault_task() {
        assert_eq!(TASK_DUE_KIND, liberado_outbox::TASK_DUE);
        let encoded = serde_json::to_value(OutboxSource::VaultNote {
            path: "Tasks/Inbox.md".into(),
            label: "due".into(),
            text: "Call dentist".into(),
        })
        .unwrap();
        assert_eq!(encoded["type"], VAULT_NOTE_SOURCE);

        let task = parse_payload(&reminder_payload(&ReminderPushInput {
            title: "Due now: Call dentist".into(),
            body: "body".into(),
            tag: "row-1".into(),
            rows: vec![(1, "tok".into())],
            kind: TASK_DUE.into(),
            outbox_id: Some(1),
            due_at: None,
            late: false,
            source: Some(encoded),
            items: Vec::new(),
        }));
        assert_eq!(task["actions"][0]["action"], "done");
        assert_eq!(
            task["actions"][1]["title"],
            format!("Snooze {DEFAULT_SNOOZE_MINUTES}m")
        );

        let without_actions = [
            sample_input(EVENT_START),
            sample_input(HABIT),
            sample_input(TEST),
            sample_input(REMINDER),
            sample_input(TASK_DUE),
            ReminderPushInput {
                rows: vec![(1, "a".into()), (2, "b".into())],
                kind: TASK_DUE.into(),
                source: Some(
                    serde_json::json!({"type": "vault_note", "path": "n", "label": "due", "text": "t"}),
                ),
                ..sample_input(TASK_DUE)
            },
            ReminderPushInput {
                kind: DIGEST.into(),
                outbox_id: None,
                rows: vec![(1, "a".into()), (2, "b".into())],
                source: None,
                items: vec![(1, TASK_DUE.into(), "Due now: x".into())],
                ..sample_input(DIGEST)
            },
        ];
        for input in without_actions {
            let value = parse_payload(&reminder_payload(&input));
            assert!(
                value.get("actions").is_none(),
                "kind {} must not offer tray actions",
                input.kind
            );
            assert!(value.get("snooze_minutes").is_none());
        }
    }

    #[test]
    fn detail_page_short_snooze_uses_the_tray_default() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../webui/src/components/notification_detail/mod.rs");
        let source = std::fs::read_to_string(path).expect("notification detail page");
        let needle = format!("Snooze {DEFAULT_SNOOZE_MINUTES}m");
        assert!(
            source.contains(&needle),
            "detail page short snooze must match DEFAULT_SNOOZE_MINUTES ({DEFAULT_SNOOZE_MINUTES})"
        );
    }

    #[test]
    fn event_start_payload_carries_expected_copy() {
        let json = reminder_payload(&ReminderPushInput {
            title: "Starting now: Standup".into(),
            body: "Event at 9:00 AM · Calendar/2026-11-01.md".into(),
            tag: "row-7".into(),
            rows: vec![(7, "t".into())],
            kind: EVENT_START.into(),
            outbox_id: Some(7),
            due_at: Some(Utc.with_ymd_and_hms(2026, 11, 1, 15, 0, 0).unwrap()),
            late: false,
            source: None,
            items: Vec::new(),
        });
        let value = parse_payload(&json);
        assert_eq!(value["title"], "Starting now: Standup");
        assert_eq!(value["body"], "Event at 9:00 AM · Calendar/2026-11-01.md");
        assert_eq!(value["kind"], EVENT_START);
    }

    #[test]
    fn habit_task_list_event_list_job_error_test_and_digest_payloads() {
        let cases: [(&str, &str, &str); 5] = [
            (
                HABIT,
                "Morning tasks",
                "Plan the day. · Morning tasks routine",
            ),
            (TASK_LIST, "Open tasks (3)", "Open tasks\n\n1. a"),
            (EVENT_LIST, "Upcoming events (2)", "Events through …"),
            (JOB_ERROR, "Morning tasks failed", "git push failed"),
            (
                TEST,
                "Liberado test",
                "Test notification from Notifications settings · 2:00 PM",
            ),
        ];
        for (kind_name, title, body) in cases {
            let json = reminder_payload(&ReminderPushInput {
                title: title.into(),
                body: body.into(),
                tag: "row-1".into(),
                rows: vec![(1, "t".into())],
                kind: kind_name.into(),
                outbox_id: Some(1),
                due_at: None,
                late: false,
                source: None,
                items: Vec::new(),
            });
            let value = parse_payload(&json);
            assert_eq!(value["title"], title, "kind {kind_name}");
            assert_eq!(value["body"], body, "kind {kind_name}");
        }
        let digest = reminder_payload(&ReminderPushInput {
            title: "4 missed reminders".into(),
            body: "a\nb".into(),
            tag: "digest".into(),
            rows: vec![(1, "a".into()), (2, "b".into())],
            kind: DIGEST.into(),
            outbox_id: None,
            due_at: None,
            late: true,
            source: None,
            items: vec![(1, TASK_DUE.into(), "Due now: x".into())],
        });
        let value = parse_payload(&digest);
        assert_eq!(value["title"], "4 missed reminders");
        assert_eq!(value["url"], "/notifications");
        assert!(value.get("items").is_some());
    }

    #[test]
    fn legacy_row_without_kind_reads_as_reminder_in_payload() {
        let json = reminder_payload(&ReminderPushInput {
            title: "Old title".into(),
            body: "Old body".into(),
            tag: "row-9".into(),
            rows: vec![(9, "t".into())],
            kind: REMINDER.into(),
            outbox_id: Some(9),
            due_at: None,
            late: false,
            source: None,
            items: Vec::new(),
        });
        let value = parse_payload(&json);
        assert_eq!(value["kind"], REMINDER);
        assert!(value.get("source").is_none());
    }

    #[test]
    fn worst_case_digest_body_stays_below_aes128gcm_plaintext_limit() {
        let body = "x".repeat(1800);
        let json = reminder_payload(&ReminderPushInput {
            title: "10 missed reminders".into(),
            body,
            tag: "digest".into(),
            rows: (1..=10).map(|id| (id, format!("tok{id}"))).collect(),
            kind: DIGEST.into(),
            outbox_id: None,
            due_at: None,
            late: true,
            source: None,
            items: (1..=10)
                .map(|id| (id, TASK_DUE.into(), format!("Due now: item {id}")))
                .collect(),
        });
        assert!(json.len() < 3993, "payload {} bytes", json.len());
    }

    pub(crate) mod log_interest {
        use std::cell::Cell;
        use std::sync::{Mutex, MutexGuard};

        static LOCK: Mutex<()> = Mutex::new(());

        thread_local! {
            static CAPTURE: Cell<bool> = const { Cell::new(false) };
        }

        pub(crate) fn set_capture(on: bool) {
            CAPTURE.with(|flag| flag.set(on));
        }

        pub(crate) fn hold() -> MutexGuard<'static, ()> {
            let guard = LOCK.lock().expect("log interest lock");
            if CAPTURE.with(|flag| flag.get()) {
                tracing::callsite::rebuild_interest_cache();
            }
            guard
        }
    }

    struct PushLogSession {
        logs: LogCapture,
        _guard: tracing::subscriber::DefaultGuard,
    }

    impl PushLogSession {
        fn arm() -> Self {
            let logs = LogCapture::new();
            log_interest::set_capture(true);
            let guard = tracing::subscriber::set_default(logs.clone());
            Self {
                logs,
                _guard: guard,
            }
        }

        fn text(&self) -> String {
            self.logs.text()
        }
    }

    impl Drop for PushLogSession {
        fn drop(&mut self) {
            log_interest::set_capture(false);
        }
    }

    #[derive(Clone)]
    struct LogCapture {
        lines: std::sync::Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>,
    }

    struct FieldVisit(Vec<String>);

    impl tracing::field::Visit for FieldVisit {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push(format!("{}={value}", field.name()));
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.0.push(format!("{}={value}", field.name()));
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.0.push(format!("{}={value}", field.name()));
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push(format!("{}={value:?}", field.name()));
        }
    }

    impl LogCapture {
        fn new() -> Self {
            Self {
                lines: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn text(&self) -> String {
            self.lines
                .lock()
                .expect("log lock")
                .iter()
                .map(|(_, line)| line.clone())
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    impl tracing::Subscriber for LogCapture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn register_callsite(
            &self,
            _: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::sometimes()
        }

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visit = FieldVisit(Vec::new());
            event.record(&mut visit);
            self.lines
                .lock()
                .expect("log lock")
                .push((*event.metadata().level(), visit.0.join(" ")));
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    fn attempt_ids() -> PushAttemptIds<'static> {
        PushAttemptIds {
            row_ids: &[42, 43],
            subscription_id: 9,
        }
    }

    fn push_request<'a>(endpoint: &'a str, payload: &'a [u8]) -> PushRequest<'a> {
        PushRequest {
            endpoint,
            p256dh: UA_PUBLIC,
            auth: AUTH,
            payload,
            ttl_secs: REMINDER_TTL,
            urgency: "high",
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn web_push_attempt_logs_provider_accepted_without_secrets() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        let endpoint = format!("{}/SECRET_ENDPOINT_PATH", server.uri());
        let payload = reminder_payload(&ReminderPushInput {
            title: "SECRET_TITLE_DO_NOT_LOG".into(),
            body: "SECRET_BODY_DO_NOT_LOG".into(),
            tag: "row-42".into(),
            rows: vec![(42, "SECRET_PUSH_TOKEN".into())],
            kind: TASK_DUE.into(),
            outbox_id: Some(42),
            due_at: None,
            late: false,
            source: None,
            items: Vec::new(),
        });
        let session = PushLogSession::arm();
        let delivery = post_push(
            &keys(),
            push_request(&endpoint, payload.as_bytes()),
            attempt_ids(),
        )
        .await;
        assert_eq!(delivery, PushDelivery::Accepted);
        let text = session.text();
        assert!(text.contains("message=web_push_attempt"), "{text}");
        assert!(text.contains("outcome=provider_accepted"), "{text}");
        assert!(text.contains("row_ids=42,43"), "{text}");
        assert!(text.contains("subscription_id=9"), "{text}");
        assert!(text.contains("status=201"), "{text}");
        assert!(text.contains("attempt_at="), "{text}");
        assert!(!text.contains("SECRET"), "{text}");
        assert!(!text.contains(AUTH), "{text}");
        assert!(!text.contains(UA_PUBLIC), "{text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn web_push_attempt_logs_gone_and_rejected_without_secrets() {
        let gone = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(410))
            .expect(1)
            .mount(&gone)
            .await;
        let rejected = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400))
            .expect(1)
            .mount(&rejected)
            .await;
        let payload = b"SECRET_TITLE_DO_NOT_LOG";
        let session = PushLogSession::arm();
        let gone_endpoint = format!("{}/SECRET_ENDPOINT_PATH", gone.uri());
        let gone_delivery = post_push(
            &keys(),
            push_request(&gone_endpoint, payload),
            attempt_ids(),
        )
        .await;
        let rejected_endpoint = format!("{}/SECRET_ENDPOINT_PATH", rejected.uri());
        let rejected_delivery = post_push(
            &keys(),
            push_request(&rejected_endpoint, payload),
            attempt_ids(),
        )
        .await;
        assert_eq!(gone_delivery, PushDelivery::Gone);
        assert_eq!(rejected_delivery, PushDelivery::Failed);
        let text = session.text();
        assert!(text.contains("outcome=gone"), "{text}");
        assert!(text.contains("status=410"), "{text}");
        assert!(text.contains("outcome=rejected"), "{text}");
        assert!(text.contains("status=400"), "{text}");
        assert!(text.contains("row_ids=42,43"), "{text}");
        assert!(text.contains("subscription_id=9"), "{text}");
        assert!(!text.contains("SECRET"), "{text}");
        assert!(!text.contains(AUTH), "{text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn web_push_attempt_logs_transport_error_without_the_endpoint() {
        let session = PushLogSession::arm();
        let endpoint = "http://127.0.0.1:1/SECRET_ENDPOINT_PATH";
        let delivery = post_push(
            &keys(),
            push_request(endpoint, b"SECRET_PUSH_TOKEN"),
            attempt_ids(),
        )
        .await;
        assert_eq!(delivery, PushDelivery::Failed);
        let text = session.text();
        assert!(text.contains("outcome=transport_error"), "{text}");
        assert!(text.contains("row_ids=42,43"), "{text}");
        assert!(text.contains("subscription_id=9"), "{text}");
        assert!(text.contains("push_host=127.0.0.1:1"), "{text}");
        assert!(!text.contains("SECRET"), "{text}");
        assert!(!text.contains("status="), "{text}");
    }
}
