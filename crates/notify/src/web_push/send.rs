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

/// Slice 2 will return `/notifications/<id>` for actionable kinds; keep `/` until that route ships.
pub fn notification_url(_kind: &str, _outbox_id: i64) -> String {
    "/".to_string()
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
    value.to_string()
}

pub async fn post_push(keys: &VapidMaterial, request: PushRequest<'_>) -> PushDelivery {
    let host = host_of(request.endpoint);
    match post(keys, &request).await {
        Ok(status) => classified(&host, status),
        Err(err) => {
            tracing::warn!(host = %host, %err, "web push transport failed");
            PushDelivery::Failed
        }
    }
}

fn classified(host: &str, status: reqwest::StatusCode) -> PushDelivery {
    let code = status.as_u16();
    if (200..300).contains(&code) {
        return PushDelivery::Accepted;
    }
    if code == 404 || code == 410 {
        tracing::info!(host = %host, status = code, "web push subscription is gone");
        return PushDelivery::Gone;
    }
    tracing::warn!(host = %host, status = code, "web push was rejected");
    PushDelivery::Failed
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
        assert_eq!(value["title"], "Due now: Call dentist");
        assert_eq!(value["body"], "Task due 2:00 PM · Tasks/Inbox.md");
        assert_eq!(value["tag"], "row-42");
        assert_eq!(value["url"], "/");
        assert!(value.get("rows").is_some());
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
}
