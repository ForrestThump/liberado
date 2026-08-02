//! Thin HTTP client over the daemon's public API. No daemon crates linked.

use std::time::Duration;

use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;

use chat_client_contract::{DaemonStatus, ReactionEvent, ReactionOutcome};

#[derive(Debug, Clone)]
pub struct DaemonClient {
    http: Client,
    base: String,
}

impl DaemonClient {
    pub fn new(base_url: &str) -> Result<Self, String> {
        let base = base_url.trim_end_matches('/').to_string();
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Self { http, base })
    }

    pub async fn status(&self) -> Result<DaemonStatus, String> {
        self.get_json("/api/status").await
    }

    pub async fn reactions(&self) -> Result<Vec<ReactionEvent>, String> {
        self.get_json("/api/reactions").await
    }

    pub async fn goal(&self, id: &str) -> Result<Value, String> {
        self.get_json(&format!("/api/goals/{id}")).await
    }

    /// Open the goal SSE briefly to prove the session is joinable (P3).
    pub async fn goal_stream_accepts(&self, id: &str) -> Result<(), String> {
        let url = format!("{}/api/goals/{id}/stream", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("GET stream: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("GET stream status {}", resp.status()));
        }
        // Read at least one chunk or idle — either proves the hub accepted the subscriber.
        let mut stream = resp.bytes_stream();
        let _ = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
        Ok(())
    }

    pub async fn trigger_hook(
        &self,
        name: &str,
        secret: &str,
        run_id: &str,
    ) -> Result<Value, String> {
        let url = format!("{}/api/hooks/{name}", self.base);
        let body = serde_json::json!({ "context": format!("run_id={run_id}") });
        let resp = self
            .http
            .post(&url)
            .header("X-Liberado-Hook-Secret", secret)
            .header("X-Liberado-Idempotency-Key", run_id)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("POST hook: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("POST hook {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(|e| format!("hook body: {e} ({text})"))
    }

    pub async fn start_goal(&self, body: Value) -> Result<Value, String> {
        let url = format!("{}/api/goals", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("POST goals: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("POST goals {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(|e| format!("goals body: {e} ({text})"))
    }

    /// POST /api/chat/stream; returns (session_id, saw_token, model_from_assistant_if_any).
    pub async fn chat_turn(
        &self,
        message: &str,
        background: bool,
    ) -> Result<ChatTurnOutcome, String> {
        let url = format!("{}/api/chat/stream", self.base);
        let body = serde_json::json!({
            "message": message,
            "background": background,
        });
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("POST chat/stream: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("chat/stream {status}: {text}"));
        }

        let mut session_id: Option<String> = None;
        let mut saw_token = false;
        let mut buffer = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("sse chunk: {e}"))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(idx) = buffer.find("\n\n") {
                let block = buffer[..idx].to_string();
                buffer = buffer[idx + 2..].to_string();
                parse_sse_block(&block, &mut session_id, &mut saw_token);
            }
        }
        // Trailing block without blank line.
        if !buffer.trim().is_empty() {
            parse_sse_block(&buffer, &mut session_id, &mut saw_token);
        }

        Ok(ChatTurnOutcome {
            session_id: session_id
                .ok_or_else(|| "chat stream never announced a session id".to_string())?,
            saw_token,
        })
    }

    pub async fn conversation(&self, id: &str) -> Result<Value, String> {
        self.get_json(&format!("/api/conversations/{id}")).await
    }

    pub async fn sessions(&self) -> Result<Value, String> {
        self.get_json("/api/sessions").await
    }

    /// Poll reactions until a Dispatched outcome appears for `correlation_id`, or timeout.
    pub async fn wait_dispatched(
        &self,
        correlation_id: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let start = std::time::Instant::now();
        loop {
            let reactions = self.reactions().await?;
            for r in &reactions {
                if r.correlation_id == correlation_id
                    && let ReactionOutcome::Dispatched { session_id } = &r.outcome
                {
                    return Ok(session_id.clone());
                }
            }
            if start.elapsed() > timeout {
                return Err(format!(
                    "no Dispatched for correlation_id={correlation_id} within {timeout:?}"
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Poll GET /api/goals/{id} until terminal or timeout.
    pub async fn wait_goal_terminal(
        &self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<Value, String> {
        let start = std::time::Instant::now();
        loop {
            let snap = self.goal(session_id).await?;
            let status = snap
                .pointer("/session/status")
                .or_else(|| snap.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if matches!(
                status,
                "succeeded" | "failed" | "cancelled" | "budget_exhausted"
            ) {
                return Ok(snap);
            }
            if start.elapsed() > timeout {
                return Err(format!(
                    "session {session_id} not terminal after {timeout:?} (last status={status})"
                ));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let url = format!("{}{path}", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("GET {path}: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("GET {path} {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(|e| format!("GET {path} parse: {e} ({text})"))
    }
}

#[derive(Debug)]
pub struct ChatTurnOutcome {
    pub session_id: String,
    pub saw_token: bool,
}

fn parse_sse_block(block: &str, session_id: &mut Option<String>, saw_token: &mut bool) {
    let mut event_name = String::new();
    let mut data = String::new();
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }
    if data.is_empty() {
        return;
    }
    // session announcement: event name "session" or data {"session":"..."}
    if event_name == "session" || event_name.is_empty() {
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            if let Some(s) = v.get("session").and_then(|x| x.as_str()) {
                *session_id = Some(s.to_string());
            }
            if let Some(s) = v.get("session_id").and_then(|x| x.as_str()) {
                *session_id = Some(s.to_string());
            }
        } else if event_name == "session" {
            *session_id = Some(data.trim().trim_matches('"').to_string());
        }
    }
    if event_name == "token" || data.contains("token") {
        *saw_token = true;
    }
    // chat-client-contract: token events may be bare text under event: token
    if event_name == "token" {
        *saw_token = true;
    }
}

/// Extract Dispatched session_id from a reaction list for a correlation id (pure; unit-testable).
pub fn find_dispatched(reactions: &[ReactionEvent], correlation_id: &str) -> Option<String> {
    reactions.iter().find_map(|r| {
        if r.correlation_id == correlation_id
            && let ReactionOutcome::Dispatched { session_id } = &r.outcome
        {
            return Some(session_id.clone());
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_client_contract::ReactionOutcome;

    #[test]
    fn find_dispatched_matches_correlation() {
        let reactions = vec![ReactionEvent {
            event_type: "WebhookFired".into(),
            timestamp: "t".into(),
            source: "webhook:conformance".into(),
            correlation_id: "webhook:conformance:abc".into(),
            path: None,
            outcome: ReactionOutcome::Dispatched {
                session_id: "sess-1".into(),
            },
        }];
        assert_eq!(
            find_dispatched(&reactions, "webhook:conformance:abc").as_deref(),
            Some("sess-1")
        );
        assert!(find_dispatched(&reactions, "other").is_none());
    }
}
