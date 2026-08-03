//! P7 — a chat turn survives, or is honestly reported, across a daemon restart.
//!
//! Against a live daemon (when `restart_command` is configured):
//!
//! 1. start a **background** chat turn and drop the SSE after the session id;
//! 2. run the configured restart command (host-specific — never hard-coded);
//! 3. while the daemon is down/draining, probe `POST /api/chat` until we observe
//!    `503` + `error: shutting_down` (or fail if we never do before it is fully up again);
//! 4. wait until the daemon is up again;
//! 5. assert lifecycle flags: **`turn_running` is false**, and either an assistant reply is on
//!    the transcript **or** `turn_unanswered` is true — never a silent lost turn.
//!
//! Unconfigured restart hook → [`PathStatus::Skipped`] with a reason (never Pass).
//!
//! Envelope: opt-in only (not in `all_default()`); only the session this run creates.

use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::client::{ConversationSnapshot, DaemonClient};
use crate::config::ConformanceConfig;
use crate::paths::elapsed_ms;
use crate::result::{PathId, PathResult};

/// Long enough that a real restart is likely mid-turn on a live box.
const RESTART_PROMPT: &str = "Write a careful multi-paragraph explanation of graceful shutdown \
for durable chat turns. Use at least six short paragraphs. Do not refuse.";

/// Pure check used by the live path and forced-fail matrix (same code path).
pub fn post_restart_lifecycle_ok(snap: &ConversationSnapshot) -> bool {
    snap.restart_lifecycle_honest()
}

pub async fn run(client: &DaemonClient, cfg: &ConformanceConfig, timeout: Duration) -> PathResult {
    let start = Instant::now();

    let Some(restart_cmd) = cfg.restart_command() else {
        return PathResult::skipped(
            PathId::P7,
            "restart_command unset or empty in conformance.toml — P7 is opt-in and will not \
             restart the daemon by surprise; configure a host-specific restart hook to run it",
        );
    };

    // ── 1. Background turn ───────────────────────────────────────────────────
    let session = match client
        .start_background_turn_drop_stream(RESTART_PROMPT)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return PathResult::fail(
                PathId::P7,
                "POST /api/chat/stream background, drop after session id",
                elapsed_ms(start),
                serde_json::json!({"error": e}),
            );
        }
    };

    // Best-effort: observe running before we kill the process (not required if the model is fast).
    let _ = client
        .wait_turn_running(&session, Duration::from_secs(15).min(timeout))
        .await;

    // ── 2. Restart ───────────────────────────────────────────────────────────
    if let Err(e) = run_shell_command(restart_cmd).await {
        return PathResult::fail(
            PathId::P7,
            "execute configured restart_command",
            elapsed_ms(start),
            serde_json::json!({"error": e, "restart_command": restart_cmd, "session_id": session}),
        );
    }

    // ── 3. Drain window: must refuse new turns with shutting_down ────────────
    let drain_deadline = Instant::now() + timeout.min(Duration::from_secs(120));
    let mut saw_shutting_down = false;
    let mut last_probe = serde_json::json!({});
    while Instant::now() < drain_deadline {
        match client.post_chat("p7 probe during restart").await {
            Ok((status, body)) => {
                last_probe = serde_json::json!({ "status": status, "body": body });
                if DaemonClient::is_shutting_down_response(status, &body) {
                    saw_shutting_down = true;
                    break;
                }
                // If the daemon is already fully back and accepting, stop probing — we either
                // missed the window or the gate never closed.
                if status == 200 {
                    break;
                }
            }
            Err(e) => {
                last_probe = serde_json::json!({ "error": e });
                // Connection errors are expected while the process is down; keep probing.
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    if !saw_shutting_down {
        return PathResult::fail(
            PathId::P7,
            "during restart/drain, POST /api/chat returns 503 with error=shutting_down",
            elapsed_ms(start),
            serde_json::json!({
                "session_id": session,
                "last_probe": last_probe,
                "restart_command": restart_cmd,
            }),
        );
    }

    // ── 4. Wait for daemon up ────────────────────────────────────────────────
    let up_timeout = timeout
        .saturating_sub(start.elapsed())
        .max(Duration::from_secs(60));
    if let Err(e) = client.wait_until_up(up_timeout).await {
        return PathResult::fail(
            PathId::P7,
            "daemon comes back after restart (GET /api/status)",
            elapsed_ms(start),
            serde_json::json!({"error": e, "session_id": session}),
        );
    }

    // ── 5. Honest lifecycle ──────────────────────────────────────────────────
    let snap = match client.conversation_snapshot(&session).await {
        Ok(s) => s,
        Err(e) => {
            return PathResult::fail(
                PathId::P7,
                "GET /api/conversations/{id} after restart",
                elapsed_ms(start),
                serde_json::json!({"error": e, "session_id": session}),
            );
        }
    };

    if !post_restart_lifecycle_ok(&snap) {
        return PathResult::fail(
            PathId::P7,
            "after restart: turn_running false and (assistant reply OR turn_unanswered) — \
             never a lost turn or a zombie turn_running",
            elapsed_ms(start),
            serde_json::json!({
                "session_id": session,
                "turn_running": snap.turn_running,
                "turn_unanswered": snap.turn_unanswered,
                "has_user": snap.has_user,
                "has_assistant": snap.has_assistant,
            }),
        );
    }

    PathResult::pass(
        PathId::P7,
        "restart observed shutting_down; post-restart lifecycle honest",
        elapsed_ms(start),
        serde_json::json!({
            "session_id": session,
            "saw_shutting_down": true,
            "turn_running": snap.turn_running,
            "turn_unanswered": snap.turn_unanswered,
            "has_assistant": snap.has_assistant,
            "residue": "one background conversation created by this run only",
        }),
    )
}

async fn run_shell_command(cmd: &str) -> Result<(), String> {
    #[cfg(windows)]
    let mut child = tokio::process::Command::new("cmd")
        .args(["/C", cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn restart_command: {e}"))?;

    #[cfg(not(windows))]
    let mut child = tokio::process::Command::new("sh")
        .args(["-c", cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn restart_command: {e}"))?;

    let status = child
        .wait()
        .await
        .map_err(|e| format!("wait restart_command: {e}"))?;
    if !status.success() {
        return Err(format!("restart_command exited with {status}: {cmd}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ConversationSnapshot;

    #[test]
    fn honest_when_assistant_present() {
        let snap = ConversationSnapshot {
            turn_running: false,
            turn_unanswered: false,
            has_user: true,
            has_assistant: true,
            user_contents: vec!["q".into()],
            assistant_contents: vec!["a".into()],
        };
        assert!(post_restart_lifecycle_ok(&snap));
    }

    #[test]
    fn honest_when_unanswered_not_running() {
        let snap = ConversationSnapshot {
            turn_running: false,
            turn_unanswered: true,
            has_user: true,
            has_assistant: false,
            user_contents: vec!["q".into()],
            assistant_contents: vec![],
        };
        assert!(post_restart_lifecycle_ok(&snap));
    }

    #[test]
    fn dishonest_when_still_running() {
        let snap = ConversationSnapshot {
            turn_running: true,
            turn_unanswered: false,
            has_user: true,
            has_assistant: false,
            user_contents: vec!["q".into()],
            assistant_contents: vec![],
        };
        assert!(!post_restart_lifecycle_ok(&snap));
    }

    #[test]
    fn dishonest_when_lost_silently() {
        let snap = ConversationSnapshot {
            turn_running: false,
            turn_unanswered: false,
            has_user: true,
            has_assistant: false,
            user_contents: vec!["q".into()],
            assistant_contents: vec![],
        };
        assert!(!post_restart_lifecycle_ok(&snap));
    }
}
