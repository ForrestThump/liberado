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

    /// Parse conversation history flags used by P6 (turn lifecycle + transcript roles).
    pub async fn conversation_snapshot(&self, id: &str) -> Result<ConversationSnapshot, String> {
        let v = self.conversation(id).await?;
        Ok(ConversationSnapshot::from_json(&v))
    }

    pub async fn sessions(&self) -> Result<Value, String> {
        self.get_json("/api/sessions").await
    }

    /// Start a **background** chat turn, read until the session id is announced, then **drop** the
    /// SSE body so the connection ends while the turn (if durable) keeps running.
    ///
    /// P6 landmine: pre-durable turns died on disconnect. The suite must leave nobody attached
    /// and still observe `turn_running`.
    pub async fn start_background_turn_drop_stream(&self, message: &str) -> Result<String, String> {
        let url = format!("{}/api/chat/stream", self.base);
        let body = serde_json::json!({
            "message": message,
            "background": true,
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
            if session_id.is_some() {
                // Drop the stream here — connection closes; durable turn must keep running.
                break;
            }
        }
        let _ = saw_token;
        session_id.ok_or_else(|| "chat stream never announced a session id before drop".into())
    }

    /// Poll until `turn_running` is true, or timeout.
    pub async fn wait_turn_running(&self, id: &str, timeout: Duration) -> Result<(), String> {
        let start = std::time::Instant::now();
        loop {
            let snap = self.conversation_snapshot(id).await?;
            if snap.turn_running {
                return Ok(());
            }
            // Already finished with a reply: cannot prove outlive for this attempt.
            if snap.has_assistant && !snap.turn_running {
                return Err(
                    "turn finished before we observed turn_running (assistant already present)"
                        .into(),
                );
            }
            if start.elapsed() > timeout {
                return Err(format!(
                    "turn_running never became true for {id} within {timeout:?} \
                     (turn_unanswered={}, has_assistant={})",
                    snap.turn_unanswered, snap.has_assistant
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Poll until `turn_running` is false, or timeout.
    pub async fn wait_turn_not_running(&self, id: &str, timeout: Duration) -> Result<(), String> {
        let start = std::time::Instant::now();
        loop {
            let snap = self.conversation_snapshot(id).await?;
            if !snap.turn_running {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err(format!("turn still running for {id} after {timeout:?}"));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// `GET /api/conversations/{id}/attach` — collect SSE until done/timeout.
    ///
    /// Uses the same event vocabulary as chat stream. "Replay before live" is observable as
    /// receiving content/events while the turn was already started (we do not invent a second
    /// decoder). Empty/409 attach is a fail for P6.
    pub async fn attach_and_collect(
        &self,
        id: &str,
        timeout: Duration,
    ) -> Result<AttachCollect, String> {
        let url = format!("{}/api/conversations/{id}/attach", self.base);
        let resp = self
            .http
            .get(&url)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| format!("GET attach: {e}"))?;
        let status = resp.status();
        if status.as_u16() == 409 {
            return Err("attach 409: nothing running".into());
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("attach {status}: {text}"));
        }

        let mut session_id: Option<String> = None;
        let mut saw_token = false;
        let mut event_blocks = 0usize;
        let mut buffer = String::new();
        let mut stream = resp.bytes_stream();
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(
                deadline.saturating_duration_since(std::time::Instant::now()),
                stream.next(),
            )
            .await
            {
                Ok(Some(Ok(chunk))) => {
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(idx) = buffer.find("\n\n") {
                        let block = buffer[..idx].to_string();
                        buffer = buffer[idx + 2..].to_string();
                        if !block.trim().is_empty() {
                            event_blocks += 1;
                        }
                        parse_sse_block(&block, &mut session_id, &mut saw_token);
                    }
                }
                Ok(Some(Err(e))) => return Err(format!("attach stream: {e}")),
                Ok(None) => break,
                Err(_) => break,
            }
            // Enough signal: session + content, or several blocks (replay).
            if saw_token || event_blocks >= 2 {
                break;
            }
        }
        Ok(AttachCollect {
            event_blocks,
            saw_token,
            session_echo: session_id,
        })
    }

    /// `POST /api/conversations/{id}/cancel`.
    pub async fn cancel_conversation(&self, id: &str) -> Result<(), String> {
        let url = format!("{}/api/conversations/{id}/cancel", self.base);
        let resp = self
            .http
            .post(&url)
            .send()
            .await
            .map_err(|e| format!("POST cancel: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("cancel {status}: {text}"));
        }
        Ok(())
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

/// Snapshot of GET /api/conversations/{id} fields P6 asserts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSnapshot {
    pub turn_running: bool,
    pub turn_unanswered: bool,
    pub has_user: bool,
    pub has_assistant: bool,
    pub user_contents: Vec<String>,
    pub assistant_contents: Vec<String>,
}

impl ConversationSnapshot {
    pub fn from_json(v: &Value) -> Self {
        let turn_running = v
            .get("turn_running")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let turn_unanswered = v
            .get("turn_unanswered")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let mut user_contents = Vec::new();
        let mut assistant_contents = Vec::new();
        if let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) {
            for m in msgs {
                let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
                let content = m
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                match role {
                    "user" if !content.is_empty() => user_contents.push(content),
                    "assistant" if !content.trim().is_empty() => assistant_contents.push(content),
                    _ => {}
                }
            }
        }
        Self {
            turn_running,
            turn_unanswered,
            has_user: !user_contents.is_empty(),
            has_assistant: !assistant_contents.is_empty(),
            user_contents,
            assistant_contents,
        }
    }

    /// Cancel rollback ground truth: question kept, no assistant text persisted.
    pub fn cancel_left_question_without_reply(&self) -> bool {
        self.has_user && !self.has_assistant && !self.turn_running
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachCollect {
    pub event_blocks: usize,
    pub saw_token: bool,
    pub session_echo: Option<String>,
}

impl AttachCollect {
    /// Attach produced observable stream content (replay and/or live).
    pub fn has_content(&self) -> bool {
        self.saw_token || self.event_blocks > 0
    }
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

    #[test]
    fn conversation_snapshot_parses_lifecycle_and_roles() {
        let v = serde_json::json!({
            "messages": [
                {"role": "user", "content": "q"},
                {"role": "assistant", "content": "a"}
            ],
            "turn_running": true,
            "turn_unanswered": false
        });
        let s = ConversationSnapshot::from_json(&v);
        assert!(s.turn_running);
        assert!(!s.turn_unanswered);
        assert!(s.has_user && s.has_assistant);
        assert!(!s.cancel_left_question_without_reply());
    }

    #[test]
    fn cancel_rollback_requires_user_without_assistant() {
        let ok = ConversationSnapshot::from_json(&serde_json::json!({
            "messages": [{"role": "user", "content": "only question"}],
            "turn_running": false,
            "turn_unanswered": true
        }));
        assert!(ok.cancel_left_question_without_reply());

        let partial = ConversationSnapshot::from_json(&serde_json::json!({
            "messages": [
                {"role": "user", "content": "q"},
                {"role": "assistant", "content": "partial…"}
            ],
            "turn_running": false
        }));
        assert!(
            !partial.cancel_left_question_without_reply(),
            "partial assistant must fail rollback assert"
        );
    }

    #[test]
    fn attach_collect_empty_has_no_content() {
        assert!(
            !AttachCollect {
                event_blocks: 0,
                saw_token: false,
                session_echo: None,
            }
            .has_content()
        );
        assert!(
            AttachCollect {
                event_blocks: 1,
                saw_token: false,
                session_echo: None,
            }
            .has_content()
        );
    }
}
