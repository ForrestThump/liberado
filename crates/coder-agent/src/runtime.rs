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
            // A latched fatal refuses further *exploration*, which is the point — but it must not
            // refuse the edit it is demanding. This used to return for every tool, ahead of
            // `observe`, which made `observe`'s escape hatch dead code and left the deadlock it
            // was written to fix fully live.
            let guard = self.progress.lock().expect("progress mutex poisoned");
            if let Some(fatal) = guard.fatal()
                && !crate::progress::escapes_fatal(&call.name)
            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::{CommandPolicy, PathPolicy, ProgressPolicy};
    use serde_json::json;

    fn call(name: &str, args: serde_json::Value) -> ToolInvocation {
        ToolInvocation {
            id: "1".into(),
            name: name.into(),
            arguments: args,
        }
    }

    /// A latched progress fatal must not refuse the edit it is demanding.
    ///
    /// `ProgressGuard::observe` has an escape hatch for exactly this, added after a live run where
    /// "8 inspect calls latched ReadOnlyStall, then write_file/write_file/edit_file were all
    /// refused". It was dead code: `invoke` returned on `guard.fatal()` before `observe` ran, so
    /// every tool stayed blocked and the deadlock persisted — a later run reported "All mutation
    /// tools are blocked by the progress guard" and filed a plan it had no way to carry out.
    #[tokio::test]
    async fn a_latched_fatal_still_lets_the_demanded_edit_through() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();

        let inner = CodingToolRuntime::new(
            dir.path(),
            CommandPolicy::none_allowed(),
            PathPolicy::default(),
        )
        .unwrap();
        let guard = Arc::new(Mutex::new(ProgressGuard::new(ProgressPolicy {
            // Nudge at 1 inspect call, latch fatal at 2.
            read_only_turn_limit: 1,
            same_tool_limit: 100,
            ..ProgressPolicy::default()
        })));
        let rt = GuardedTracingRuntime::new(inner, EventLog::default(), guard.clone(), 500);

        // Drive it into a latched ReadOnlyStall.
        for _ in 0..4 {
            let _ = rt.invoke(&call("read_file", json!({"path": "a.rs"}))).await;
        }
        assert!(
            guard.lock().unwrap().fatal().is_some(),
            "test setup: the guard should have latched"
        );

        // Exploration stays refused — that is what the guard is for.
        let refused = rt.invoke(&call("read_file", json!({"path": "a.rs"}))).await;
        assert!(refused.is_err(), "reads must still be refused once latched");

        // The remedy must get through, and must actually reach disk.
        let written = rt
            .invoke(&call(
                "write_file",
                json!({"path": "new.rs", "content": "fn added() {}\n"}),
            ))
            .await;
        assert!(
            written.is_ok(),
            "a latched guard refused the write it was demanding: {written:?}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.rs")).unwrap(),
            "fn added() {}\n"
        );
    }
}
