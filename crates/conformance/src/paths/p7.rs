//! P7 — a chat turn survives, or is honestly reported, across a daemon restart.
//!
//! Against a live daemon (when `restart_command` is configured):
//!
//! 1. start a **background** chat turn and drop the SSE after the session id;
//! 2. **spawn** the configured restart command (host-specific — never hard-coded);
//! 3. **while that command runs** (and briefly after), probe `POST /api/chat` until we observe
//!    `503` + `error: shutting_down` — the drain window is *during* recreate, not after it returns;
//! 4. wait for the restart process to exit, then until the daemon is up again;
//! 5. assert lifecycle flags: **`turn_running` is false**, and either an assistant reply is on
//!    the transcript **or** `turn_unanswered` is true — never a silent lost turn.
//!
//! Unconfigured restart hook → `PathStatus::Skipped` with a reason (never Pass).
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

    // ── 2–3. Spawn restart + probe drain concurrently ────────────────────────
    // Typical hooks (`docker compose up -d --force-recreate`) drain *during* the command. Awaiting
    // the process first then probing only sees the new process accepting 200s.
    let mut child = match spawn_restart_command(restart_cmd) {
        Ok(c) => c,
        Err(e) => {
            return PathResult::fail(
                PathId::P7,
                "spawn configured restart_command",
                elapsed_ms(start),
                serde_json::json!({
                    "error": e,
                    "restart_command": restart_cmd,
                    "session_id": session
                }),
            );
        }
    };

    let drain_deadline = Instant::now() + timeout.min(Duration::from_secs(120));
    let mut saw_shutting_down = false;
    let mut last_probe = serde_json::json!({});
    let mut restart_exit: Option<std::process::ExitStatus> = None;
    // After the restart process exits without a drain observation, probe only briefly — the
    // gate is almost always closed *during* recreate, not after a successful up.
    let mut post_exit_deadline: Option<Instant> = None;

    while Instant::now() < drain_deadline {
        // Non-blocking reap of the restart process (null stdio — no pipe deadlock).
        if restart_exit.is_none() {
            match child.try_wait() {
                Ok(Some(st)) => {
                    restart_exit = Some(st);
                    if !saw_shutting_down {
                        post_exit_deadline = Some(Instant::now() + Duration::from_secs(2));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    return PathResult::fail(
                        PathId::P7,
                        "wait restart_command (try_wait)",
                        elapsed_ms(start),
                        serde_json::json!({
                            "error": e.to_string(),
                            "restart_command": restart_cmd,
                            "session_id": session
                        }),
                    );
                }
            }
        }

        match client.post_chat("p7 probe during restart").await {
            Ok((status, body)) => {
                last_probe = serde_json::json!({ "status": status, "body": body });
                if DaemonClient::is_shutting_down_response(status, &body) {
                    saw_shutting_down = true;
                }
            }
            Err(e) => {
                last_probe = serde_json::json!({ "error": e });
                // Connection errors are expected while the process is down; keep probing.
            }
        }

        // Seen drain and restart process finished → enough for step 3.
        if saw_shutting_down && restart_exit.is_some() {
            break;
        }
        // Restart finished, short post-exit window elapsed, still no drain signal → fail next.
        if let Some(d) = post_exit_deadline
            && Instant::now() >= d
            && !saw_shutting_down
        {
            break;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Reap if still running past the probe window.
    let restart_status = match restart_exit {
        Some(st) => st,
        None => match child.wait().await {
            Ok(st) => st,
            Err(e) => {
                return PathResult::fail(
                    PathId::P7,
                    "wait restart_command",
                    elapsed_ms(start),
                    serde_json::json!({
                        "error": e.to_string(),
                        "restart_command": restart_cmd,
                        "session_id": session,
                        "saw_shutting_down": saw_shutting_down,
                    }),
                );
            }
        },
    };

    if !restart_status.success() {
        return PathResult::fail(
            PathId::P7,
            "execute configured restart_command",
            elapsed_ms(start),
            serde_json::json!({
                "error": format!("exit {restart_status}"),
                "restart_command": restart_cmd,
                "session_id": session,
                "saw_shutting_down": saw_shutting_down,
            }),
        );
    }

    if !saw_shutting_down {
        return PathResult::fail(
            PathId::P7,
            "during restart/drain, POST /api/chat returns 503 with error=shutting_down \
             (probed concurrently while restart_command ran)",
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
        "restart observed shutting_down (concurrent with restart_command); post-restart lifecycle honest",
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

/// Spawn the host restart hook. **Stdout/stderr are discarded** (`Stdio::null`) so a verbose
/// compose log cannot fill a pipe buffer and deadlock the child while we only `wait`.
fn spawn_restart_command(cmd: &str) -> Result<tokio::process::Child, String> {
    #[cfg(windows)]
    let child = liberado_common::process::command("cmd")
        .args(["/C", cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn restart_command: {e}"))?;

    #[cfg(not(windows))]
    let child = liberado_common::process::command("sh")
        .args(["-c", cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn restart_command: {e}"))?;

    Ok(child)
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

    /// The zombie case, **isolated**: the reply landed *and* the flag is still set. Only the
    /// `!turn_running` half of the check can reject this — `dishonest_when_still_running` above
    /// has no assistant reply, so it is also a lost turn and fails either way. Without this test
    /// the zombie guard can be deleted with the whole suite staying green.
    ///
    /// This is the case that strands a human: the answer is on the transcript, and the surface
    /// shows a turn still in flight in a process that no longer exists.
    #[test]
    fn dishonest_when_running_flag_survives_a_completed_turn() {
        let snap = ConversationSnapshot {
            turn_running: true,
            turn_unanswered: false,
            has_user: true,
            has_assistant: true,
            user_contents: vec!["q".into()],
            assistant_contents: vec!["a".into()],
        };
        assert!(
            !post_restart_lifecycle_ok(&snap),
            "turn_running after a restart is a claim about a dead process, reply or not"
        );
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
