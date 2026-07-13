//! Second-domain proof: life-ops style goal session **without** coder-tools / git / cargo.
//!
//! Implements a tiny deterministic pack: given success_criteria strings, "completes" them by
//! emitting tool-like events and succeeding when criteria are non-empty (or failing when
//! payload.force_fail is true). Proves the session kernel is pack-pluggable.

use std::time::Duration;

use async_trait::async_trait;
use liberado_common::{Capability, Zone};
use tokio::sync::mpsc::Sender;

use crate::LIFE_OPS_DOMAIN;
use crate::event::{SessionEvent, SessionEventKind};
use crate::goal::{GoalResult, GoalSpec, TerminalKind};
use crate::runner::{DomainPackRunner, InputChannel, InputOutcome, PackContext, PackError};

/// The vault zone this pack writes notes into. A session's grant must carry
/// `Write(Vault("tasks"))` for the write to happen — a `research` profile that holds only
/// `Read` gets a refusal, not a silent write.
const NOTES_ZONE: &str = "tasks";

/// Demo life-ops pack — vault/task flavored events, no coding dependencies.
pub struct LifeOpsDemoRunner;

impl LifeOpsDemoRunner {
    /// Interactive demo (`payload.interactive = true`): ask one question, await the human's answer
    /// through the [`InputChannel`], echo it, and succeed. Exercises the session-focus S1 kernel
    /// primitive (input channel + `AwaitingInput` + idle budget) without any coding dependency.
    ///
    /// Only reached when the session's grant permits [`Capability::AskHuman`] — see [`run`].
    async fn run_interactive(
        &self,
        session_id: &str,
        ctx: &PackContext<'_>,
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
                let path = format!("vault/{NOTES_ZONE}/{}.md", slugify(&title));
                let _ = events
                    .send(SessionEvent::new(
                        session_id,
                        SessionEventKind::ToolStarted {
                            name: "vault_write_note".into(),
                            args_preview: format!("{path}: {title}"),
                        },
                    ))
                    .await;

                // The capability check, at the point of the consequential act — not at dispatch,
                // not by convention. A profile granted only `Read` reaches here and is refused.
                if !ctx.can(&Capability::Write(Zone::vault(NOTES_ZONE))) {
                    let _ = events
                        .send(SessionEvent::new(
                            session_id,
                            SessionEventKind::ToolFinished {
                                name: "vault_write_note".into(),
                                ok: false,
                                result_preview: format!(
                                    "denied: grant lacks Write(vault:{NOTES_ZONE})"
                                ),
                            },
                        ))
                        .await;
                    return Ok(GoalResult {
                        terminal: TerminalKind::Failed,
                        summary: format!(
                            "refused to write '{title}' — this session's grant does not include \
                             Write(vault:{NOTES_ZONE})"
                        ),
                        artifacts: vec![],
                        diagnostics: serde_json::json!({
                            "domain": LIFE_OPS_DOMAIN,
                            "denied_capability": format!("Write(vault:{NOTES_ZONE})"),
                            "profile": ctx.profile(),
                        }),
                    });
                }

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
        ctx: &PackContext<'_>,
        events: Sender<SessionEvent>,
        inputs: InputChannel,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        let wants_input = goal
            .payload
            .get("interactive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Asking a human is a *capability*, not a mode the caller can simply assert. A session
        // whose grant omits `AskHuman` (an unattended cron, a narrow research hat) falls through to
        // the non-interactive path and gets the job done without a person — rather than blocking on
        // one who was never going to answer. The kernel has already closed its input channel; this
        // check just lets the pack degrade gracefully instead of tripping over the closure.
        if wants_input && ctx.can(&Capability::AskHuman) {
            return self
                .run_interactive(session_id, ctx, events, inputs, cancel)
                .await;
        }

        // `role` is the worked example of an opaque, pack-parsed override: the config stack never
        // interprets it (`[[session_profiles]].overrides`), this pack does.
        let role = ctx
            .overrides()
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("life-worker")
            .to_string();

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleStarted {
                    role,
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
    use crate::goal::{DomainHint, SessionGrant};
    use crate::hub::GoalSessionHub;
    use crate::store::GoalSessionStore;
    use liberado_common::CapabilitySet;
    use std::sync::Arc;
    use std::time::Duration;

    /// The grant an *attended* session resolves to: it may interrupt the human, and it may write
    /// the note. Interactivity is a capability (S6), so an interactive test must grant `AskHuman`
    /// — a session without it is handed a closed input channel and can never await.
    fn attended_grant() -> SessionGrant {
        let mut capabilities = CapabilitySet::empty();
        capabilities.grant(Capability::AskHuman);
        capabilities.grant(Capability::Write(Zone::vault(NOTES_ZONE)));
        SessionGrant {
            capabilities,
            profile: None,
            overrides: serde_json::Value::Null,
        }
    }

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
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "capture a note interactively".into(),
                    success_criteria: vec![],
                    domain: DomainHint::Life,
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({ "interactive": true }),
                },
                attended_grant(),
            )
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
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "abandoned interactive note".into(),
                    success_criteria: vec![],
                    domain: DomainHint::Life,
                    max_turns: 0,
                    max_idle_secs: Some(0), // expires immediately with no answer
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({ "interactive": true }),
                },
                attended_grant(),
            )
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

        // A session that *held* AskHuman and has since finished — its input sender was removed at
        // teardown. This is `Terminal` ("too late"), which must stay distinct from `NotPermitted`
        // ("never allowed"), asserted below.
        let id = hub
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "quick non-interactive goal".into(),
                    success_criteria: vec!["done".into()],
                    domain: DomainHint::Life,
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({}),
                },
                attended_grant(),
            )
            .await
            .unwrap();
        let _ = await_terminal(&hub, &id).await;

        let err = hub.send_input(&id, "too late").await.unwrap_err();
        assert_eq!(err, crate::SendInputError::Terminal);
    }

    #[tokio::test]
    async fn a_session_without_ask_human_refuses_input_as_not_permitted() {
        // The S6 gate. A zero-authority grant (the default — an unattended cron) never gets an
        // input sender at all, so input is refused on *authority* grounds, not timing. Keeping this
        // distinct from `Terminal` is what lets the HTTP layer answer 403 rather than a misleading
        // 409 "you're too late" for a session that was never going to listen.
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let hub = Arc::new(hub);

        let id = hub
            .start(GoalSpec {
                id: None,
                description: "unattended note".into(),
                success_criteria: vec!["done".into()],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({ "interactive": true }),
            })
            .await
            .unwrap();

        let err = hub.send_input(&id, "let me help").await.unwrap_err();
        assert_eq!(err, crate::SendInputError::NotPermitted);
    }

    #[tokio::test]
    async fn a_session_that_may_ask_but_not_write_is_refused_at_the_write() {
        // The other half of the gate: capabilities are checked at the point of the consequential
        // act, not just at the door. This hat may interrupt the human (AskHuman) but holds no
        // Write — so it gets its answer and is then refused the vault write, rather than the
        // capability being quietly ignored once the session is already running.
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let hub = Arc::new(hub);

        let mut capabilities = CapabilitySet::empty();
        capabilities.grant(Capability::AskHuman); // may ask...
        // ...but is NOT granted Write(vault:tasks).

        let id = hub
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "read-only hat tries to write".into(),
                    success_criteria: vec![],
                    domain: DomainHint::Life,
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: Some("research".into()),
                    payload: serde_json::json!({ "interactive": true }),
                },
                SessionGrant {
                    capabilities,
                    profile: Some("research".into()),
                    overrides: serde_json::Value::Null,
                },
            )
            .await
            .unwrap();

        // It does reach the human (it holds AskHuman).
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if hub.snapshot(&id).await.unwrap().session.awaiting_input {
                break;
            }
        }
        hub.send_input(&id, "Weekly Review").await.unwrap();

        let snap = await_terminal(&hub, &id).await;
        let result = snap.session.result.expect("terminal result");
        assert_eq!(snap.session.status, crate::goal::SessionStatus::Failed);
        assert!(
            result.summary.contains("does not include Write"),
            "expected a capability refusal, got: {}",
            result.summary
        );
        assert!(
            result.artifacts.is_empty(),
            "a refused write must not report an artifact it never created"
        );
    }

    #[tokio::test]
    async fn a_grant_is_never_widened_by_sending_input() {
        // The G8 non-widening invariant, now that a session actually *has* an authority boundary:
        // delivering human input must not change what the session may do (Decision 4 — capabilities
        // only ever narrow).
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let hub = Arc::new(hub);

        let id = hub
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "capture a note interactively".into(),
                    success_criteria: vec![],
                    domain: DomainHint::Life,
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({ "interactive": true }),
                },
                attended_grant(),
            )
            .await
            .unwrap();

        // Wait until it blocks on the human, then capture the grant.
        let mut before = None;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let snap = hub.snapshot(&id).await.unwrap();
            if snap.session.awaiting_input {
                before = Some(snap.session.grant.clone());
                break;
            }
        }
        let before = before.expect("session never awaited input");

        hub.send_input(&id, "Weekly Review").await.unwrap();
        let after = await_terminal(&hub, &id).await.session.grant;

        assert_eq!(before, after, "human input must never widen a session's grant");
        assert_eq!(before.capabilities, attended_grant().capabilities);
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
                profile: None,
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
                profile: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap_err();
        assert!(err.contains("no domain pack"));
    }
}
