//! Load, render, compare, and import coding traces.
//!
//! Pure logic only — no CLI/IO policy beyond reading paths the caller supplies. The `liberado
//! coder …` binary is a thin adapter over these functions.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{CoderEvent, CoderTrace};

/// Tools that count as a successful **mutation** for first-mutation metrics.
///
/// Matches the coding pack's write-class surface: file content changes via dedicated tools, not
/// shell `run_command` (whose effect is opaque from the trace alone).
const MUTATION_TOOLS: &[&str] = &["write_file", "edit_file", "apply_patch", "git_commit"];

// ── Load ────────────────────────────────────────────────────────────────────

/// Load a native [`CoderTrace`] from a JSON file.
pub fn load_trace(path: impl AsRef<Path>) -> Result<CoderTrace, String> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Resolve a session id (or partial id / bare stem) to a native trace path under `search_dirs`.
///
/// Prefers an exact `{id}.json` match; otherwise the first file whose stem starts with `id`
/// (so a short ULID prefix works). Explicit paths that already exist are returned as-is.
pub fn resolve_trace_path(id_or_path: &str, search_dirs: &[&Path]) -> Result<PathBuf, String> {
    let as_path = Path::new(id_or_path);
    if as_path.is_file() {
        return Ok(as_path.to_path_buf());
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    for dir in search_dirs {
        if !dir.is_dir() {
            continue;
        }
        let exact = dir.join(format!("{id_or_path}.json"));
        if exact.is_file() {
            return Ok(exact);
        }
        // Also accept an id that already includes `.json` or a full session stem.
        let bare = dir.join(id_or_path);
        if bare.is_file() {
            return Ok(bare);
        }
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                // Skip message exports (`*.messages.json`).
                if path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.ends_with(".messages"))
                {
                    continue;
                }
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                if stem == id_or_path || stem.starts_with(id_or_path) {
                    candidates.push(path);
                }
            }
        }
    }

    match candidates.len() {
        0 => Err(format!(
            "no native coder trace found for '{id_or_path}' under {}",
            search_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        1 => Ok(candidates.remove(0)),
        _ => {
            candidates.sort();
            Err(format!(
                "ambiguous session id '{id_or_path}'; matches:\n  {}",
                candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ))
        }
    }
}

// ── Render ──────────────────────────────────────────────────────────────────

/// Render a native trace as a turn-by-turn human transcript.
///
/// Includes, for each model turn: tools offered, model text (when present), tool calls, subsequent
/// tool results, and any loop-guard events. Other events (role start/finish, session lifecycle)
/// appear as section markers so the timeline stays complete.
pub fn render_transcript(trace: &CoderTrace) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Coder trace: {}\n", trace.session_id));
    out.push_str(&format!(
        "task: {} — {}\n",
        trace.request.task.id, trace.request.task.description
    ));
    if let Some(result) = &trace.result {
        out.push_str(&format!(
            "result: {:?} — {}\n",
            result.outcome, result.summary
        ));
    }
    out.push('\n');

    for event in &trace.events {
        match event {
            CoderEvent::SessionStarted {
                session_id,
                backend,
                task_id,
                at,
            } => {
                out.push_str(&format!(
                    "== session started ==\n  id: {session_id}\n  backend: {backend}\n  task: {task_id}\n  at: {at}\n\n"
                ));
            }
            CoderEvent::RoleStarted { role, model, at } => {
                out.push_str(&format!("-- role started: {role} ({model}) @ {at} --\n\n"));
            }
            CoderEvent::RoleFinished { role, at } => {
                out.push_str(&format!("-- role finished: {role} @ {at} --\n\n"));
            }
            CoderEvent::ModelTurnStarted { role, turn, at } => {
                out.push_str(&format!("## turn {turn} ({role}) started @ {at}\n\n"));
            }
            CoderEvent::ModelTurnFinished {
                role,
                turn,
                tools_offered,
                content,
                finish_reason,
                tool_calls,
                prompt_tokens,
                completion_tokens,
                at,
                ..
            } => {
                out.push_str(&format!("## turn {turn} — {role} finished @ {at}\n"));
                out.push_str(&format!("  finish: {finish_reason}\n"));
                out.push_str(&format!(
                    "  tokens: prompt={prompt_tokens} completion={completion_tokens}\n"
                ));
                out.push_str("  tools offered:\n");
                if tools_offered.is_empty() {
                    out.push_str("    (none)\n");
                } else {
                    for t in tools_offered {
                        out.push_str(&format!("    - {t}\n"));
                    }
                }
                if let Some(text) = content {
                    out.push_str("  model text:\n");
                    for line in text.lines() {
                        out.push_str(&format!("    | {line}\n"));
                    }
                } else {
                    out.push_str("  model text: (none — tool calls only)\n");
                }
                out.push_str("  tool calls:\n");
                if tool_calls.is_empty() {
                    out.push_str("    (none)\n");
                } else {
                    for c in tool_calls {
                        out.push_str(&format!("    → {c}\n"));
                    }
                }
                out.push('\n');
            }
            CoderEvent::ToolStarted {
                name,
                args_preview,
                at,
            } => {
                out.push_str(&format!("  tool start: {name} @ {at}\n"));
                if !args_preview.is_empty() {
                    out.push_str(&format!("    args: {args_preview}\n"));
                }
            }
            CoderEvent::ToolFinished {
                name,
                ok,
                result_preview,
                at,
            } => {
                let mark = if *ok { "ok" } else { "FAILED" };
                out.push_str(&format!("  tool result [{mark}]: {name} @ {at}\n"));
                if !result_preview.is_empty() {
                    for line in result_preview.lines().take(20) {
                        out.push_str(&format!("    | {line}\n"));
                    }
                }
                out.push('\n');
            }
            CoderEvent::FileChanged { path, at } => {
                out.push_str(&format!("  file changed: {path} @ {at}\n\n"));
            }
            CoderEvent::ValidationFinished { ok, summary, at } => {
                let mark = if *ok { "ok" } else { "FAILED" };
                out.push_str(&format!("  validation [{mark}] @ {at}: {summary}\n\n"));
            }
            CoderEvent::LoopGuardTriggered { guard, action, at } => {
                out.push_str(&format!(
                    "  !! guard triggered: {guard} → {action} @ {at}\n\n"
                ));
            }
            CoderEvent::CriticVerdict { verdict, at } => {
                out.push_str(&format!("  critic @ {at}: {verdict:?}\n\n"));
            }
            CoderEvent::ReportFiled {
                outcome,
                summary,
                at,
            } => {
                out.push_str(&format!(
                    "== report filed ==\n  outcome: {outcome:?}\n  summary: {summary}\n  at: {at}\n\n"
                ));
            }
            CoderEvent::SessionFinished { outcome, at } => {
                out.push_str(&format!(
                    "== session finished ==\n  outcome: {outcome:?}\n  at: {at}\n\n"
                ));
            }
        }
    }

    out
}

// ── Compare ─────────────────────────────────────────────────────────────────

/// Side-by-side comparison of two native traces (the metric set that catches harness defects).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceComparison {
    pub a_session_id: String,
    pub b_session_id: String,
    pub turns_used: SideBySide<u32>,
    /// Per-turn tool-offer lists (empty string if that run has no such turn).
    pub tools_offered_per_turn: Vec<SideBySide<Vec<String>>>,
    pub refused_or_failed_calls: SideBySide<Vec<FailedCall>>,
    /// 1-based model turn index of first successful mutation; `None` if never.
    pub first_successful_mutation_turn: SideBySide<Option<u32>>,
    pub terminal: SideBySide<TerminalSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SideBySide<T> {
    pub a: T,
    pub b: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedCall {
    pub name: String,
    pub result_preview: String,
    /// Model turn that was open when the failure was recorded, if known.
    pub turn: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSummary {
    pub outcome: Option<String>,
    pub summary: Option<String>,
    pub cause: String,
}

/// Compute the F2 comparison metrics for two native traces.
pub fn compare_traces(a: &CoderTrace, b: &CoderTrace) -> TraceComparison {
    let a_m = metrics(a);
    let b_m = metrics(b);
    let max_turns = a_m
        .tools_offered_per_turn
        .len()
        .max(b_m.tools_offered_per_turn.len());
    let mut tools_offered_per_turn = Vec::with_capacity(max_turns);
    for i in 0..max_turns {
        tools_offered_per_turn.push(SideBySide {
            a: a_m
                .tools_offered_per_turn
                .get(i)
                .cloned()
                .unwrap_or_default(),
            b: b_m
                .tools_offered_per_turn
                .get(i)
                .cloned()
                .unwrap_or_default(),
        });
    }
    TraceComparison {
        a_session_id: a.session_id.clone(),
        b_session_id: b.session_id.clone(),
        turns_used: SideBySide {
            a: a_m.turns_used,
            b: b_m.turns_used,
        },
        tools_offered_per_turn,
        refused_or_failed_calls: SideBySide {
            a: a_m.refused_or_failed_calls,
            b: b_m.refused_or_failed_calls,
        },
        first_successful_mutation_turn: SideBySide {
            a: a_m.first_successful_mutation_turn,
            b: b_m.first_successful_mutation_turn,
        },
        terminal: SideBySide {
            a: a_m.terminal,
            b: b_m.terminal,
        },
    }
}

struct TraceMetrics {
    turns_used: u32,
    tools_offered_per_turn: Vec<Vec<String>>,
    refused_or_failed_calls: Vec<FailedCall>,
    first_successful_mutation_turn: Option<u32>,
    terminal: TerminalSummary,
}

fn metrics(trace: &CoderTrace) -> TraceMetrics {
    let mut turns_used = 0u32;
    let mut tools_offered_per_turn = Vec::new();
    let mut refused_or_failed_calls = Vec::new();
    let mut first_successful_mutation_turn = None;
    let mut current_turn: Option<u32> = None;
    let mut terminal_outcome: Option<String> = None;
    let mut terminal_summary: Option<String> = None;
    let mut terminal_cause = String::from("no terminal event");

    for event in &trace.events {
        match event {
            CoderEvent::ModelTurnFinished {
                turn,
                tools_offered,
                ..
            } => {
                turns_used = turns_used.max(*turn);
                current_turn = Some(*turn);
                // Keep index = turn-1 alignment when turns are sequential from 1.
                while tools_offered_per_turn.len() < *turn as usize {
                    tools_offered_per_turn.push(Vec::new());
                }
                if let Some(slot) =
                    tools_offered_per_turn.get_mut((*turn as usize).saturating_sub(1))
                {
                    *slot = tools_offered.clone();
                }
            }
            CoderEvent::ToolFinished {
                name,
                ok,
                result_preview,
                ..
            } => {
                if !*ok {
                    refused_or_failed_calls.push(FailedCall {
                        name: name.clone(),
                        result_preview: result_preview.clone(),
                        turn: current_turn,
                    });
                } else if is_mutation_tool(name) && first_successful_mutation_turn.is_none() {
                    first_successful_mutation_turn = current_turn.or(Some(1));
                }
            }
            CoderEvent::FileChanged { .. } => {
                if first_successful_mutation_turn.is_none() {
                    first_successful_mutation_turn = current_turn.or(Some(1));
                }
            }
            CoderEvent::ReportFiled {
                outcome, summary, ..
            } => {
                terminal_outcome = Some(format!("{outcome:?}"));
                terminal_summary = Some(summary.clone());
                terminal_cause = format!("report filed: {outcome:?}");
            }
            CoderEvent::SessionFinished { outcome, .. } => {
                terminal_outcome = Some(format!("{outcome:?}"));
                terminal_cause = format!("session finished: {outcome:?}");
            }
            CoderEvent::LoopGuardTriggered { guard, action, .. } => {
                // Not terminal by itself, but useful if nothing else closed the run.
                if terminal_cause == "no terminal event" {
                    terminal_cause = format!("guard {guard} → {action} (no terminal yet)");
                }
            }
            _ => {}
        }
    }

    if let Some(result) = &trace.result {
        terminal_outcome = Some(format!("{:?}", result.outcome));
        if terminal_summary.is_none() {
            terminal_summary = Some(result.summary.clone());
        }
        terminal_cause = format!("result.outcome: {:?}", result.outcome);
    }

    TraceMetrics {
        turns_used,
        tools_offered_per_turn,
        refused_or_failed_calls,
        first_successful_mutation_turn,
        terminal: TerminalSummary {
            outcome: terminal_outcome,
            summary: terminal_summary,
            cause: terminal_cause,
        },
    }
}

fn is_mutation_tool(name: &str) -> bool {
    MUTATION_TOOLS.contains(&name)
}

/// Human-readable comparison report (table-like text).
pub fn format_comparison(c: &TraceComparison) -> String {
    let mut out = String::new();
    out.push_str("# Trace comparison\n\n");
    out.push_str(&format!("A: {}\nB: {}\n\n", c.a_session_id, c.b_session_id));

    out.push_str("## Turns used\n");
    out.push_str(&format!(
        "  A: {}\n  B: {}\n\n",
        c.turns_used.a, c.turns_used.b
    ));

    out.push_str("## Tools offered per turn\n");
    if c.tools_offered_per_turn.is_empty() {
        out.push_str("  (no model turns)\n");
    } else {
        for (i, side) in c.tools_offered_per_turn.iter().enumerate() {
            let turn = i + 1;
            out.push_str(&format!(
                "  turn {turn}:\n    A: {}\n    B: {}\n",
                fmt_tools(&side.a),
                fmt_tools(&side.b)
            ));
        }
    }
    out.push('\n');

    out.push_str("## Refused / failed calls\n");
    out.push_str(&format!(
        "  A ({}):\n{}",
        c.refused_or_failed_calls.a.len(),
        fmt_failures(&c.refused_or_failed_calls.a)
    ));
    out.push_str(&format!(
        "  B ({}):\n{}",
        c.refused_or_failed_calls.b.len(),
        fmt_failures(&c.refused_or_failed_calls.b)
    ));
    out.push('\n');

    out.push_str("## First successful mutation (turn)\n");
    out.push_str(&format!(
        "  A: {}\n  B: {}\n\n",
        fmt_opt_turn(c.first_successful_mutation_turn.a),
        fmt_opt_turn(c.first_successful_mutation_turn.b)
    ));

    out.push_str("## Terminal cause / outcome\n");
    out.push_str(&format!(
        "  A: outcome={} cause={} summary={}\n",
        c.terminal.a.outcome.as_deref().unwrap_or("—"),
        c.terminal.a.cause,
        c.terminal.a.summary.as_deref().unwrap_or("—")
    ));
    out.push_str(&format!(
        "  B: outcome={} cause={} summary={}\n",
        c.terminal.b.outcome.as_deref().unwrap_or("—"),
        c.terminal.b.cause,
        c.terminal.b.summary.as_deref().unwrap_or("—")
    ));

    out
}

fn fmt_tools(tools: &[String]) -> String {
    if tools.is_empty() {
        "(none)".into()
    } else {
        tools.join(", ")
    }
}

fn fmt_failures(fails: &[FailedCall]) -> String {
    if fails.is_empty() {
        "    (none)\n".into()
    } else {
        fails
            .iter()
            .map(|f| {
                format!(
                    "    - {} (turn {}): {}\n",
                    f.name,
                    f.turn.map(|t| t.to_string()).unwrap_or_else(|| "?".into()),
                    truncate(&f.result_preview, 120)
                )
            })
            .collect()
    }
}

fn fmt_opt_turn(t: Option<u32>) -> String {
    t.map(|n| n.to_string()).unwrap_or_else(|| "never".into())
}

fn truncate(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

// ── Foreign import ──────────────────────────────────────────────────────────

/// Known foreign harness formats the importer understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignTraceFormat {
    /// Kilo Code `api_conversation_history.json` — typically a bare message array, or
    /// `{ "messages": [...] }` / `{ "apiConversationHistory": [...] }`.
    Kilo,
    /// OpenHands-style trajectory — `{ "trajectory": [...] }` events, or a message list.
    OpenHands,
}

/// Our `.messages.json` export shape (same family as native `openai-messages`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesExport {
    pub session_id: String,
    pub messages: Vec<Value>,
}

/// Import a foreign conversation into our `.messages.json` shape.
pub fn import_foreign_messages(
    raw: &Value,
    format: ForeignTraceFormat,
    session_id: impl Into<String>,
) -> Result<MessagesExport, String> {
    let session_id = session_id.into();
    let messages = match format {
        ForeignTraceFormat::Kilo => import_kilo_messages(raw)?,
        ForeignTraceFormat::OpenHands => import_openhands_messages(raw)?,
    };
    Ok(MessagesExport {
        session_id,
        messages,
    })
}

/// Auto-detect Kilo vs OpenHands from top-level keys, then import.
pub fn import_foreign_auto(
    raw: &Value,
    session_id: impl Into<String>,
) -> Result<(ForeignTraceFormat, MessagesExport), String> {
    let format = detect_foreign_format(raw)?;
    let export = import_foreign_messages(raw, format, session_id)?;
    Ok((format, export))
}

fn detect_foreign_format(raw: &Value) -> Result<ForeignTraceFormat, String> {
    if raw.get("trajectory").is_some()
        || raw.get("history").is_some()
        || raw.get("agent_events").is_some()
    {
        return Ok(ForeignTraceFormat::OpenHands);
    }
    if raw.get("apiConversationHistory").is_some() || raw.get("api_conversation_history").is_some()
    {
        return Ok(ForeignTraceFormat::Kilo);
    }
    // Bare array or {messages: [...]} — Kilo's common on-disk shape.
    if raw.is_array() || raw.get("messages").is_some() {
        return Ok(ForeignTraceFormat::Kilo);
    }
    Err(
        "cannot detect foreign trace format (expected Kilo message list or OpenHands trajectory)"
            .into(),
    )
}

fn import_kilo_messages(raw: &Value) -> Result<Vec<Value>, String> {
    let list = extract_message_list(raw)?;
    let mut out = Vec::with_capacity(list.len());
    for (i, msg) in list.iter().enumerate() {
        out.push(normalize_message(msg, i)?);
    }
    Ok(out)
}

fn extract_message_list(raw: &Value) -> Result<&Vec<Value>, String> {
    if let Some(arr) = raw.as_array() {
        return Ok(arr);
    }
    for key in [
        "messages",
        "apiConversationHistory",
        "api_conversation_history",
    ] {
        if let Some(arr) = raw.get(key).and_then(|v| v.as_array()) {
            return Ok(arr);
        }
    }
    Err("Kilo input is not a message array or {messages|apiConversationHistory: [...]}".into())
}

fn import_openhands_messages(raw: &Value) -> Result<Vec<Value>, String> {
    // Prefer an explicit message list when present.
    if let Some(arr) = raw
        .get("messages")
        .and_then(|v| v.as_array())
        .or_else(|| raw.as_array())
        && arr
            .first()
            .and_then(|m| m.get("role"))
            .and_then(|r| r.as_str())
            .is_some()
    {
        let mut out = Vec::with_capacity(arr.len());
        for (i, msg) in arr.iter().enumerate() {
            out.push(normalize_message(msg, i)?);
        }
        return Ok(out);
    }

    let events = raw
        .get("trajectory")
        .or_else(|| raw.get("history"))
        .or_else(|| raw.get("agent_events"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            "OpenHands input needs trajectory/history/agent_events array or a messages list"
                .to_string()
        })?;

    let mut out = Vec::new();
    for event in events {
        if let Some(msg) = openhands_event_to_message(event) {
            out.push(msg);
        }
    }
    if out.is_empty() {
        return Err("OpenHands trajectory produced no messages".into());
    }
    Ok(out)
}

/// Map one OpenHands-style event into a chat message when possible.
fn openhands_event_to_message(event: &Value) -> Option<Value> {
    // Already a chat message.
    if event.get("role").and_then(|r| r.as_str()).is_some() {
        return normalize_message(event, 0).ok();
    }

    let action = event
        .get("action")
        .or_else(|| event.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let args = event.get("args").cloned().unwrap_or(json!({}));
    let content = event
        .get("message")
        .or_else(|| event.get("content"))
        .cloned();

    match action {
        "message" | "user_message" | "UserMessageAction" => {
            let text = content
                .or_else(|| args.get("content").cloned())
                .unwrap_or(json!(""));
            Some(json!({ "role": "user", "content": value_as_text(&text) }))
        }
        "agent" | "assistant" | "MessageAction" | "AgentFinishAction" => {
            let text = content
                .or_else(|| args.get("content").cloned())
                .or_else(|| args.get("thought").cloned())
                .unwrap_or(json!(""));
            let mut msg = json!({ "role": "assistant", "content": value_as_text(&text) });
            if let Some(tool_calls) = event.get("tool_calls").or_else(|| args.get("tool_calls")) {
                msg["tool_calls"] = tool_calls.clone();
            }
            Some(msg)
        }
        "run" | "CmdRunAction" | "IPythonRunCellAction" | "call_tool_mcp" | "tool_call" => {
            let name = args
                .get("name")
                .or_else(|| event.get("tool_name"))
                .or_else(|| args.get("command"))
                .map(value_as_text)
                .unwrap_or_else(|| action.to_string());
            Some(json!({
                "role": "assistant",
                "tool_calls": [{
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": args.to_string(),
                    }
                }]
            }))
        }
        "run_observation" | "CmdOutputObservation" | "tool_result" | "observation" => {
            let name = event
                .get("tool_name")
                .or_else(|| args.get("name"))
                .map(value_as_text)
                .unwrap_or_else(|| "tool".into());
            let body = content
                .or_else(|| args.get("content").cloned())
                .or_else(|| event.get("observation").cloned())
                .unwrap_or(json!(""));
            let is_error = event
                .get("success")
                .and_then(|v| v.as_bool())
                .map(|ok| !ok)
                .or_else(|| event.get("is_error").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            Some(json!({
                "role": "tool",
                "name": name,
                "content": value_as_text(&body),
                "is_error": is_error,
            }))
        }
        _ => {
            // Last resort: if there is readable content, keep it as assistant prose.
            content.map(|c| {
                json!({
                    "role": "assistant",
                    "content": value_as_text(&c),
                })
            })
        }
    }
}

fn normalize_message(msg: &Value, index: usize) -> Result<Value, String> {
    let role = msg
        .get("role")
        .and_then(|r| r.as_str())
        .ok_or_else(|| format!("message[{index}] missing role"))?
        .to_string();

    let mut out = json!({ "role": role });

    if let Some(content) = msg.get("content") {
        out["content"] = Value::String(value_as_text(content));
    }

    if let Some(name) = msg.get("name").and_then(|n| n.as_str()) {
        out["name"] = Value::String(name.to_string());
    }

    if let Some(tool_calls) = msg.get("tool_calls") {
        out["tool_calls"] = tool_calls.clone();
    } else if let Some(function_call) = msg.get("function_call") {
        // Older OpenAI shape → tool_calls array.
        out["tool_calls"] = json!([{
            "type": "function",
            "function": function_call,
        }]);
    }

    if let Some(is_error) = msg.get("is_error") {
        out["is_error"] = is_error.clone();
    } else if role == "tool" {
        // Kilo sometimes uses `error` / status instead.
        if msg.get("error").is_some()
            || msg
                .get("status")
                .and_then(|s| s.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("error"))
        {
            out["is_error"] = json!(true);
        }
    }

    Ok(out)
}

fn value_as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            // Multimodal content blocks: [{type:text, text:...}, ...]
            parts
                .iter()
                .filter_map(|p| {
                    p.get("text")
                        .and_then(|t| t.as_str())
                        .map(str::to_string)
                        .or_else(|| p.as_str().map(str::to_string))
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        other => other.to_string(),
    }
}

/// Write a [`MessagesExport`] as pretty JSON.
pub fn write_messages_export(
    path: impl AsRef<Path>,
    export: &MessagesExport,
) -> Result<(), String> {
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| format!("create dir {}: {e}", parent.display()))?;
    }
    let bytes =
        serde_json::to_vec_pretty(export).map_err(|e| format!("serialize messages export: {e}"))?;
    fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Parse foreign JSON from a file and import.
pub fn import_foreign_file(
    path: impl AsRef<Path>,
    format: Option<ForeignTraceFormat>,
    session_id: Option<String>,
) -> Result<(ForeignTraceFormat, MessagesExport), String> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let raw: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let sid = session_id.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("imported")
            .to_string()
    });
    match format {
        Some(f) => Ok((f, import_foreign_messages(&raw, f, sid)?)),
        None => import_foreign_auto(&raw, sid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CoderRunConfig, CoderRunRequest, CoderRunResult, CoderTask, LIBERADO_LOOP_BACKEND,
        WorkspaceRef,
    };
    use chrono::Utc;
    use liberado_common::Outcome;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_config() -> CoderRunConfig {
        serde_json::from_value(serde_json::json!({
            "backend": "liberado-loop",
            "planner": {"model": "m"},
            "coder": {"model": "m"},
            "critic": {"model": "m"},
            "sandbox": {"backend": "host_local"},
            "command_policy": {"timeout_secs": 10, "output_max_bytes": 1024}
        }))
        .expect("config fixture")
    }

    fn trace_with(session_id: &str, description: &str, events: Vec<CoderEvent>) -> CoderTrace {
        CoderTrace {
            session_id: session_id.into(),
            request: CoderRunRequest {
                task: CoderTask::new("t1", description),
                workspace: WorkspaceRef::new("/w", "HEAD"),
                config: {
                    let mut c = fixture_config();
                    c.backend = LIBERADO_LOOP_BACKEND.into();
                    c.trace_dir = None;
                    c.trace_formats = Vec::new();
                    c
                },
                attempt: 0,
                prior_feedback: Vec::new(),
                strategist_directive: None,
            },
            events,
            result: None,
        }
    }

    fn turn(n: u32, content: Option<&str>, offered: &[&str], calls: &[&str]) -> CoderEvent {
        CoderEvent::ModelTurnFinished {
            role: "coder".into(),
            turn: n,
            tools_offered: offered.iter().map(|s| s.to_string()).collect(),
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
            at: Utc::now(),
        }
    }

    fn tool(name: &str, ok: bool, preview: &str) -> CoderEvent {
        CoderEvent::ToolFinished {
            name: name.into(),
            ok,
            result_preview: preview.into(),
            at: Utc::now(),
        }
    }

    fn tmp_dir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("liberado-trace-view-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    // ── F1 ──────────────────────────────────────────────────────────────────

    #[test]
    fn render_includes_offered_tools_model_text_calls_results_and_guards() {
        let t = trace_with(
            "sess-render",
            "add a button",
            vec![
                turn(
                    1,
                    Some("I'll read the file first."),
                    &["read_file", "write_file", "edit_file"],
                    &["read_file"],
                ),
                tool("read_file", true, "fn main() {}"),
                CoderEvent::LoopGuardTriggered {
                    guard: "read_only_stall".into(),
                    action: "withdraw write_file".into(),
                    at: Utc::now(),
                },
                turn(
                    2,
                    Some("Now I write."),
                    &["read_file", "edit_file"],
                    &["write_file"],
                ),
                tool(
                    "write_file",
                    false,
                    "PROGRESS GUARD (fatal): stop exploring",
                ),
            ],
        );

        let text = render_transcript(&t);

        assert!(
            text.contains("tools offered:"),
            "must list offered tools: {text}"
        );
        assert!(
            text.contains("read_file") && text.contains("write_file"),
            "offered tools must appear: {text}"
        );
        assert!(
            text.contains("I'll read the file first."),
            "model text must appear: {text}"
        );
        assert!(
            text.contains("→ read_file") || text.contains("tool calls:"),
            "tool calls section required: {text}"
        );
        assert!(
            text.contains("tool result [ok]: read_file"),
            "successful tool result must appear: {text}"
        );
        assert!(
            text.contains("tool result [FAILED]: write_file"),
            "failed tool result must appear: {text}"
        );
        assert!(
            text.contains("PROGRESS GUARD"),
            "failure preview must appear: {text}"
        );
        assert!(
            text.contains("guard triggered: read_only_stall"),
            "loop guard events must appear: {text}"
        );
        assert!(
            text.contains("withdraw write_file"),
            "guard action must appear: {text}"
        );
    }

    #[test]
    fn load_and_render_round_trip_from_disk() {
        let dir = tmp_dir();
        let path = dir.join("s1.json");
        let t = trace_with(
            "s1",
            "wire it",
            vec![
                turn(1, Some("hello model"), &["read_file"], &["read_file"]),
                tool("read_file", true, "ok body"),
            ],
        );
        let bytes = serde_json::to_vec_pretty(&t).unwrap();
        fs::write(&path, bytes).unwrap();

        let loaded = load_trace(&path).expect("load");
        let text = render_transcript(&loaded);
        assert!(text.contains("hello model"));
        assert!(text.contains("tool result [ok]: read_file"));
        assert!(text.contains("s1"));

        let resolved = resolve_trace_path("s1", &[&dir]).expect("resolve by id");
        assert_eq!(resolved, path);

        let _ = fs::remove_dir_all(&dir);
    }

    // ── F2 ──────────────────────────────────────────────────────────────────

    #[test]
    fn compare_reports_turns_offers_refusals_mutation_and_terminal() {
        let a = {
            let mut t = trace_with(
                "run-a",
                "task A",
                vec![
                    turn(
                        1,
                        Some("explore"),
                        &["read_file", "search_text"],
                        &["read_file"],
                    ),
                    tool("read_file", true, "src"),
                    turn(
                        2,
                        Some("mutate"),
                        &["read_file", "write_file"],
                        &["write_file"],
                    ),
                    tool("write_file", true, "wrote"),
                    CoderEvent::SessionFinished {
                        outcome: Outcome::Succeeded,
                        at: Utc::now(),
                    },
                ],
            );
            t.result = Some(CoderRunResult {
                backend: "liberado-loop".into(),
                outcome: Outcome::Succeeded,
                summary: "done A".into(),
                files_changed: vec!["a.rs".into()],
                file_changes: Vec::new(),
                validation_notes: None,
                critic_verdict: None,
                gate_votes: Vec::new(),
                trace_path: None,
                diagnostics: json!({}),
            });
            t
        };
        let b = {
            let mut t = trace_with(
                "run-b",
                "task B",
                vec![
                    turn(1, Some("try write early"), &["write_file"], &["write_file"]),
                    tool("write_file", false, "refused by path policy"),
                    turn(2, Some("read only"), &["read_file"], &["read_file"]),
                    tool("read_file", true, "x"),
                    turn(3, Some("give up"), &["read_file"], &[]),
                    CoderEvent::SessionFinished {
                        outcome: Outcome::Failed,
                        at: Utc::now(),
                    },
                ],
            );
            t.result = Some(CoderRunResult {
                backend: "liberado-loop".into(),
                outcome: Outcome::Failed,
                summary: "stuck".into(),
                files_changed: vec![],
                file_changes: Vec::new(),
                validation_notes: None,
                critic_verdict: None,
                gate_votes: Vec::new(),
                trace_path: None,
                diagnostics: json!({}),
            });
            t
        };

        let c = compare_traces(&a, &b);
        let report = format_comparison(&c);

        assert_eq!(c.turns_used.a, 2);
        assert_eq!(c.turns_used.b, 3);
        assert!(
            c.tools_offered_per_turn[0].a.contains(&"read_file".into()),
            "A turn1 offers read_file"
        );
        assert!(
            c.tools_offered_per_turn[0].b.contains(&"write_file".into()),
            "B turn1 offers write_file"
        );
        assert_eq!(c.refused_or_failed_calls.a.len(), 0);
        assert_eq!(c.refused_or_failed_calls.b.len(), 1);
        assert_eq!(c.refused_or_failed_calls.b[0].name, "write_file");
        assert_eq!(c.first_successful_mutation_turn.a, Some(2));
        assert_eq!(c.first_successful_mutation_turn.b, None);
        assert!(
            c.terminal
                .a
                .outcome
                .as_deref()
                .unwrap()
                .contains("Succeeded"),
            "{:?}",
            c.terminal.a
        );
        assert!(
            c.terminal.b.outcome.as_deref().unwrap().contains("Failed"),
            "{:?}",
            c.terminal.b
        );

        // Report text must surface all five metric families.
        for needle in [
            "Turns used",
            "Tools offered per turn",
            "Refused / failed calls",
            "First successful mutation",
            "Terminal cause",
        ] {
            assert!(
                report.contains(needle),
                "comparison report missing '{needle}':\n{report}"
            );
        }
        assert!(report.contains("run-a") && report.contains("run-b"));
    }

    // ── F3 ──────────────────────────────────────────────────────────────────

    #[test]
    fn import_kilo_conversation_history_to_messages_json() {
        let kilo = json!([
            {"role": "user", "content": "fix the watcher"},
            {
                "role": "assistant",
                "content": "I'll inspect the file.",
                "tool_calls": [{
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"a.rs\"}"}
                }]
            },
            {
                "role": "tool",
                "name": "read_file",
                "content": "fn main() {}",
                "is_error": false
            },
            {"role": "assistant", "content": "done"}
        ]);

        let export = import_foreign_messages(&kilo, ForeignTraceFormat::Kilo, "kilo-1")
            .expect("kilo import");
        assert_eq!(export.session_id, "kilo-1");
        assert_eq!(export.messages.len(), 4);
        assert_eq!(export.messages[0]["role"], "user");
        assert_eq!(export.messages[0]["content"], "fix the watcher");
        assert_eq!(export.messages[1]["role"], "assistant");
        assert_eq!(
            export.messages[1]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(export.messages[2]["role"], "tool");
        assert_eq!(export.messages[2]["is_error"], false);
        assert_eq!(export.messages[3]["content"], "done");
    }

    #[test]
    fn import_openhands_trajectory_to_messages_json() {
        let oh = json!({
            "trajectory": [
                {"action": "message", "args": {"content": "implement feature X"}},
                {
                    "action": "run",
                    "args": {"command": "read_file", "path": "src/lib.rs"}
                },
                {
                    "action": "run_observation",
                    "tool_name": "read_file",
                    "content": "pub fn x() {}",
                    "success": true
                },
                {
                    "action": "agent",
                    "args": {"content": "Feature complete."}
                }
            ]
        });

        let export = import_foreign_messages(&oh, ForeignTraceFormat::OpenHands, "oh-1")
            .expect("openhands import");
        assert_eq!(export.session_id, "oh-1");
        assert!(
            export.messages.len() >= 3,
            "expected mapped messages, got {:?}",
            export.messages
        );
        assert_eq!(export.messages[0]["role"], "user");
        assert!(
            export.messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("implement feature X")
        );
        // Somewhere an assistant tool call and a tool result should appear.
        let roles: Vec<&str> = export
            .messages
            .iter()
            .filter_map(|m| m.get("role").and_then(|r| r.as_str()))
            .collect();
        assert!(roles.contains(&"assistant"), "roles={roles:?}");
        assert!(roles.contains(&"tool"), "roles={roles:?}");
    }

    #[test]
    fn import_writes_parseable_messages_file() {
        let dir = tmp_dir();
        let input = dir.join("api_conversation_history.json");
        let output = dir.join("imported.messages.json");
        let kilo = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"}
            ]
        });
        let mut f = fs::File::create(&input).unwrap();
        f.write_all(serde_json::to_vec_pretty(&kilo).unwrap().as_slice())
            .unwrap();

        let (fmt, export) = import_foreign_file(&input, None, Some("imp-1".into())).unwrap();
        assert_eq!(fmt, ForeignTraceFormat::Kilo);
        write_messages_export(&output, &export).unwrap();

        let loaded: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        assert_eq!(loaded["session_id"], "imp-1");
        assert!(
            loaded["messages"].as_array().unwrap().len() >= 2,
            "{loaded}"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
