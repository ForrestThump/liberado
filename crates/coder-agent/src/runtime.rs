//! ToolRuntime wrapper: tracing + coding progress guards.
//!
//! Domain-agnostic idea: wrap any ToolRuntime with session events + progress policy.
//! Implementation is still coding-event typed (`CoderEvent`) until a neutral session event exists.
//! When the coding pack scopes [`crate::completion_gate::LIVE_GATE`], tool start/finish also
//! mirror onto the goal session stream (dogfood finding #4).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use liberado_coder_core::CoderEvent;
use liberado_coder_tools::CodingToolRuntime;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use liberado_session::{SessionEvent, SessionEventKind};

use crate::completion_gate::LIVE_GATE;
use crate::progress::{ProgressAction, ProgressGuard};
use crate::trace::{self, EventLog};

pub struct GuardedTracingRuntime {
    inner: CodingToolRuntime,
    events: EventLog,
    progress: Arc<Mutex<ProgressGuard>>,
    preview_max_chars: usize,
}

impl GuardedTracingRuntime {
    pub fn new(
        inner: CodingToolRuntime,
        events: EventLog,
        progress: Arc<Mutex<ProgressGuard>>,
        preview_max_chars: usize,
    ) -> Self {
        Self {
            inner,
            events,
            progress,
            preview_max_chars,
        }
    }
}

/// Best-effort mirror onto the goal session bus when LIVE_GATE is scoped (coding pack build phase).
fn emit_live(kind: SessionEventKind) {
    let Ok((tx, session_id)) = LIVE_GATE.try_with(|(tx, id)| (tx.clone(), id.clone())) else {
        return;
    };
    // try_send: never block the tool loop on a slow UI consumer.
    let _ = tx.try_send(SessionEvent::new(session_id, kind));
}

#[async_trait]
impl ToolRuntime for GuardedTracingRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.inner.catalog()
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        {
            let guard = self.progress.lock().expect("progress mutex poisoned");
            if let Some(fatal) = guard.fatal() {
                return Err(fatal.message());
            }
        }

        let args_preview = trace::preview_value(&call.arguments, self.preview_max_chars);
        trace::push_event(
            &self.events,
            CoderEvent::ToolStarted {
                name: call.name.clone(),
                args_preview: args_preview.clone(),
                at: Utc::now(),
            },
        );
        emit_live(SessionEventKind::ToolStarted {
            name: call.name.clone(),
            args_preview,
        });
        let result = self.inner.invoke(call).await;
        let result_preview = match &result {
            Ok(value) => trace::preview_str(value, self.preview_max_chars),
            Err(value) => trace::preview_str(value, self.preview_max_chars),
        };
        let ok = result.is_ok();
        trace::push_event(
            &self.events,
            CoderEvent::ToolFinished {
                name: call.name.clone(),
                ok,
                result_preview: result_preview.clone(),
                at: Utc::now(),
            },
        );
        emit_live(SessionEventKind::ToolFinished {
            name: call.name.clone(),
            ok,
            result_preview: result_preview.clone(),
        });

        let full_preview = match &result {
            Ok(value) => value.clone(),
            Err(value) => value.clone(),
        };
        let action = self
            .progress
            .lock()
            .expect("progress mutex poisoned")
            .observe(&call.name, result.is_ok(), &full_preview);

        match action {
            ProgressAction::Continue { nudge: None } => result,
            ProgressAction::Continue {
                nudge: Some(message),
            } => {
                trace::push_event(
                    &self.events,
                    CoderEvent::LoopGuardTriggered {
                        guard: "progress_nudge".to_string(),
                        action: "nudge".to_string(),
                        at: Utc::now(),
                    },
                );
                match result {
                    Ok(body) => Ok(format!("{body}\n\n{message}")),
                    Err(body) => Err(format!("{body}\n\n{message}")),
                }
            }
            ProgressAction::Fatal(fatal) => {
                trace::push_event(
                    &self.events,
                    CoderEvent::LoopGuardTriggered {
                        guard: fatal.guard_name().to_string(),
                        action: "fail_tool".to_string(),
                        at: Utc::now(),
                    },
                );
                Err(fatal.message())
            }
        }
    }
}
