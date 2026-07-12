//! Coding domain pack adapter for [`liberado_session::DomainPackRunner`].
//!
//! Bridges `LiberadoLoopBackend` into the goal-session kernel so TUI/WebUI can drive coding
//! goals without owning the loop. Optional: only used when server registers this pack.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use liberado_coder_core::{
    CoderBackend, CoderRoleConfig, CoderRunConfig, CoderRunRequest, CoderTask, CommandPolicy,
    LIBERADO_LOOP_BACKEND, PathPolicy, ProgressPolicy, SandboxSpec, WorkspaceRef,
};
use liberado_common::Outcome;
use liberado_provider::Provider;
use liberado_session::{
    CODING_DOMAIN, DomainPackRunner, GoalResult, GoalSpec, PackError, SessionEvent,
    SessionEventKind, TerminalKind,
};
use tokio::sync::mpsc::Sender;

use crate::LiberadoLoopBackend;

/// Runs coding goals via [`LiberadoLoopBackend`].
pub struct CodingSessionPack {
    backend: LiberadoLoopBackend,
    /// Default workspace when payload.workspace_root is absent (temp parent for demos).
    default_workspace_parent: PathBuf,
}

impl CodingSessionPack {
    pub fn new(provider: Arc<dyn Provider>, default_workspace_parent: PathBuf) -> Self {
        Self {
            backend: LiberadoLoopBackend::new(provider),
            default_workspace_parent,
        }
    }

    pub fn with_backend(backend: LiberadoLoopBackend, default_workspace_parent: PathBuf) -> Self {
        Self {
            backend,
            default_workspace_parent,
        }
    }
}

#[async_trait]
impl DomainPackRunner for CodingSessionPack {
    fn domain_id(&self) -> &str {
        CODING_DOMAIN
    }

    async fn run(
        &self,
        session_id: &str,
        goal: &GoalSpec,
        events: Sender<SessionEvent>,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        if *cancel.borrow() {
            return Err(PackError::Cancelled);
        }

        let workspace = goal
            .payload
            .get("workspace_root")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let dir = self
                    .default_workspace_parent
                    .join(format!("goal-{session_id}"));
                let _ = std::fs::create_dir_all(&dir);
                dir
            });

        let model = goal
            .payload
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("session-coder")
            .to_string();

        let prompt = goal
            .payload
            .get("coder_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or(
                "You are Liberado's coding worker. Inspect, edit with tools, then submit_report.",
            )
            .to_string();

        let max_turns = if goal.max_turns > 0 {
            goal.max_turns
        } else {
            12
        };

        let role = CoderRoleConfig {
            model: model.clone(),
            prompt_path: None,
            prompt: Some(prompt),
            temperature: Some(0.1),
            max_tokens: None,
            max_turns: Some(max_turns),
        };
        let disabled = CoderRoleConfig {
            model: model.clone(),
            prompt_path: None,
            prompt: None,
            temperature: None,
            max_tokens: None,
            max_turns: Some(2),
        };

        let mut task = CoderTask::new(session_id, &goal.description);
        task.success_criteria = goal.success_criteria.clone();

        let request = CoderRunRequest {
            task,
            workspace: WorkspaceRef::new(workspace.to_string_lossy(), "HEAD"),
            config: CoderRunConfig {
                backend: LIBERADO_LOOP_BACKEND.into(),
                trace_dir: None,
                planner: disabled.clone(),
                coder: role.clone(),
                critic: disabled,
                repair: Some(role),
                sandbox: SandboxSpec::HostLocal,
                command_policy: CommandPolicy::default(),
                validation_command: None,
                verifiers: Vec::new(),
                verify_policy: Default::default(),
                path_policy: PathPolicy::default(),
                progress: ProgressPolicy {
                    max_attempts: 2,
                    ..ProgressPolicy::default()
                },
            },
            attempt: 0,
            prior_feedback: Vec::new(),
        };

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleStarted {
                    role: "coder".into(),
                    model,
                },
            ))
            .await;

        // Race coding run against cancel (best-effort; LiberadoLoopBackend is not yet cancel-aware).
        let run_fut = self.backend.run(request);
        tokio::pin!(run_fut);

        let result = tokio::select! {
            r = &mut run_fut => r,
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    return Err(PackError::Cancelled);
                }
                run_fut.await
            }
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
            Ok(r) => {
                let ok = r.outcome == Outcome::Succeeded;
                let _ = events
                    .send(SessionEvent::new(
                        session_id,
                        SessionEventKind::ValidationFinished {
                            ok,
                            summary: r
                                .validation_notes
                                .clone()
                                .unwrap_or_else(|| r.summary.clone()),
                        },
                    ))
                    .await;
                Ok(GoalResult {
                    terminal: if ok {
                        TerminalKind::Succeeded
                    } else {
                        TerminalKind::Failed
                    },
                    summary: r.summary,
                    artifacts: r.files_changed,
                    diagnostics: r.diagnostics,
                })
            }
            Err(e) => {
                let msg = e.to_string();
                let _ = events
                    .send(SessionEvent::new(
                        session_id,
                        SessionEventKind::Failed {
                            message: msg.clone(),
                        },
                    ))
                    .await;
                Ok(GoalResult {
                    terminal: TerminalKind::Failed,
                    summary: msg,
                    artifacts: vec![],
                    diagnostics: serde_json::json!({"error": "coder_backend"}),
                })
            }
        }
    }
}
