//! Session event log + optional durable `CoderTrace` artifacts.
//!
//! Note: event vocabulary is currently coding-specialized (`CoderEvent`). Architecture intends a
//! domain-neutral session event envelope later; until then this module stays thin and local.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use liberado_coder_core::{
    CoderError, CoderEvent, CoderRunRequest, CoderRunResult, CoderTrace, TraceFormat,
};
use serde_json::Value;

pub type EventLog = Arc<Mutex<Vec<CoderEvent>>>;

pub fn push_event(events: &EventLog, event: CoderEvent) {
    events
        .lock()
        .expect("coder event mutex poisoned")
        .push(event);
}

pub fn snapshot_events(events: &EventLog) -> Vec<CoderEvent> {
    events.lock().expect("coder event mutex poisoned").clone()
}

/// How much of a tool's arguments and output the **trace** keeps.
///
/// Distinct from `[coder.progress] event_preview_max_chars` (default 500), which sizes the excerpt
/// mirrored onto the live session stream for a human watching a chat pane. That number was doing
/// both jobs, and 500 characters is right for one and useless for the other: the model is fed the
/// tool's *full* output, so a trace that stores the first 500 characters of a compiler error has
/// dropped the part that explains the run. Two of thirty results in the one real trace on disk hit
/// that ceiling — including the `run_command` calls, which is where failures live.
///
/// A constant rather than a setting, deliberately. `docs/future-work/backlog.md` records seven
/// settings that parsed, validated and reached nothing because a consumer hardcoded a literal;
/// `CoderRunConfig` is built by thirteen separate initializers, so an eighth knob here buys
/// tunability nobody asked for at the cost of the exact failure that band F exists to stop. Raise
/// this number if a real trace is truncating something you needed.
pub const TRACE_MAX_CHARS: usize = 20_000;

pub fn preview_value(value: &Value, max_chars: usize) -> String {
    preview_str(&value.to_string(), max_chars)
}

pub fn preview_str(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub fn session_id(request: &CoderRunRequest) -> String {
    format!(
        "{}-attempt-{}-{}",
        safe_segment(&request.task.id),
        request.attempt,
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
    )
}

fn safe_segment(value: &str) -> String {
    let segment = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let segment = segment.trim_matches('-');
    if segment.is_empty() {
        "session".to_string()
    } else {
        segment.to_string()
    }
}

pub async fn write_trace(
    request: &CoderRunRequest,
    session_id: &str,
    events: Vec<CoderEvent>,
    mut result: Option<CoderRunResult>,
) -> Result<Option<String>, CoderError> {
    let Some(trace_dir) = &request.config.trace_dir else {
        return Ok(None);
    };
    let path = trace_file_path(trace_dir, session_id);
    let path_string = path.to_string_lossy().to_string();
    if let Some(result) = &mut result {
        result.trace_path = Some(path_string.clone());
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            CoderError::Setup(format!("create trace dir {}: {e}", parent.display()))
        })?;
    }
    let trace = CoderTrace {
        session_id: session_id.to_string(),
        request: request.clone(),
        events,
        result,
    };
    let bytes = serde_json::to_vec_pretty(&trace)
        .map_err(|e| CoderError::Backend(format!("serialize coder trace: {e}")))?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| CoderError::Backend(format!("write coder trace {}: {e}", path.display())))?;

    // Exports sit beside the native record, never in place of it. A failure to write one must not
    // fail the run or lose the canonical trace we already wrote — the export is regenerable and
    // the run's own result is what the caller came for.
    if request
        .config
        .trace_formats
        .contains(&TraceFormat::OpenaiMessages)
    {
        let export = trace_file_path(trace_dir, &format!("{session_id}.messages"));
        match serde_json::to_vec_pretty(&to_openai_messages(&trace)) {
            Ok(export_bytes) => {
                if let Err(e) = tokio::fs::write(&export, export_bytes).await {
                    tracing::warn!(path = %export.display(), error = %e, "writing message-format trace export failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "serializing message-format trace export failed"),
        }
    }

    Ok(Some(path_string))
}

fn trace_file_path(trace_dir: &str, session_id: &str) -> PathBuf {
    Path::new(trace_dir).join(format!("{session_id}.json"))
}

/// Adapts the executor's domain-neutral [`TurnRecord`] into this pack's event vocabulary.
///
/// The executor knows nothing about coding sessions, so it hands back a neutral record and the
/// pack decides what to store. This is the only path by which the model's own words reach the
/// trace: before it existed, `ModelTurnFinished` was declared in `CoderEvent` and never once
/// emitted, so a finished run recorded which tools ran but not a single thing the model said.
pub struct TurnTracer {
    events: EventLog,
    role: String,
    /// Hashes whose full prompt text has already been written.
    ///
    /// The policy the executor deliberately does not own: record the text once per distinct
    /// prompt and the hash every turn. A 5 KB system prompt over forty turns would otherwise be
    /// 200 KB of the same paragraph.
    seen_prompts: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl TurnTracer {
    pub fn new(events: EventLog, role: impl Into<String>) -> Self {
        Self {
            events,
            role: role.into(),
            seen_prompts: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }
}

impl liberado_executor::TurnObserver for TurnTracer {
    /// Record what the model was sent, with the prompt text once per distinct hash.
    ///
    /// A poisoned lock is recovered rather than propagated: losing the "already written" set
    /// would repeat a prompt in the trace, which is untidy. Panicking inside an observer to avoid
    /// untidiness would take the run with it.
    fn on_request(&self, record: liberado_executor::RequestRecord) {
        let first_time = self
            .seen_prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(record.system_prompt_sha256.clone());
        push_event(
            &self.events,
            CoderEvent::ModelRequestSent {
                role: self.role.clone(),
                turn: record.turn,
                tools_offered: record.tools_offered,
                message_count: record.message_count,
                system_prompt_sha256: record.system_prompt_sha256,
                system_prompt: first_time.then_some(record.system_prompt).flatten(),
                at: chrono::Utc::now(),
            },
        );
    }

    fn on_turn(&self, record: liberado_executor::TurnRecord) {
        // Mirror the model's own words onto the live bus as well as into the trace.
        //
        // Nothing in the coding pack emitted `Token` before this: every `SessionEventKind::Token`
        // in the workspace was a *consumer* (TUI, CLI, face client) and there was no producer, so
        // a watcher saw tools fire with nothing explaining why. The text is already here and
        // already verbatim; it simply never reached the surface.
        //
        // Sent whole rather than as deltas. The executor hands back a finished turn, so there are
        // no incremental chunks to forward — pretending otherwise would mean fabricating a
        // streaming cadence the pack does not have.
        if let Some(text) = record.content.as_deref()
            && !text.trim().is_empty()
        {
            crate::live::emit(liberado_session::SessionEventKind::Token {
                text: text.to_string(),
            });
        }

        push_event(
            &self.events,
            CoderEvent::ModelTurnFinished {
                role: self.role.clone(),
                turn: record.turn,
                tools_offered: record.tools_offered,
                message_count: record.message_count,
                // Verbatim, not previewed — see the field's doc comment.
                content: record.content,
                finish_reason: record.finish_reason.to_string(),
                tool_calls: record.tool_calls,
                prompt_tokens: record.prompt_tokens,
                completion_tokens: record.completion_tokens,
                at: chrono::Utc::now(),
            },
        );
    }
}

/// Project a native trace into a flat OpenAI-style message list.
///
/// This is the shape most other harnesses persist — Kilo Code's `api_conversation_history.json`
/// and OpenHands trajectories are both essentially this — so writing it turns a cross-harness
/// comparison on the same task and model into a near-direct diff.
///
/// **Deliberately lossy.** A message list has no slot for which tools were *offered* on a turn, or
/// for a guard withdrawing one, and those are the facts that diagnose a stuck run. Anything that
/// does not fit is dropped here rather than smuggled into message text, because a half-honest
/// export is worse than a clean one: the native record next to it is complete, and this file can
/// be regenerated from it at any time. Nothing is locked into someone else's schema.
pub fn to_openai_messages(trace: &CoderTrace) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    messages.push(serde_json::json!({
        "role": "user",
        "content": trace.request.task.description,
    }));

    for event in &trace.events {
        match event {
            CoderEvent::ModelTurnFinished {
                content,
                tool_calls,
                ..
            } => {
                let mut msg = serde_json::json!({ "role": "assistant" });
                if let Some(text) = content {
                    msg["content"] = Value::String(text.clone());
                }
                if !tool_calls.is_empty() {
                    msg["tool_calls"] = Value::Array(
                        tool_calls
                            .iter()
                            .map(|name| {
                                // `arguments` is required by the shape, and every other harness
                                // fills it: Kilo carries the real object, so omitting it entirely
                                // made a cross-harness diff read as "they pass arguments, we don't".
                                // The turn record keeps only the called *names*; the arguments are
                                // on the paired `ToolStarted`, which this projection cannot see
                                // from here. Empty and explicit beats absent and mysterious.
                                serde_json::json!({
                                    "type": "function",
                                    "function": { "name": name, "arguments": "" },
                                })
                            })
                            .collect(),
                    );
                }
                messages.push(msg);
            }
            CoderEvent::ToolFinished {
                name,
                ok,
                result_preview,
                ..
            } => {
                messages.push(serde_json::json!({
                    "role": "tool",
                    "name": name,
                    "content": result_preview,
                    // Not part of the message schema, but a failed tool call reads identically to
                    // a successful one without it, and "did that call error?" is the first
                    // question anyone asks of a transcript.
                    "is_error": !ok,
                }));
            }
            _ => {}
        }
    }

    serde_json::json!({
        "session_id": trace.session_id,
        "messages": messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::{CoderRunConfig, CoderTask, WorkspaceRef};

    fn trace_with(events: Vec<CoderEvent>) -> CoderTrace {
        CoderTrace {
            session_id: "s1".into(),
            request: CoderRunRequest {
                task: CoderTask::new("t1", "wire the thing"),
                workspace: WorkspaceRef::new("/w", "HEAD"),
                config: CoderRunConfig {
                    backend: "liberado-loop".into(),
                    trace_dir: None,
                    trace_formats: Vec::new(),
                    ..serde_json::from_value(serde_json::json!({
                        "backend": "liberado-loop",
                        "planner": {"model": "m"},
                        "coder": {"model": "m"},
                        "critic": {"model": "m"},
                        "sandbox": {"backend": "host_local"},
                        "command_policy": {"timeout_secs": 10, "output_max_bytes": 1024}
                    }))
                    .expect("config fixture")
                },
                attempt: 0,
                prior_feedback: Vec::new(),
                strategist_directive: None,
            },
            events,
            result: None,
        }
    }

    fn turn(content: Option<&str>, calls: &[&str]) -> CoderEvent {
        CoderEvent::ModelTurnFinished {
            role: "coder".into(),
            turn: 1,
            tools_offered: vec!["read_file".into(), "write_file".into()],
            message_count: 3,
            content: content.map(str::to_string),
            finish_reason: if calls.is_empty() {
                "prose".into()
            } else {
                "tool_calls".into()
            },
            tool_calls: calls.iter().map(|s| s.to_string()).collect(),
            prompt_tokens: 10,
            completion_tokens: 2,
            at: chrono::Utc::now(),
        }
    }

    /// The export must carry the model's words and its calls — that is the whole reason another
    /// harness's log is comparable to ours.
    #[test]
    fn the_message_export_carries_what_the_model_said_and_called() {
        let t = trace_with(vec![
            turn(Some("I'll start by reading the config."), &["read_file"]),
            CoderEvent::ToolFinished {
                name: "read_file".into(),
                ok: true,
                result_preview: "fn main() {}".into(),
                at: chrono::Utc::now(),
            },
        ]);
        let v = to_openai_messages(&t);
        let msgs = v["messages"].as_array().expect("messages array");

        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "wire the thing");

        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "I'll start by reading the config.");
        assert_eq!(msgs[1]["tool_calls"][0]["function"]["name"], "read_file");
        // `arguments` is part of the shape every other harness fills — Kilo carries the real
        // object. Omitting the key entirely made a cross-harness diff read as a difference in
        // behaviour ("they pass arguments, we don't") rather than one in our projection.
        assert_eq!(
            msgs[1]["tool_calls"][0]["function"]["arguments"], "",
            "arguments must be present and explicitly empty, not absent: {}",
            msgs[1]
        );

        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["name"], "read_file");
        assert_eq!(msgs[2]["is_error"], false);
    }

    /// A failed tool call must not read like a successful one. Today's debugging turned on
    /// distinguishing "the tool ran and returned nothing useful" from "the tool was refused".
    #[test]
    fn a_refused_tool_call_is_marked_as_an_error_in_the_export() {
        let t = trace_with(vec![CoderEvent::ToolFinished {
            name: "write_file".into(),
            ok: false,
            result_preview: "PROGRESS GUARD (fatal): stop exploring".into(),
            at: chrono::Utc::now(),
        }]);
        let v = to_openai_messages(&t);
        let tool_msg = &v["messages"].as_array().unwrap()[1];
        assert_eq!(
            tool_msg["is_error"], true,
            "a refusal must be distinguishable from a result"
        );
    }

    /// The export is knowingly lossy: `tools_offered` has no slot in a message list. This test
    /// pins that as a *decision*, so nobody later "fixes" it by smuggling harness state into
    /// message text — the native trace is where that data lives, and it is written alongside.
    #[test]
    fn the_export_drops_harness_state_rather_than_smuggling_it_into_message_text() {
        let t = trace_with(vec![turn(Some("thinking"), &["read_file"])]);
        let rendered = serde_json::to_string(&to_openai_messages(&t)).unwrap();
        assert!(
            !rendered.contains("tools_offered") && !rendered.contains("write_file"),
            "offered-tool state must not leak into the message export: {rendered}"
        );
    }
}

#[cfg(test)]
mod request_record_tests {
    use super::*;
    use liberado_coder_core::CoderEvent;
    use liberado_executor::{RequestRecord, TurnObserver};

    fn record(turn: u32, prompt: &str) -> RequestRecord {
        let digest = <sha2::Sha256 as sha2::Digest>::digest(prompt.as_bytes());
        RequestRecord {
            turn,
            tools_offered: vec!["grep".into(), "edit_file".into()],
            message_count: turn as usize + 1,
            system_prompt_sha256: format!("{digest:x}"),
            system_prompt: Some(prompt.to_string()),
        }
    }

    fn prompts_in(events: &[CoderEvent]) -> Vec<Option<String>> {
        events
            .iter()
            .filter_map(|e| match e {
                CoderEvent::ModelRequestSent { system_prompt, .. } => Some(system_prompt.clone()),
                _ => None,
            })
            .collect()
    }

    /// The gap this closes: a trace recorded what the model *returned* and nothing about what it
    /// was told. Comparing this harness against another came down to exactly that, and neither
    /// side could answer it.
    #[test]
    fn the_system_prompt_reaches_the_trace() {
        let events: EventLog = Arc::new(Mutex::new(Vec::new()));
        let tracer = TurnTracer::new(events.clone(), "coder");
        tracer.on_request(record(1, "You are Liberado's coding worker."));

        let recorded = snapshot_events(&events);
        let prompts = prompts_in(&recorded);
        assert_eq!(prompts.len(), 1, "one request, one event: {recorded:?}");
        assert_eq!(
            prompts[0].as_deref(),
            Some("You are Liberado's coding worker."),
            "the text must be readable, not only hashed"
        );
    }

    /// Text once per distinct prompt, hash every turn. Forty turns of a 5 KB prompt would
    /// otherwise be 200 KB of the same paragraph.
    #[test]
    fn an_unchanged_prompt_is_written_once() {
        let events: EventLog = Arc::new(Mutex::new(Vec::new()));
        let tracer = TurnTracer::new(events.clone(), "coder");
        for turn in 1..=4 {
            tracer.on_request(record(turn, "same prompt every turn"));
        }

        let recorded = snapshot_events(&events);
        let prompts = prompts_in(&recorded);
        assert_eq!(prompts.len(), 4, "every request is still recorded");
        assert_eq!(
            prompts.iter().filter(|p| p.is_some()).count(),
            1,
            "the text belongs in the trace once: {prompts:?}"
        );
    }

    /// A prompt that changes mid-run must be visible, or the hash is decoration.
    #[test]
    fn a_changed_prompt_is_written_again() {
        let events: EventLog = Arc::new(Mutex::new(Vec::new()));
        let tracer = TurnTracer::new(events.clone(), "coder");
        tracer.on_request(record(1, "first wording"));
        tracer.on_request(record(2, "first wording"));
        tracer.on_request(record(3, "second wording"));

        let prompts = prompts_in(&snapshot_events(&events));
        let texts: Vec<&str> = prompts.iter().filter_map(|p| p.as_deref()).collect();
        assert_eq!(
            texts,
            vec!["first wording", "second wording"],
            "each distinct prompt must appear exactly once"
        );
    }

    /// The hash is on every request even when the text is not, so "is it still that prompt" is
    /// answerable at any turn.
    #[test]
    fn every_request_carries_a_hash() {
        let events: EventLog = Arc::new(Mutex::new(Vec::new()));
        let tracer = TurnTracer::new(events.clone(), "coder");
        tracer.on_request(record(1, "p"));
        tracer.on_request(record(2, "p"));

        let hashes: Vec<String> = snapshot_events(&events)
            .iter()
            .filter_map(|e| match e {
                CoderEvent::ModelRequestSent {
                    system_prompt_sha256,
                    ..
                } => Some(system_prompt_sha256.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0], hashes[1]);
        assert!(!hashes[0].is_empty());
    }

    /// The tools offered are recorded at *request* time. `ModelTurnFinished` records them after
    /// the response, and guards withdraw tools mid-run — so the two can legitimately differ, and
    /// only this one says what the model could actually reach when it chose.
    #[test]
    fn the_offered_tools_are_recorded_with_the_request() {
        let events: EventLog = Arc::new(Mutex::new(Vec::new()));
        let tracer = TurnTracer::new(events.clone(), "coder");
        tracer.on_request(record(1, "p"));

        let offered = snapshot_events(&events)
            .iter()
            .find_map(|e| match e {
                CoderEvent::ModelRequestSent { tools_offered, .. } => Some(tools_offered.clone()),
                _ => None,
            })
            .expect("a request event");
        assert_eq!(offered, vec!["grep".to_string(), "edit_file".to_string()]);
    }
}
