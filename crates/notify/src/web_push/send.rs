//! One Web Push POST. 2xx records success. 404 and 410 drop the subscription.

use std::time::Duration;

use aws_lc_rs::rand::{SecureRandom, SystemRandom};

use super::aesgcm::{self, SealParts};
use super::store::{self, Subscription};
use super::vapid::VapidMaterial;

const WEEK_TTL: u64 = 86_400;
const TEST_TTL: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Ok,
    Gone,
    Failed,
}

pub(crate) struct Outgoing<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub ttl: u64,
    pub topic: Option<String>,
}

pub(crate) fn weekly<'a>(title: &'a str, body: &'a str, week_id: &str) -> Outgoing<'a> {
    Outgoing {
        title,
        body,
        ttl: WEEK_TTL,
        topic: Some(topic_for(week_id)),
    }
}

pub(crate) fn test_note<'a>(title: &'a str, body: &'a str) -> Outgoing<'a> {
    Outgoing {
        title,
        body,
        ttl: TEST_TTL,
        topic: None,
    }
}

fn topic_for(week_id: &str) -> String {
    let mut out = String::from("spend");
    for ch in week_id.chars() {
        push_alnum(&mut out, ch);
    }
    out.truncate(32);
    out
}

fn push_alnum(out: &mut String, ch: char) {
    if ch.is_ascii_alphanumeric() {
        out.push(ch);
    }
}

pub(crate) async fn deliver(
    keys: &VapidMaterial,
    sub: &Subscription,
    note: &Outgoing<'_>,
) -> Delivery {
    let host = store::host_of(&sub.endpoint);
    match post(keys, sub, note).await {
        Ok(status) => classified(&host, status),
        Err(err) => {
            tracing::warn!(host = %host, %err, "web push transport failed");
            Delivery::Failed
        }
    }
}

fn classified(host: &str, status: reqwest::StatusCode) -> Delivery {
    let code = status.as_u16();
    if (200..300).contains(&code) {
        return Delivery::Ok;
    }
    if code == 404 || code == 410 {
        tracing::info!(host = %host, status = code, "web push subscription is gone");
        return Delivery::Gone;
    }
    tracing::warn!(host = %host, status = code, "web push was rejected");
    Delivery::Failed
}

struct Prepared {
    body: Vec<u8>,
    authorization: String,
    ttl: u64,
    topic: Option<String>,
}

async fn post(
    keys: &VapidMaterial,
    sub: &Subscription,
    note: &Outgoing<'_>,
) -> Result<reqwest::StatusCode, String> {
    let prepared = prepare_post(keys, sub, note)?;
    send_prepared(&sub.endpoint, prepared).await
}

fn prepare_post(
    keys: &VapidMaterial,
    sub: &Subscription,
    note: &Outgoing<'_>,
) -> Result<Prepared, String> {
    let body = encrypted(keys, sub, note)?;
    let origin = store::origin(&sub.endpoint)?;
    let authorization = keys.authorization(&origin, super::vapid::now_unix())?;
    Ok(Prepared {
        body,
        authorization,
        ttl: note.ttl,
        topic: note.topic.clone(),
    })
}

async fn send_prepared(endpoint: &str, prepared: Prepared) -> Result<reqwest::StatusCode, String> {
    let client = http_client()?;
    let response = with_topic(client.post(endpoint), prepared.topic.as_deref())
        .header("Content-Type", "application/octet-stream")
        .header("Content-Encoding", "aes128gcm")
        .header("TTL", prepared.ttl.to_string())
        .header("Authorization", prepared.authorization)
        .body(prepared.body)
        .send()
        .await
        .map_err(transport_error)?;
    Ok(response.status())
}

fn transport_error(err: reqwest::Error) -> String {
    liberado_common::redact_secrets(&err.without_url().to_string())
}

fn encrypted(
    keys: &VapidMaterial,
    sub: &Subscription,
    note: &Outgoing<'_>,
) -> Result<Vec<u8>, String> {
    let ua_public = super::b64::decode(&sub.p256dh).map_err(|_| "subscription key".to_string())?;
    let auth = super::b64::decode(&sub.auth).map_err(|_| "subscription auth".to_string())?;
    let salt = fresh_salt()?;
    let payload = serde_json::json!({
        "title": note.title,
        "body": note.body,
        "data": { "url": "/" }
    });
    let sealed = aesgcm::seal(SealParts {
        plaintext: payload.to_string().as_bytes(),
        ua_public: &ua_public,
        auth: &auth,
        as_private: keys.private_scalar(),
        salt: &salt,
    })?;
    Ok(sealed.body)
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

fn with_topic(builder: reqwest::RequestBuilder, topic: Option<&str>) -> reqwest::RequestBuilder {
    match topic {
        Some(topic) => builder.header("Topic", topic),
        None => builder,
    }
}
