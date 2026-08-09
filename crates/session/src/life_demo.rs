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
use crate::record_store::TurnAuthor;
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
        const QUESTION: &str = "What should I title the note?";

        // Asking is a *turn* — it is the pack talking to a human. The `AwaitingInput` event below is
        // the same fact seen by a live subscriber (it is what raises the "needs you" badge), but an
        // event is not dialogue: it is not searchable and it has no place in the message DAG. The
        // human's reply is recorded as a turn by the hub, so both halves of the exchange survive.
        ctx.record_turn(TurnAuthor::Assistant, QUESTION).await;

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::AwaitingInput {
                    prompt: QUESTION.into(),
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
    use crate::goal::SessionStatus;
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
            ..Default::default()
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
                    ..Default::default()
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

        assert_eq!(
            before, after,
            "human input must never widen a session's grant"
        );
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

    #[tokio::test]
    async fn a_parked_session_is_not_told_it_finished() {
        // E6 added `Parked` and did not update `send_input`'s match, so a session parked mid-question
        // across a daemon restart fell into `Some(_) => Terminal` and the API answered
        // "goal session has already finished — not accepting input". That is the one thing it has
        // definitively NOT done: the question it holds for you is still right there. A client (or a
        // person) reading "finished" concludes the work is dead and starts over.
        //
        // Found by the live restart control, 2026-07-14.
        let store = GoalSessionStore::new();
        let mut rec = crate::goal::GoalSessionRecord::new(GoalSpec {
            id: Some("parked-1".into()),
            description: "waiting on you".into(),
            success_criteria: vec![],
            domain: DomainHint::Life,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::Value::Null,
        });
        // It may ask (AskHuman), it is awaiting an answer, and the daemon restarted under it.
        rec.grant = SessionGrant {
            capabilities: CapabilitySet::from_iter([liberado_common::Capability::AskHuman]),
            ..Default::default()
        };
        rec.status = crate::goal::SessionStatus::Parked;
        rec.awaiting_input = true;
        crate::record_store::SessionRecordStore::insert(&store, rec).await;

        let hub = Arc::new(GoalSessionHub::new(store));
        let err = hub
            .send_input("parked-1", "accept".to_string())
            .await
            .expect_err("a parked session has no live pack to receive the answer");

        assert!(
            matches!(err, crate::hub::SendInputError::Parked),
            "a parked session must report Parked, not Terminal: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            !msg.contains("already finished"),
            "must not tell a parked session it finished: {msg}"
        );
        assert!(
            msg.contains("parked"),
            "and must say what it actually is: {msg}"
        );
    }

    /// F4: a parked store record has no live cancel token. Cancel must still finish it as
    /// Cancelled so the stuck-session panel (and shepherd-side cleanup) can clear orphans.
    #[tokio::test]
    async fn cancel_parked_session_without_live_host_reaches_cancelled() {
        let store = GoalSessionStore::new();
        let created = chrono::Utc::now() - chrono::Duration::hours(26);
        let mut rec = crate::goal::GoalSessionRecord::new(GoalSpec {
            id: Some("parked-orphan".into()),
            description: "orphaned intake question".into(),
            success_criteria: vec![],
            domain: DomainHint::Life,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::Value::Null,
        });
        rec.status = crate::goal::SessionStatus::Parked;
        rec.awaiting_input = true;
        rec.created_at = created;
        crate::record_store::SessionRecordStore::insert(&store, rec).await;

        let hub = Arc::new(GoalSessionHub::new(store.clone()));

        // list surfaces parked sessions (panel source of truth).
        let listed = hub.list().await;
        let parked: Vec<_> = listed
            .iter()
            .filter(|r| r.status == crate::goal::SessionStatus::Parked)
            .collect();
        assert_eq!(parked.len(), 1, "list must identify parked sessions");
        assert_eq!(parked[0].id, "parked-orphan");
        assert_eq!(
            parked[0].created_at, created,
            "age is derived from durable created_at"
        );

        hub.cancel("parked-orphan")
            .await
            .expect("cancel of parked (no live token) must be accepted");

        let after = store.get("parked-orphan").await.expect("record remains");
        assert_eq!(
            after.status,
            crate::goal::SessionStatus::Cancelled,
            "parked cancel must leave a terminal Cancelled record"
        );
        assert!(
            after.finished_at.is_some(),
            "cancelled parked session must stamp finished_at"
        );
        assert!(
            !after.awaiting_input,
            "cancel clears awaiting_input so it no longer looks stuck"
        );
        // No longer listed as parked.
        let still_parked = hub
            .list()
            .await
            .into_iter()
            .filter(|r| r.status == crate::goal::SessionStatus::Parked)
            .count();
        assert_eq!(still_parked, 0, "cancelled session must not list as parked");
    }

    /// Cancelling a parked orphan must be *observable*, not just durable. `store.finish` mutates
    /// the row and publishes nothing, so without an explicit event the cancel is invisible to
    /// every event consumer: an SSE client watching the session sees it stay parked, and
    /// `await_terminal` — which blocks on `recv()` between its status checks — never wakes, so a
    /// `delegate` parented to the session hangs instead of returning.
    #[tokio::test]
    async fn cancel_of_a_parked_orphan_wakes_event_consumers() {
        let store = GoalSessionStore::new();
        let mut rec = crate::goal::GoalSessionRecord::new(GoalSpec {
            id: Some("parked-observed".into()),
            description: "orphaned intake question".into(),
            success_criteria: vec![],
            domain: DomainHint::Life,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::Value::Null,
        });
        rec.status = crate::goal::SessionStatus::Parked;
        rec.awaiting_input = true;
        crate::record_store::SessionRecordStore::insert(&store, rec).await;

        let hub = Arc::new(GoalSessionHub::new(store.clone()));

        // The SSE path: a subscriber attached while the session is parked.
        let (_history, mut rx) = store.subscribe("parked-observed").await.expect("subscribe");

        // The `delegate` path: a waiter already blocked on the session's terminal state. Started
        // before the cancel, so it can only return by being woken by an event.
        let waiter = {
            let hub = Arc::clone(&hub);
            tokio::spawn(async move { hub.await_terminal("parked-observed").await })
        };
        tokio::task::yield_now().await;

        hub.cancel("parked-observed").await.expect("cancel parked");

        let finished = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("a parked cancel must publish an event, not only mutate the row")
            .expect("event bus stayed open");
        match finished.kind {
            SessionEventKind::SessionFinished { ref status, .. } => {
                assert_eq!(status, "cancelled", "event must report the cancel");
            }
            other => panic!("expected SessionFinished, got {other:?}"),
        }

        let snap = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("await_terminal must be woken by the cancel, not hang forever")
            .expect("waiter task")
            .expect("terminal snapshot");
        assert_eq!(snap.session.status, crate::goal::SessionStatus::Cancelled);
        assert!(
            snap.session.result.is_some(),
            "result must be durable before the event that announces it"
        );
    }

    /// E6-c end to end: a parked session, answered, comes back with its memory intact.
    #[tokio::test]
    async fn answering_a_parked_session_resumes_the_pack_with_its_transcript() {
        use crate::runner::DomainPackRunner;
        use std::sync::Mutex;

        /// A pack that can rebuild itself from its transcript, and records what it saw on start.
        struct ResumablePack {
            saw: Arc<Mutex<Vec<(crate::record_store::TurnAuthor, String)>>>,
        }
        #[async_trait::async_trait]
        impl DomainPackRunner for ResumablePack {
            fn domain_id(&self) -> &str {
                "life"
            }
            async fn can_resume(&self, _ctx: &crate::runner::PackContext<'_>) -> bool {
                true
            }
            async fn run(
                &self,
                _id: &str,
                _goal: &GoalSpec,
                ctx: &crate::runner::PackContext<'_>,
                _events: tokio::sync::mpsc::Sender<crate::SessionEvent>,
                _inputs: crate::runner::InputChannel,
                _cancel: tokio::sync::watch::Receiver<bool>,
            ) -> Result<crate::GoalResult, crate::runner::PackError> {
                // The whole point: on a resume, the pack can see what was already said.
                *self.saw.lock().unwrap() = ctx.prior_turns().await;
                Ok(crate::GoalResult {
                    terminal: crate::TerminalKind::Succeeded,
                    summary: "resumed and finished".into(),
                    artifacts: vec![],
                    diagnostics: serde_json::Value::Null,
                })
            }
        }

        let saw = Arc::new(Mutex::new(Vec::new()));
        let store = GoalSessionStore::new();

        // A session that was parked mid-question when the daemon died: it holds a goal, a question
        // the pack asked, and no answer.
        let mut rec = crate::goal::GoalSessionRecord::new(GoalSpec {
            id: Some("parked-2".into()),
            description: "capture a note".into(),
            success_criteria: vec![],
            domain: DomainHint::Life,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::Value::Null,
        });
        rec.grant = SessionGrant {
            capabilities: CapabilitySet::from_iter([liberado_common::Capability::AskHuman]),
            ..Default::default()
        };
        rec.status = crate::goal::SessionStatus::Parked;
        rec.awaiting_input = true;
        crate::record_store::SessionRecordStore::insert(&store, rec).await;
        crate::record_store::SessionRecordStore::append_turn(
            &store,
            "parked-2",
            crate::record_store::TurnAuthor::User,
            "capture a note".into(),
        )
        .await;
        crate::record_store::SessionRecordStore::append_turn(
            &store,
            "parked-2",
            crate::record_store::TurnAuthor::Assistant,
            "What should I title it?".into(),
        )
        .await;

        let mut hub = GoalSessionHub::new(store);
        hub.register_pack(Arc::new(ResumablePack { saw: saw.clone() }));
        let hub = Arc::new(hub);

        // Answering a parked session IS the resume.
        hub.resume("parked-2", "Weekly Review")
            .await
            .expect("a resumable parked session must accept its answer");

        let snap = await_terminal(&hub, "parked-2").await;
        assert_eq!(snap.session.status, crate::goal::SessionStatus::Succeeded);

        // The pack picked the conversation back up rather than starting over: it saw the goal, the
        // question it had asked before the restart, AND the answer that woke it.
        let seen = saw.lock().unwrap().clone();
        let said: Vec<&str> = seen.iter().map(|(_, t)| t.as_str()).collect();
        assert!(
            said.contains(&"What should I title it?"),
            "the pack must remember the question it asked: {said:?}"
        );
        assert!(
            said.contains(&"Weekly Review"),
            "and must see the answer that resumed it -- it is recorded BEFORE the pack starts,              which is what makes the replay unnecessary: {said:?}"
        );
    }

    /// Cancel must actually cancel.
    ///
    /// Found by mutation testing (2026-07-14): replacing `GoalSessionHub::cancel`'s entire body with
    /// `Ok(())` — making it a **no-op** — broke no test in the codebase. Both the TUI and the WebUI
    /// offer a cancel button, `POST /api/goals/{id}/cancel` is a documented endpoint, and nothing
    /// anywhere proved it stopped anything. A cancel that silently does nothing is worse than no
    /// cancel button: the user believes the work stopped, and walks away while it keeps running.
    #[tokio::test]
    async fn cancel_actually_stops_a_running_pack() {
        use crate::runner::DomainPackRunner;

        /// Never finishes on its own. The ONLY way this session can terminate is if the cancel
        /// signal genuinely reaches the pack — so a no-op cancel hangs the test rather than passing.
        struct NeverEndingPack;
        #[async_trait::async_trait]
        impl DomainPackRunner for NeverEndingPack {
            fn domain_id(&self) -> &str {
                "life"
            }
            async fn run(
                &self,
                _id: &str,
                _goal: &GoalSpec,
                _ctx: &crate::runner::PackContext<'_>,
                _events: tokio::sync::mpsc::Sender<crate::SessionEvent>,
                _inputs: crate::runner::InputChannel,
                mut cancel: tokio::sync::watch::Receiver<bool>,
            ) -> Result<crate::GoalResult, crate::runner::PackError> {
                loop {
                    if *cancel.borrow() {
                        return Err(crate::runner::PackError::Cancelled);
                    }
                    if cancel.changed().await.is_err() {
                        return Err(crate::runner::PackError::Cancelled);
                    }
                }
            }
        }

        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(NeverEndingPack));
        let hub = Arc::new(hub);

        let id = hub
            .start(GoalSpec {
                id: None,
                description: "work forever until told to stop".into(),
                success_criteria: vec![],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::Value::Null,
            })
            .await
            .unwrap();

        // Cancel work that is genuinely running, not a task that has not started.
        for _ in 0..100 {
            if hub.snapshot(&id).await.map(|s| s.session.status) == Some(SessionStatus::Running) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        hub.cancel(&id).await.expect("cancel must be accepted");

        let snap = await_terminal(&hub, &id).await;
        assert_eq!(
            snap.session.status,
            SessionStatus::Cancelled,
            "a cancelled session must actually reach Cancelled — this pack cannot finish any other way"
        );
    }

    /// `list` must actually list. Mutation testing found that replacing it with `vec![]` broke
    /// nothing — and the session switcher in every surface reads it.
    #[tokio::test]
    async fn list_returns_the_sessions_that_exist() {
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let hub = Arc::new(hub);

        assert!(hub.list().await.is_empty(), "no sessions yet");

        let id = hub
            .start(GoalSpec {
                id: None,
                description: "capture a note".into(),
                success_criteria: vec!["done".into()],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::Value::Null,
            })
            .await
            .unwrap();
        await_terminal(&hub, &id).await;

        let listed = hub.list().await;
        assert_eq!(listed.len(), 1, "the session must be listable");
        assert_eq!(listed[0].id, id);
    }

    /// B1: a session parked by the drain mid-execution (not awaiting a human)
    /// rehydrates as Parked and is resumable. The E6 path covers awaiting;
    /// this exercises the drain-park path.
    #[tokio::test]
    async fn a_drain_parked_session_resumes_after_rehydrate() {
        use crate::goal::{GoalSessionRecord, GoalSpec, SessionGrant, SessionStatus};
        use crate::store::GoalSessionStore;

        /// A pack that can resume and records that it was called.
        struct ResumablePack;
        #[async_trait::async_trait]
        impl DomainPackRunner for ResumablePack {
            fn domain_id(&self) -> &str {
                "life"
            }
            async fn can_resume(&self, _ctx: &crate::runner::PackContext<'_>) -> bool {
                true
            }
            async fn run(
                &self,
                _id: &str,
                _goal: &GoalSpec,
                _ctx: &crate::runner::PackContext<'_>,
                _events: tokio::sync::mpsc::Sender<crate::SessionEvent>,
                _inputs: crate::runner::InputChannel,
                _cancel: tokio::sync::watch::Receiver<bool>,
            ) -> Result<crate::GoalResult, crate::runner::PackError> {
                Ok(crate::GoalResult {
                    terminal: crate::TerminalKind::Succeeded,
                    summary: "drain-park resume succeeded".into(),
                    artifacts: vec![],
                    diagnostics: serde_json::Value::Null,
                })
            }
        }

        let dir = std::env::temp_dir().join(format!("liberado-goals-test-{}", ulid::Ulid::new()));
        {
            // The drain-park shape: session was running, drain sets Parked.
            let store = GoalSessionStore::open(&dir).await;
            let mut rec = GoalSessionRecord::new(GoalSpec {
                id: Some("b1-resume".into()),
                description: "long running task".into(),
                success_criteria: vec![],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::Value::Null,
            });
            rec.grant = SessionGrant {
                capabilities: CapabilitySet::from_iter([liberado_common::Capability::AskHuman]),
                ..Default::default()
            };
            rec.status = SessionStatus::Running;
            store.insert(rec).await;
            store.set_status("b1-resume", SessionStatus::Parked).await;
        }

        // Reopen: the store rehydrates from disk.
        let store = GoalSessionStore::open(&dir).await;
        let rehydrated = store.get("b1-resume").await.unwrap();
        assert_eq!(
            rehydrated.status,
            SessionStatus::Parked,
            "B1: drain-parked session must rehydrate as Parked"
        );
        assert!(
            !rehydrated.awaiting_input,
            "drain-park has no awaiting question"
        );

        let mut hub = GoalSessionHub::new(store);
        hub.register_pack(Arc::new(ResumablePack));
        let hub = Arc::new(hub);

        // Resume must work — this is what B1 fixes.
        hub.resume("b1-resume", "let's continue")
            .await
            .expect("drain-parked session must be resumable");

        std::fs::remove_dir_all(&dir).ok();
    }
}
