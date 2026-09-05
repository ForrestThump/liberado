//! One coding attempt, split from the build lifecycle for module-health boundaries.

use liberado_coder_core::CoderRunRequest;
use liberado_common::Outcome;
use liberado_session::{GoalResult, PackError, SessionEvent, SessionEventKind, TerminalKind};
use tokio::sync::mpsc::Sender;

use super::CodingSessionPack;

/// How one coding attempt ended.
pub(super) enum AttemptOutcome {
    /// The attempt ran and produced a verdict.
    Verdict {
        ok: bool,
        summary: String,
        artifacts: Vec<String>,
        diagnostics: serde_json::Value,
    },
    /// The environment broke; no human answer can fix a dead backend.
    Broken(GoalResult),
}

impl CodingSessionPack {
    /// Race one coding attempt against cancellation, then classify how it ended.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_one_attempt(
        &self,
        session_id: &str,
        model: &str,
        overrides: &serde_json::Value,
        payload: &serde_json::Value,
        request: &CoderRunRequest,
        events: &Sender<SessionEvent>,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<AttemptOutcome, PackError> {
        let worker_id = self
            .workers
            .select(overrides, payload)
            .map_err(|error| PackError::Setup(error.to_string()))?;
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleStarted {
                    role: "coder".into(),
                    model: model.to_string(),
                },
            ))
            .await;

        let result = crate::live::LIVE_GATE
            .scope(
                (events.clone(), session_id.to_string()),
                self.workers
                    .run(&worker_id, &self.backend, request.clone(), cancel),
            )
            .await;
        let result = match result {
            Ok(super::workers::RegistryRun::Finished(result)) => Ok(*result),
            Ok(super::workers::RegistryRun::Cancelled) => return Err(PackError::Cancelled),
            Err(error) => Err(error),
        };

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleFinished {
                    role: "coder".into(),
                },
            ))
            .await;

        match result {
            Ok(run) => {
                let ok = run.outcome == Outcome::Succeeded;
                for change in &run.file_changes {
                    let _ = events
                        .send(SessionEvent::new(
                            session_id,
                            SessionEventKind::FileChanged {
                                path: change.path.clone(),
                                change: change.change.clone(),
                            },
                        ))
                        .await;
                }
                let _ = events
                    .send(SessionEvent::new(
                        session_id,
                        SessionEventKind::ValidationFinished {
                            ok,
                            summary: run
                                .validation_notes
                                .clone()
                                .unwrap_or_else(|| run.summary.clone()),
                        },
                    ))
                    .await;
                Ok(AttemptOutcome::Verdict {
                    ok,
                    summary: run.summary,
                    artifacts: run.files_changed,
                    diagnostics: run.diagnostics,
                })
            }
            Err(error) if crate::is_stuck_error(&error) => {
                let message = error.to_string();
                let _ = events
                    .send(SessionEvent::new(
                        session_id,
                        SessionEventKind::ValidationFinished {
                            ok: false,
                            summary: message.clone(),
                        },
                    ))
                    .await;
                Ok(AttemptOutcome::Verdict {
                    ok: false,
                    summary: message,
                    artifacts: Vec::new(),
                    diagnostics: serde_json::json!({"error": "coder_backend", "stuck": true}),
                })
            }
            Err(error) => {
                let message = error.to_string();
                let _ = events
                    .send(SessionEvent::new(
                        session_id,
                        SessionEventKind::Failed {
                            message: message.clone(),
                        },
                    ))
                    .await;
                Ok(AttemptOutcome::Broken(GoalResult {
                    terminal: TerminalKind::Failed,
                    summary: message,
                    artifacts: vec![],
                    diagnostics: serde_json::json!({"error": "coder_backend"}),
                }))
            }
        }
    }
}
