//! ToolRuntime wrapper: tracing + coding progress guards.
//!
//! Domain-agnostic idea: wrap any ToolRuntime with session events + progress policy.
//! Implementation is still coding-event typed (`CoderEvent`) until a neutral session event exists.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use liberado_coder_core::CoderEvent;
use liberado_coder_tools::CodingToolRuntime;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};

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

        trace::push_event(
            &self.events,
            CoderEvent::ToolStarted {
                name: call.name.clone(),
                args_preview: trace::preview_value(&call.arguments, self.preview_max_chars),
                at: Utc::now(),
            },
        );
        let result = self.inner.invoke(call).await;
        let result_preview = match &result {
            Ok(value) => trace::preview_str(value, self.preview_max_chars),
            Err(value) => trace::preview_str(value, self.preview_max_chars),
        };
        trace::push_event(
            &self.events,
            CoderEvent::ToolFinished {
                name: call.name.clone(),
                ok: result.is_ok(),
                result_preview: result_preview.clone(),
                at: Utc::now(),
            },
        );

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
