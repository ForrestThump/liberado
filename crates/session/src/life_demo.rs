//! Second-domain proof: life-ops style goal session **without** coder-tools / git / cargo.
//!
//! Implements a tiny deterministic pack: given success_criteria strings, "completes" them by
//! emitting tool-like events and succeeding when criteria are non-empty (or failing when
//! payload.force_fail is true). Proves the session kernel is pack-pluggable.

use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

use crate::LIFE_OPS_DOMAIN;
use crate::event::{SessionEvent, SessionEventKind};
use crate::goal::{GoalResult, GoalSpec, TerminalKind};
use crate::runner::{DomainPackRunner, InputChannel, InputOutcome, PackError};

/// Demo life-ops pack — vault/task flavored events, no coding dependencies.
pub struct LifeOpsDemoRunner;

impl LifeOpsDemoRunner {
    /// Interactive demo (`payload.interactive = true`): ask one question, await the human's answer
    /// through the [`InputChannel`], echo it, and succeed. Exercises the session-focus S1 kernel
    /// primitive (input channel + `AwaitingInput` + idle budget) without any coding dependency.
    async fn run_interactive(
        &self,
        session_id: &str,
        events: Sender<SessionEvent>,
        mut inputs: InputChannel,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::AwaitingInput {
                    prompt: "What should I title the note?".into(),
                    options: Vec::new(),
                },
            ))
            .await;

        let outcome = tokio::select! {
            outcome = inputs.recv() => outcome,
            _ = cancel.changed() => InputOutcome::Closed,
        };
        if *cancel.borrow() {
            return Err(PackError::Cancelled);
        }

        match outcome {
            InputOutcome::Received(input) => {
                let title = input.text.trim().to_string();
                let path = format!("vault/tasks/{}.md", slugify(&title));
                let _ = events
                    .send(SessionEvent::new(
                        session_id,
                        SessionEventKind::ToolStarted {
                            name: "vault_write_note".into(),
                            args_preview: format!("{path}: {title}"),
                        },
                    ))
                    .await;
                let _ = events
                    .send(SessionEvent::new(
                        session_id,
                        SessionEventKind::ToolFinished {
                            name: "vault_write_note".into(),
                            ok: true,
                            result_preview: "written".into(),
                        },
                    ))
                    .await;
                Ok(GoalResult {
                    terminal: TerminalKind::Succeeded,
                    summary: format!("wrote note titled '{title}'"),
                    artifacts: vec![path],
                    diagnostics: serde_json::json!({ "domain": LIFE_OPS_DOMAIN, "interactive": true }),
                })
            }
            InputOutcome::IdleExpired(d) => Ok(GoalResult {
                terminal: TerminalKind::BudgetExhausted,
                summary: format!("no answer after {}s idle", d.as_secs()),
                artifacts: vec![],
                diagnostics: serde_json::json!({ "domain": LIFE_OPS_DOMAIN, "idle_timeout": true }),
            }),
            InputOutcome::Closed => Err(PackError::Cancelled),
        }
    }
}

#[async_trait]
impl DomainPackRunner for LifeOpsDemoRunner {
    fn domain_id(&self) -> &str {
        LIFE_OPS_DOMAIN
    }

    async fn run(
        &self,
        session_id: &str,
        goal: &GoalSpec,
        events: Sender<SessionEvent>,
        inputs: InputChannel,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        if goal
            .payload
            .get("interactive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return self
                .run_interactive(session_id, events, inputs, cancel)
                .await;
        }

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleStarted {
                    role: "life-worker".into(),
                    model: "deterministic-demo".into(),
                },
            ))
            .await;

        // Simulated read-only "vault" inspect.
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::ToolStarted {
                    name: "vault_list_notes".into(),
                    args_preview: r#"{"folder":"tasks"}"#.into(),
                },
            ))
            .await;

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    return Err(PackError::Cancelled);
                }
            }
        }

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::ToolFinished {
                    name: "vault_list_notes".into(),
                    ok: true,
                    result_preview: "3 notes".into(),
                },
            ))
            .await;

        let force_fail = goal
            .payload
            .get("force_fail")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if force_fail {
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::ValidationFinished {
                        ok: false,
                        summary: "life criteria not met (force_fail)".into(),
                    },
                ))
                .await;
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::RoleFinished {
                        role: "life-worker".into(),
                    },
                ))
                .await;
            return Ok(GoalResult {
                terminal: TerminalKind::Failed,
                summary: "life demo forced failure".into(),
                artifacts: vec![],
                diagnostics: serde_json::json!({"domain": LIFE_OPS_DOMAIN}),
            });
        }

        // "Write" a vault note for each success criterion (simulated).
        let mut artifacts = Vec::new();
        for (i, criterion) in goal.success_criteria.iter().enumerate() {
            if *cancel.borrow() {
                return Err(PackError::Cancelled);
            }
            let path = format!("vault/tasks/item-{i}.md");
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::ToolStarted {
                        name: "vault_write_note".into(),
                        args_preview: format!("{path}: {criterion}"),
                    },
                ))
                .await;
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::ToolFinished {
                        name: "vault_write_note".into(),
                        ok: true,
                        result_preview: "written".into(),
                    },
                ))
                .await;
            artifacts.push(path);
        }

        if artifacts.is_empty() {
            // Description-only goal still "files" a single outcome note.
            artifacts.push("vault/tasks/outcome.md".into());
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::Progress {
                        message: format!("completed life goal: {}", goal.description),
                    },
                ))
                .await;
        }

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::ValidationFinished {
                    ok: true,
                    summary: format!("{} artifact(s) recorded", artifacts.len()),
                },
            ))
            .await;

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleFinished {
                    role: "life-worker".into(),
                },
            ))
            .await;

        Ok(GoalResult {
            terminal: TerminalKind::Succeeded,
            summary: format!(
                "life-ops demo completed: {} ({} criteria)",
                goal.description,
                goal.success_criteria.len()
            ),
            artifacts,
            diagnostics: serde_json::json!({
                "domain": LIFE_OPS_DOMAIN,
                "note": "deterministic second-domain proof — no coder-tools"
            }),
        })
    }
}

/// Lowercase, non-alphanumerics to `-`, collapsed — enough for a demo note path.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() { "note".into() } else { slug }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::DomainHint;
    use crate::hub::GoalSessionHub;
    use crate::store::GoalSessionStore;
    use std::sync::Arc;
    use std::time::Duration;

    /// Spin until the session reaches a terminal state (or panic after ~1s).
    async fn await_terminal(hub: &Arc<GoalSessionHub>, id: &str) -> crate::SessionSnapshot {
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let snap = hub.snapshot(id).await.unwrap();
            if snap.session.status.is_terminal() {
                return snap;
            }
        }
        panic!("session {id} did not finish");
    }

    #[tokio::test]
    async fn interactive_session_asks_awaits_and_echoes_the_answer() {
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let hub = Arc::new(hub);

        let id = hub
            .start(GoalSpec {
                id: None,
                description: "capture a note interactively".into(),
                success_criteria: vec![],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                payload: serde_json::json!({ "interactive": true }),
            })
            .await
            .unwrap();

        // The pack should reach AwaitingInput and mark the record.
        let mut asked = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let snap = hub.snapshot(&id).await.unwrap();
            if snap.session.awaiting_input
                && snap
                    .events
                    .iter()
                    .any(|e| matches!(e.kind, SessionEventKind::AwaitingInput { .. }))
            {
                asked = true;
                break;
            }
        }
        assert!(asked, "pack never reached AwaitingInput");

        // Answer it.
        hub.send_input(&id, "Weekly Review").await.unwrap();

        let snap = await_terminal(&hub, &id).await;
        assert_eq!(snap.session.status, crate::goal::SessionStatus::Succeeded);
        assert!(!snap.session.awaiting_input, "should clear after answer");
        // The human input is echoed into the transcript.
        assert!(snap.events.iter().any(|e| matches!(
            &e.kind, SessionEventKind::HumanInput { text } if text == "Weekly Review"
        )));
        // And the answer drove the outcome.
        assert!(
            snap.session
                .result
                .as_ref()
                .unwrap()
                .summary
                .contains("Weekly Review")
        );
        assert!(snap.session.result.as_ref().unwrap().artifacts[0].contains("weekly-review"));
    }

    #[tokio::test]
    async fn interactive_session_idle_budget_terminates_budget_exhausted() {
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let hub = Arc::new(hub);

        let id = hub
            .start(GoalSpec {
                id: None,
                description: "abandoned interactive note".into(),
                success_criteria: vec![],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: Some(0), // expires immediately with no answer
                origin: None,
                payload: serde_json::json!({ "interactive": true }),
            })
            .await
            .unwrap();

        let snap = await_terminal(&hub, &id).await;
        assert_eq!(
            snap.session.status,
            crate::goal::SessionStatus::BudgetExhausted
        );
        // Never answered → no input echo.
        assert!(!snap.session.awaiting_input);
    }

    #[tokio::test]
    async fn send_input_to_finished_session_errors() {
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let hub = Arc::new(hub);

        // Non-interactive session finishes on its own; its input sender is then removed.
        let id = hub
            .start(GoalSpec {
                id: None,
                description: "quick non-interactive goal".into(),
                success_criteria: vec!["done".into()],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();
        let _ = await_terminal(&hub, &id).await;

        let err = hub.send_input(&id, "too late").await.unwrap_err();
        assert_eq!(err, crate::SendInputError::Terminal);
    }

    #[tokio::test]
    async fn send_input_to_unknown_session_is_distinct_from_terminal() {
        let hub = Arc::new(GoalSessionHub::new(GoalSessionStore::new()));
        // Never-existed id: Unknown, not Terminal — so the HTTP layer answers 404, not 409.
        let err = hub.send_input("nope", "hello").await.unwrap_err();
        assert_eq!(err, crate::SendInputError::Unknown);
    }

    #[tokio::test]
    async fn life_domain_session_succeeds_without_coding_pack() {
        let store = GoalSessionStore::new();
        let mut hub = GoalSessionHub::new(store);
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let hub = Arc::new(hub);

        let id = hub
            .start(GoalSpec {
                id: None,
                description: "file vault note and mark task done".into(),
                success_criteria: vec!["note written".into(), "task marked done".into()],
                domain: DomainHint::Life,
                max_turns: 4,
                max_idle_secs: None,
                origin: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();

        // Wait for terminal.
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let snap = hub.snapshot(&id).await.unwrap();
            if snap.session.status.is_terminal() {
                assert_eq!(snap.session.status, crate::goal::SessionStatus::Succeeded);
                assert!(!snap.events.is_empty());
                assert!(snap.events.iter().any(|e| matches!(
                    e.kind,
                    SessionEventKind::ToolStarted { ref name, .. } if name == "vault_write_note"
                )));
                // No coding domain required — only life is registered.
                assert!(hub.registered_domains().iter().any(|d| d == "life"));
                assert!(!hub.registered_domains().iter().any(|d| d == "coding"));
                return;
            }
        }
        panic!("life session did not finish");
    }

    #[tokio::test]
    async fn unknown_domain_rejected() {
        let hub = Arc::new(GoalSessionHub::new(GoalSessionStore::new()));
        let err = hub
            .start(GoalSpec {
                id: None,
                description: "x".into(),
                success_criteria: vec![],
                domain: DomainHint::Coding,
                max_turns: 1,
                max_idle_secs: None,
                origin: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap_err();
        assert!(err.contains("no domain pack"));
    }
}
