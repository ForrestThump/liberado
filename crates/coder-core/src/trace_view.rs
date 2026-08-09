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

    // The same directory routinely arrives twice: the CLI's defaults are `coder-traces` *and*
    // `<cwd>/coder-traces`, which name one directory whenever the id is resolvable at all. Scanning
    // it twice made a single file look like two matches, so every real session id — real traces are
    // written as `<id>-attempt-N-<stamp>.json`, which only the prefix branch below can match —
    // resolved as "ambiguous". Dedupe by canonical path, falling back to the literal path when a
    // directory cannot be canonicalized.
    let mut seen_dirs: Vec<PathBuf> = Vec::new();
    let mut dirs: Vec<&Path> = Vec::new();
    for dir in search_dirs {
        if !dir.is_dir() {
            continue;
        }
        let key = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        if seen_dirs.contains(&key) {
            continue;
        }
        seen_dirs.push(key);
        dirs.push(dir);
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    for dir in dirs {
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
    /// 1-based model turn index of the first successful **mutation-tool** call; `None` if none ran.
    ///
    /// Deliberately *not* derived from `FileChanged`: the coding pack emits those in one batch
    /// after the loop ends (`coder-agent/src/lib.rs`), so they carry no turn timing. Attributing
    /// them to the open turn reported the *last* turn of every `run_command`-driven run as the
    /// first mutation — "explored for 29 turns, then wrote" for a run that had been editing all
    /// along. Files changed with no attributable call are reported by
    /// [`files_changed`](Self::files_changed) instead.
    pub first_successful_mutation_turn: SideBySide<Option<u32>>,
    /// Distinct files the run changed, from `FileChanged` (or the run result). Says *whether* the
    /// run mutated anything when `first_successful_mutation_turn` cannot say *when*.
    pub files_changed: SideBySide<usize>,
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
        files_changed: SideBySide {
            a: a_m.files_changed,
            b: b_m.files_changed,
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
    files_changed: usize,
    terminal: TerminalSummary,
}

fn metrics(trace: &CoderTrace) -> TraceMetrics {
    let mut turns_used = 0u32;
    let mut tools_offered_per_turn = Vec::new();
    let mut refused_or_failed_calls = Vec::new();
    let mut first_successful_mutation_turn = None;
    let mut changed_paths: Vec<&str> = Vec::new();
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
            CoderEvent::FileChanged { path, .. } => {
                // Counted, never used to date a turn — see `first_successful_mutation_turn`.
                if !changed_paths.contains(&path.as_str()) {
                    changed_paths.push(path.as_str());
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

    // A run whose only record of its edits is the result's file list still changed those files.
    let files_changed = if changed_paths.is_empty() {
        trace.result.as_ref().map_or(0, |r| r.files_changed.len())
    } else {
        changed_paths.len()
    };

    TraceMetrics {
        turns_used,
        tools_offered_per_turn,
        refused_or_failed_calls,
        first_successful_mutation_turn,
        files_changed,
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
        fmt_mutation(c.first_successful_mutation_turn.a, c.files_changed.a),
        fmt_mutation(c.first_successful_mutation_turn.b, c.files_changed.b)
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

/// "when did it first mutate", kept honest about the case where the trace cannot say.
///
/// A run that edits through `run_command` produces no mutation-tool call, so the turn is unknown
/// while the file count is not. Reporting a turn there — or reporting "never" while 38 files
/// changed — are both wrong in ways a reader would act on.
fn fmt_mutation(turn: Option<u32>, files_changed: usize) -> String {
    match (turn, files_changed) {
        (Some(n), _) => n.to_string(),
        (None, 0) => "never".into(),
        (None, n) => format!("no mutation-tool call ({n} file(s) changed, turn unattributable)"),
    }
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
        push_kilo_message(msg, i, list, &mut out)?;
    }
    Ok(out)
}

/// Map one Kilo entry — **Anthropic Messages** shape — into our OpenAI-shaped message list.
///
/// Read off Kilo Code 7.4.20's own reader (`dist/extension.js`), which is the only spec there is:
/// it rejects anything but a top-level JSON array ("Legacy conversation history must be a JSON
/// array"), keeps `role` of `user` or `assistant` **only**, and finds tool activity in `content`
/// blocks — `{type:"tool_use", id, name, input}` inside an assistant entry, answered by
/// `{type:"tool_result", tool_use_id, content, is_error}` inside the *next user* entry.
///
/// There is no `role: "tool"` and no `tool_calls` field anywhere in that file. Reading it as
/// OpenAI-shaped parses cleanly and yields prose with **every tool call and result silently
/// dropped** — an import that looks successful and has removed the only thing worth comparing.
///
/// Deliberately dropped, in the spirit of the native `openai-messages` export: `reasoning` blocks
/// and `reasoning_content`/`reasoning_details`, because folding a model's private reasoning into
/// `content` would make our side and theirs differ for a reason that is not the harness.
/// `<task>` / `<environment_details>` wrappers are **kept** — Kilo strips them for display, but
/// they were part of what the model actually saw, which is the question this export exists to
/// answer.
fn push_kilo_message(
    msg: &Value,
    index: usize,
    all: &[Value],
    out: &mut Vec<Value>,
) -> Result<(), String> {
    let role = msg
        .get("role")
        .and_then(|r| r.as_str())
        .ok_or_else(|| format!("message[{index}] missing role"))?
        .to_string();

    // String content, or an already-OpenAI-shaped entry: the original normalizer handles both, and
    // some exports genuinely are that shape.
    let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) else {
        out.push(normalize_message(msg, index)?);
        return Ok(());
    };

    let mut text: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut tool_results: Vec<Value> = Vec::new();

    for block in blocks {
        match block.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "text" => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    text.push(t.to_string());
                }
            }
            "tool_use" => {
                let name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                // Anthropic carries the arguments as an object under `input`; OpenAI carries a
                // JSON *string* under `arguments`. Serialize so the two are diffable.
                let args = block.get("input").cloned().unwrap_or_else(|| json!({}));
                let mut call = json!({
                    "type": "function",
                    "function": { "name": name, "arguments": args.to_string() },
                });
                if let Some(id) = block.get("id") {
                    call["id"] = id.clone();
                }
                tool_calls.push(call);
            }
            "tool_result" => {
                let use_id = block
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let mut result = json!({
                    // The result block names no tool; only the `tool_use` it answers does.
                    "role": "tool",
                    "name": kilo_tool_name_for_use_id(all, use_id).unwrap_or("unknown"),
                    "content": block.get("content").map(value_as_text).unwrap_or_default(),
                    "is_error": block
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                });
                if !use_id.is_empty() {
                    result["tool_call_id"] = json!(use_id);
                }
                tool_results.push(result);
            }
            _ => {}
        }
    }

    // Results answer the *previous* assistant turn, so they precede this entry's own text — which
    // is what OpenAI ordering expects and what makes the two harnesses line up turn for turn.
    out.append(&mut tool_results);

    let joined = text.join("\n");
    // An assistant entry is always a turn, even when it is pure tool calls: turn counts are half
    // the comparison. A user entry that carried nothing but tool results is not a second turn.
    if role == "assistant" || !joined.is_empty() {
        let mut message = json!({ "role": role });
        if !joined.is_empty() {
            message["content"] = Value::String(joined);
        }
        if !tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(tool_calls);
        }
        out.push(message);
    }
    Ok(())
}

/// Resolve a `tool_result`'s tool name by finding the `tool_use` it answers, anywhere in the
/// conversation — the same lookup Kilo's own `getToolUseFromConversation` does.
fn kilo_tool_name_for_use_id<'a>(all: &'a [Value], use_id: &str) -> Option<&'a str> {
    if use_id.is_empty() {
        return None;
    }
    all.iter()
        .filter_map(|entry| entry.get("content").and_then(|c| c.as_array()))
        .flatten()
        .find(|block| {
            block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                && block.get("id").and_then(|i| i.as_str()) == Some(use_id)
        })
        .and_then(|block| block.get("name").and_then(|n| n.as_str()))
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

    /// The two things that break `liberado coder trace <session-id>` together, and only together:
    /// real traces are written as `<id>-attempt-N-<stamp>.json` (so only the prefix branch can
    /// match them), and the CLI searches one directory under two spellings. Scanning it twice
    /// collected the same file twice and reported the only trace on disk as "ambiguous".
    #[test]
    fn resolves_attempt_suffixed_trace_when_a_dir_is_searched_under_two_spellings() {
        let dir = tmp_dir();
        let real_name = "01KZJ8YSMEEZ5DSQEV4Y16GGFZ-attempt-0-20260809T032711.668Z.json";
        let path = dir.join(real_name);
        let t = trace_with(
            "01KZJ8YSMEEZ5DSQEV4Y16GGFZ-attempt-0-20260809T032711.668Z",
            "cold review",
            vec![turn(1, Some("hi"), &["read_file"], &["read_file"])],
        );
        fs::write(&path, serde_json::to_vec_pretty(&t).unwrap()).unwrap();

        // Same directory, two spellings — what `default_trace_dirs()` hands us for any cwd that
        // actually contains `coder-traces`.
        let canonical = fs::canonicalize(&dir).unwrap();
        let resolved = resolve_trace_path(
            "01KZJ8YSMEEZ5DSQEV4Y16GGFZ",
            &[dir.as_path(), canonical.as_path()],
        )
        .expect("a session id with one trace on disk must resolve, not report ambiguity");
        assert_eq!(
            fs::canonicalize(&resolved).unwrap(),
            fs::canonicalize(&path).unwrap()
        );

        // Two genuinely different sessions sharing a prefix are still ambiguous — the dedupe must
        // not paper over a real collision.
        let other = dir.join("01KZJ8YSMEEZ5DSQEV4Y16GGFZ-attempt-1-20260809T041500.000Z.json");
        fs::write(&other, serde_json::to_vec_pretty(&t).unwrap()).unwrap();
        let err = resolve_trace_path(
            "01KZJ8YSMEEZ5DSQEV4Y16GGFZ",
            &[dir.as_path(), canonical.as_path()],
        )
        .expect_err("two distinct traces sharing the prefix are genuinely ambiguous");
        assert!(err.contains("ambiguous"), "{err}");
        assert_eq!(
            err.matches("attempt-").count(),
            2,
            "each real match listed once, not once per search-dir spelling: {err}"
        );

        let _ = fs::remove_dir_all(&dir);
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

    /// The shape of every real run in `coder-traces/`: the model edits through `run_command`, so
    /// no mutation *tool* ever fires, and the pack emits all `FileChanged` events in one batch
    /// after the loop ends (`coder-agent/src/lib.rs`). Dating the first mutation from those events
    /// named the run's **last** turn — a 30-turn run read as "explored 29 turns, then wrote".
    #[test]
    fn teardown_file_changed_batch_does_not_date_the_first_mutation() {
        let mut t = trace_with(
            "run-shell-edits",
            "edit via shell",
            vec![
                turn(
                    1,
                    Some("look"),
                    &["read_file", "run_command"],
                    &["read_file"],
                ),
                tool("read_file", true, "src"),
                turn(
                    2,
                    Some("patch"),
                    &["read_file", "run_command"],
                    &["run_command"],
                ),
                tool("run_command", true, "applied"),
                turn(
                    3,
                    Some("check"),
                    &["read_file", "run_command"],
                    &["run_command"],
                ),
                tool("run_command", true, "cargo check ok"),
                // Emitted only at teardown — after the last turn, for every file the run touched.
                CoderEvent::FileChanged {
                    path: "crates/daemon/src/lib.rs".into(),
                    at: Utc::now(),
                },
                CoderEvent::FileChanged {
                    path: "crates/daemon/src/react.rs".into(),
                    at: Utc::now(),
                },
                CoderEvent::SessionFinished {
                    outcome: Outcome::Succeeded,
                    at: Utc::now(),
                },
            ],
        );
        t.result = Some(CoderRunResult {
            backend: "liberado-loop".into(),
            outcome: Outcome::Succeeded,
            summary: "done".into(),
            files_changed: vec!["crates/daemon/src/lib.rs".into()],
            file_changes: Vec::new(),
            validation_notes: None,
            critic_verdict: None,
            gate_votes: Vec::new(),
            trace_path: None,
            diagnostics: json!({}),
        });

        let c = compare_traces(&t, &t);

        assert_eq!(
            c.first_successful_mutation_turn.a, None,
            "a teardown FileChanged batch carries no turn timing and must not invent one"
        );
        assert_ne!(
            c.first_successful_mutation_turn.a,
            Some(c.turns_used.a),
            "reporting the last turn as the first mutation is the defect, not the fix"
        );
        assert_eq!(
            c.files_changed.a, 2,
            "the run did mutate; the count is what the trace can honestly say"
        );

        let report = format_comparison(&c);
        assert!(
            report.contains("no mutation-tool call") && report.contains("2 file(s) changed"),
            "report must say it cannot date the mutation, and that files changed anyway:\n{report}"
        );
        assert!(
            !report.contains("First successful mutation (turn)\n  A: 3"),
            "report must not name a turn it cannot know:\n{report}"
        );
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

    /// A fixture in Kilo's **real** on-disk shape, taken from the reader shipped in Kilo Code
    /// 7.4.20 (`dist/extension.js`): a bare JSON array of Anthropic messages, `user`/`assistant`
    /// roles only, tool activity carried as `tool_use` / `tool_result` content blocks.
    ///
    /// The wrong implementation this excludes is the one that was here: reading the file as
    /// OpenAI-shaped (`tool_calls` fields, `role: "tool"` entries). That version parses this input
    /// without error and returns prose with every tool call and result missing — so the assertions
    /// below are on the tool activity, which is the only part that can tell the two apart.
    #[test]
    fn import_kilo_anthropic_blocks_keeps_tool_calls_and_results() {
        let kilo = json!([
            {
                "role": "user",
                "content": [{"type": "text", "text": "<task>fix the watcher</task>"}],
                "ts": 1_754_700_000_000i64
            },
            {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "I'll read the file first."},
                    {
                        "type": "tool_use",
                        "id": "toolu_01A",
                        "name": "read_file",
                        "input": {"path": "crates/daemon/src/vault_source.rs"}
                    }
                ]
            },
            {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_01A",
                        "content": [{"type": "text", "text": "pub fn react() {}"}]
                    },
                    {"type": "text", "text": "<environment_details># VSCode Visible Files</environment_details>"}
                ]
            },
            {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_01B",
                        "name": "apply_diff",
                        "input": {"path": "a.rs", "diff": "-x\n+y"}
                    }
                ]
            },
            {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_01B",
                    "content": "ERROR: no match found",
                    "is_error": true
                }]
            }
        ]);

        let export =
            import_foreign_messages(&kilo, ForeignTraceFormat::Kilo, "kilo-real").expect("import");
        let m = &export.messages;

        // The assistant's tool call survives, with its arguments.
        let call = &m[1]["tool_calls"][0];
        assert_eq!(call["function"]["name"], "read_file", "messages: {m:#?}");
        assert_eq!(call["id"], "toolu_01A");
        let args: Value = serde_json::from_str(call["function"]["arguments"].as_str().unwrap())
            .expect("arguments must be a JSON string, as OpenAI carries them");
        assert_eq!(args["path"], "crates/daemon/src/vault_source.rs");

        // The result comes back as a tool message, named from the `tool_use` it answers.
        assert_eq!(m[2]["role"], "tool");
        assert_eq!(m[2]["name"], "read_file");
        assert_eq!(m[2]["tool_call_id"], "toolu_01A");
        assert_eq!(m[2]["content"], "pub fn react() {}");
        assert_eq!(m[2]["is_error"], false);

        // Text alongside a tool result stays a user turn, and is not merged into the result.
        assert_eq!(m[3]["role"], "user");
        assert!(
            m[3]["content"]
                .as_str()
                .unwrap()
                .contains("environment_details"),
            "what the model saw is kept verbatim: {m:#?}"
        );

        // A tool-call-only assistant entry is still a turn; a failed result keeps its error flag.
        assert_eq!(m[4]["role"], "assistant");
        assert_eq!(m[4]["tool_calls"][0]["function"]["name"], "apply_diff");
        assert_eq!(m[5]["role"], "tool");
        assert_eq!(m[5]["name"], "apply_diff");
        assert_eq!(m[5]["is_error"], true);

        let roles: Vec<&str> = m.iter().filter_map(|x| x["role"].as_str()).collect();
        assert_eq!(
            roles,
            vec!["user", "assistant", "tool", "user", "assistant", "tool"],
            "every turn accounted for, none invented"
        );
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
