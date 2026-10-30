//! One Web Push POST.
//!
//! Ported from `origin/feat/weekly-spending-push` @ `ac9a7fa4` (PR #317). The week
//! topic and the JSON subscription file stayed on that branch. This file sends one
//! payload. 2xx is acceptance. 404 and 410 mean the subscription is gone.

use std::time::Duration;

use aws_lc_rs::rand::{SecureRandom, SystemRandom};

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

/// JSON object whose keys are only the reminder fields. Callers pass the bytes.
pub fn reminder_payload(title: &str, body: &str, tag: &str, rows: &str, url: &str) -> String {
    serde_json::json!({
        "title": title,
        "body": body,
        "tag": tag,
        "rows": serde_json::from_str::<serde_json::Value>(rows).unwrap_or(serde_json::Value::Array(Vec::new())),
        "url": url,
    })
    .to_string()
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

async fn post(keys: &VapidMaterial, request: &PushRequest<'_>) -> Result<reqwest::StatusCode, String> {
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
    let ua_public = super::b64::decode(request.p256dh).map_err(|_| "subscription key".to_string())?;
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
        let payload = reminder_payload("Reminder", "call home", "row-1", "[]", "/");
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
    fn the_payload_keeps_only_the_reminder_fields() {
        let body = reminder_payload("Reminder", "due 09:00 — notes/a.md", "row-1", r#"[{"id":1,"token":"ab"}]"#, "/");
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        let keys: Vec<_> = value.as_object().expect("object").keys().cloned().collect();
        assert_eq!(keys, ["title", "body", "tag", "rows", "url"]);
        assert!(body.contains("notes/a.md"));
        assert!(!body.contains("p256dh"));
        assert!(!body.contains("LIBERADO_"));
    }
}
