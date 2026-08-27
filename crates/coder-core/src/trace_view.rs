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
/// Render the preamble: session id, task, and result summary.
fn render_header(out: &mut String, trace: &CoderTrace) {
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
}

/// Render turn-timeline events: requests, role starts/finishes, turn start/finish.
fn render_turn_events(out: &mut String, event: &CoderEvent) {
    match event {
        // The system prompt, rendered once — the first turn that carries the text. Later
        // turns repeat only the hash, and a transcript that reprinted a 5 KB prompt forty
        // times would bury everything else. A `None` prompt is that hash-only case.
        CoderEvent::ModelRequestSent {
            turn,
            system_prompt_sha256,
            system_prompt: Some(prompt),
            tools_offered,
            ..
        } => {
            out.push_str(&format!(
                "== system prompt (turn {turn}, sha256 {}) ==
{prompt}

  tools offered: {}

",
                &system_prompt_sha256[..system_prompt_sha256.len().min(12)],
                tools_offered.join(", ")
            ));
        }
        // A later request carries only the hash — print nothing rather than repeat the prompt.
        CoderEvent::ModelRequestSent { .. } => {}
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
        _ => {}
    }
}

/// Render session-lifecycle markers.
fn render_session_events(out: &mut String, event: &CoderEvent) {
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
        CoderEvent::SessionAborted { error, at } => {
            // Loud on purpose. This is the attempt that crashed rather than concluded, and
            // before the trace recorded it the run had no tail at all.
            out.push_str(&format!(
                "== session ABORTED (unhandled error) ==\n  error: {error}\n  at: {at}\n\n"
            ));
        }
        _ => {}
    }
}

/// Render tool and validation activity.
fn render_activity_events(out: &mut String, event: &CoderEvent) {
    match event {
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
        _ => {}
    }
}

/// Render a native trace as a turn-by-turn human transcript.
///
/// Includes, for each model turn: tools offered, model text (when present), tool calls, subsequent
/// tool results, and any loop-guard events. Other events (role start/finish, session lifecycle)
/// appear as section markers so the timeline stays complete.
pub fn render_transcript(trace: &CoderTrace) -> String {
    let mut out = String::new();
    render_header(&mut out, trace);

    for event in &trace.events {
        match event {
            CoderEvent::ModelRequestSent { .. }
            | CoderEvent::RoleStarted { .. }
            | CoderEvent::RoleFinished { .. }
            | CoderEvent::ModelTurnStarted { .. }
            | CoderEvent::ModelTurnFinished { .. } => render_turn_events(&mut out, event),

            CoderEvent::SessionStarted { .. }
            | CoderEvent::ReportFiled { .. }
            | CoderEvent::SessionFinished { .. }
            | CoderEvent::SessionAborted { .. } => render_session_events(&mut out, event),

            CoderEvent::ToolStarted { .. }
            | CoderEvent::ToolFinished { .. }
            | CoderEvent::FileChanged { .. }
            | CoderEvent::ValidationFinished { .. }
            | CoderEvent::LoopGuardTriggered { .. }
            | CoderEvent::CriticVerdict { .. } => render_activity_events(&mut out, event),
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
            CoderEvent::SessionAborted { error, .. } => {
                terminal_outcome = Some("Aborted".to_string());
                terminal_summary = Some(error.clone());
                terminal_cause = format!("session aborted: {error}");
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

// ── Neutral run view + divergence ───────────────────────────────────────────

/// How much of a tool's output the divergence report prints per call.
const DIVERGENCE_OUTPUT_CHARS: usize = 1_200;

/// One run, in terms every harness shares: model turns, and what each turn called.
///
/// The comparison seam. `compare_traces` can only take two native [`CoderTrace`]s, which is why
/// the importer's output had nowhere to go — the two halves of the A/B never met. Both a native
/// trace and an imported foreign run project into this, and the comparison happens here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunView {
    /// Which harness produced it, for the report header.
    pub source: String,
    pub run_id: String,
    /// The task, when the record states it — so a reader can confirm the two runs are comparable
    /// before believing anything below.
    pub task: Option<String>,
    pub turns: Vec<TurnView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnView {
    /// 1-based, within this run.
    pub index: u32,
    pub text: Option<String>,
    pub calls: Vec<CallView>,
    pub finish_reason: Option<String>,
    /// Facts with no counterpart in the other harness — guard trips, tool withdrawals.
    ///
    /// Kept rather than normalized away: "our harness withdrew a tool at this turn and theirs has
    /// no such mechanism" is not noise, it is the answer to why one run failed and the other did
    /// not. Normalizing to the intersection of two harnesses deletes exactly the asymmetry the
    /// comparison exists to find.
    pub annotations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallView {
    pub name: String,
    /// JSON, as a string — the shape OpenAI uses and the one both importers normalize to.
    pub arguments: String,
    /// `None` when the record never says whether the call succeeded.
    pub ok: Option<bool>,
    pub output: String,
}

/// Project a native trace into the neutral view.
pub fn run_view_from_trace(trace: &CoderTrace) -> RunView {
    let mut turns: Vec<TurnView> = Vec::new();
    // Events between one `ModelTurnFinished` and the next belong to the turn that opened them.
    let mut pending_args: Vec<(String, String)> = Vec::new();

    for event in &trace.events {
        match event {
            CoderEvent::ModelTurnFinished {
                turn,
                content,
                finish_reason,
                tool_calls,
                ..
            } => {
                // Seeded from what the model *asked for*, not from what the runtime traced. Not
                // every tool emits `ToolStarted`/`ToolFinished` — `scratchpad_write` emits neither,
                // so five of the thirty-five calls in the one real trace on disk left no event at
                // all. Building the sequence from tool events alone silently dropped them, and
                // since alignment is by call sequence that shifted every subsequent comparison by
                // one. A call with no result recorded keeps `ok: None`, which says exactly that.
                turns.push(TurnView {
                    index: *turn,
                    text: content.clone(),
                    calls: tool_calls
                        .iter()
                        .map(|name| CallView {
                            name: name.clone(),
                            arguments: String::new(),
                            ok: None,
                            output: String::new(),
                        })
                        .collect(),
                    finish_reason: Some(finish_reason.clone()),
                    annotations: Vec::new(),
                });
                pending_args.clear();
            }
            CoderEvent::ToolStarted {
                name, args_preview, ..
            } => {
                pending_args.push((name.clone(), args_preview.clone()));
            }
            CoderEvent::ToolFinished {
                name,
                ok,
                result_preview,
                ..
            } => {
                // Arguments and results are separate events; pair them by name, oldest first,
                // which is the order a turn's calls are issued and completed in.
                let arguments = pending_args
                    .iter()
                    .position(|(n, _)| n == name)
                    .map(|i| pending_args.remove(i).1)
                    .unwrap_or_default();
                let call = CallView {
                    name: name.clone(),
                    arguments,
                    ok: Some(*ok),
                    output: result_preview.clone(),
                };
                match turns.last_mut() {
                    // Fill in the seeded entry this result belongs to rather than appending a
                    // second copy of the same call.
                    Some(turn) => {
                        match turn
                            .calls
                            .iter()
                            .position(|c| c.name == *name && c.ok.is_none())
                        {
                            Some(i) => turn.calls[i] = call,
                            None => turn.calls.push(call),
                        }
                    }
                    None => turns.push(TurnView {
                        index: 1,
                        text: None,
                        calls: vec![call],
                        finish_reason: None,
                        annotations: Vec::new(),
                    }),
                }
            }
            CoderEvent::LoopGuardTriggered { guard, action, .. } => {
                let note = format!("guard {guard} → {action}");
                match turns.last_mut() {
                    Some(turn) => turn.annotations.push(note),
                    None => turns.push(TurnView {
                        index: 1,
                        text: None,
                        calls: Vec::new(),
                        finish_reason: None,
                        annotations: vec![note],
                    }),
                }
            }
            _ => {}
        }
    }

    RunView {
        source: "liberado".into(),
        run_id: trace.session_id.clone(),
        task: Some(trace.request.task.description.clone()),
        turns,
    }
}

/// Project an imported foreign run (our `.messages.json` shape) into the neutral view.
///
/// An **assistant** message is a turn; the `role:"tool"` messages that follow it carry that turn's
/// results. The first user message is taken as the task. Later user messages are recorded as
/// annotations rather than turns — in the Kilo shape they are environment blocks and tool-result
/// carriers, not model activity, and counting them as turns would misalign every comparison.
pub fn run_view_from_messages(export: &MessagesExport, source: impl Into<String>) -> RunView {
    let mut turns: Vec<TurnView> = Vec::new();
    let mut task: Option<String> = None;

    for message in &export.messages {
        let role = message.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or_default();
        match role {
            "user" => {
                if task.is_none() && !content.is_empty() {
                    task = Some(content.to_string());
                } else if !content.is_empty()
                    && let Some(turn) = turns.last_mut()
                {
                    turn.annotations
                        .push(format!("user/env: {}", truncate(content, 120)));
                }
            }
            "assistant" => {
                let calls = message
                    .get("tool_calls")
                    .and_then(|c| c.as_array())
                    .map(|list| {
                        list.iter()
                            .map(|call| CallView {
                                name: call["function"]["name"]
                                    .as_str()
                                    .unwrap_or("unknown")
                                    .to_string(),
                                arguments: call["function"]["arguments"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                // Filled in by the tool messages that follow.
                                ok: None,
                                output: String::new(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                turns.push(TurnView {
                    index: turns.len() as u32 + 1,
                    text: (!content.is_empty()).then(|| content.to_string()),
                    calls,
                    finish_reason: None,
                    annotations: Vec::new(),
                });
            }
            "tool" => {
                let name = message.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let is_error = message
                    .get("is_error")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(false);
                if let Some(turn) = turns.last_mut() {
                    // Match the call this answers: by id when the harness records one, else by
                    // name against the first call still awaiting a result.
                    let id = message.get("tool_call_id").and_then(|i| i.as_str());
                    let index = turn
                        .calls
                        .iter()
                        .position(|c| c.ok.is_none() && (id.is_none() || c.name == name))
                        .or_else(|| turn.calls.iter().position(|c| c.ok.is_none()));
                    if let Some(call) = index.map(|i| &mut turn.calls[i]) {
                        call.ok = Some(!is_error);
                        call.output = content.to_string();
                    }
                }
            }
            _ => {}
        }
    }

    RunView {
        source: source.into(),
        run_id: export.session_id.clone(),
        task,
        turns,
    }
}

/// Load any supported run — native trace or foreign export — into the neutral view.
///
/// Native is tried first because it is the richer record; anything that is not a `CoderTrace`
/// falls through to foreign auto-detection.
pub fn load_run_view(path: impl AsRef<Path>) -> Result<RunView, String> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let raw: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;

    if let Ok(trace) = serde_json::from_value::<CoderTrace>(raw.clone()) {
        return Ok(run_view_from_trace(&trace));
    }
    // Our own `.messages.json` export is already in the imported shape.
    if let Ok(export) = serde_json::from_value::<MessagesExport>(raw.clone())
        && !export.messages.is_empty()
        && raw.get("info").is_none()
    {
        return Ok(run_view_from_messages(&export, "liberado-messages"));
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("imported")
        .to_string();
    let (format, export) = import_foreign_auto(&raw, stem)?;
    Ok(run_view_from_messages(&export, format!("{format:?}")))
}

/// Where two runs stopped doing the same thing.
///
/// Alignment is over the **flattened tool-call sequence**, not the turn index. Turn boundaries are
/// a harness's own bookkeeping — the Kilo CLI closes a separate assistant message for the final
/// `stop`, so its turn 4 and our turn 4 are not the same moment — and aligning by ordinal would
/// confidently compare unrelated steps. Call sequences are the thing both harnesses agree on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Divergence {
    /// Number of leading tool calls that match by name.
    pub common_calls: usize,
    /// The turn each run was on when they parted, if either still had calls left.
    pub a_turn: Option<u32>,
    pub b_turn: Option<u32>,
    /// What each did at that point — `None` when that run had simply stopped calling tools.
    pub a_call: Option<String>,
    pub b_call: Option<String>,
}

/// Flatten to `(turn index, call)` in issue order.
fn call_sequence(run: &RunView) -> Vec<(u32, &CallView)> {
    run.turns
        .iter()
        .flat_map(|t| t.calls.iter().map(move |c| (t.index, c)))
        .collect()
}

pub fn diverge(a: &RunView, b: &RunView) -> Divergence {
    let a_seq = call_sequence(a);
    let b_seq = call_sequence(b);
    let common = a_seq
        .iter()
        .zip(b_seq.iter())
        .take_while(|((_, x), (_, y))| x.name == y.name)
        .count();
    Divergence {
        common_calls: common,
        a_turn: a_seq.get(common).map(|(t, _)| *t),
        b_turn: b_seq.get(common).map(|(t, _)| *t),
        a_call: a_seq.get(common).map(|(_, c)| c.name.clone()),
        b_call: b_seq.get(common).map(|(_, c)| c.name.clone()),
    }
}

/// The report: what both runs did in common, exactly where they parted, and what each did next.
///
/// Written to be read top-to-bottom by a person or a model answering one question — *why did this
/// harness fail where that one did not* — so the shared prefix is compressed to one line per call
/// and the divergence is printed in full, with arguments and output. The metric table in
/// [`format_comparison`] answers a different question (how do two of **our** runs compare) and
/// stays as it is.
pub fn format_divergence(a: &RunView, b: &RunView) -> String {
    let d = diverge(a, b);
    let mut out = String::new();

    out.push_str("# Run divergence\n\n");
    for (label, run) in [("A", a), ("B", b)] {
        out.push_str(&format!(
            "{label}: {} [{}] — {} model turns, {} tool calls\n",
            run.run_id,
            run.source,
            run.turns.len(),
            call_sequence(run).len()
        ));
        out.push_str(&format!(
            "   task: {}\n",
            run.task
                .as_deref()
                .map(|t| truncate(t, 160))
                .unwrap_or_else(|| "(not recorded)".into())
        ));
    }
    out.push_str(
        "\n(aligned on the tool-call sequence, not turn numbers — harnesses count turns differently)\n\n",
    );

    let a_seq = call_sequence(a);
    let b_seq = call_sequence(b);

    out.push_str(&format!("## Agreed for {} call(s)\n", d.common_calls));
    if d.common_calls == 0 {
        out.push_str("  (nothing in common — check these are the same task)\n");
    }
    for (i, (turn, call)) in a_seq.iter().take(d.common_calls).enumerate() {
        let b_ok = b_seq.get(i).and_then(|(_, c)| c.ok);
        out.push_str(&format!(
            "  {:>3}. {} (A turn {}{}, B{})\n",
            i + 1,
            call.name,
            turn,
            fmt_ok(call.ok),
            fmt_ok(b_ok),
        ));
    }
    out.push('\n');

    out.push_str("## Diverged\n");
    match (&d.a_call, &d.b_call) {
        (None, None) => out.push_str("  Both runs stopped calling tools at the same point.\n\n"),
        _ => {
            out.push_str(&format!(
                "  after call {}: A did `{}`, B did `{}`\n\n",
                d.common_calls,
                d.a_call.as_deref().unwrap_or("(stopped)"),
                d.b_call.as_deref().unwrap_or("(stopped)"),
            ));
        }
    }

    for (label, run, turn_no) in [("A", a, d.a_turn), ("B", b, d.b_turn)] {
        out.push_str(&format!("### {label} from the divergence\n"));
        let from = turn_no.unwrap_or_else(|| run.turns.last().map(|t| t.index).unwrap_or(0));
        let tail: Vec<&TurnView> = run.turns.iter().filter(|t| t.index >= from).collect();
        if tail.is_empty() {
            out.push_str("  (no further turns)\n\n");
            continue;
        }
        for turn in tail.iter().take(3) {
            out.push_str(&format!("  turn {}", turn.index));
            if let Some(reason) = &turn.finish_reason {
                out.push_str(&format!(" [{reason}]"));
            }
            out.push('\n');
            if let Some(text) = &turn.text {
                for line in truncate(text, 600).lines() {
                    out.push_str(&format!("    | {line}\n"));
                }
            }
            for call in &turn.calls {
                out.push_str(&format!(
                    "    → {}{} {}\n",
                    call.name,
                    fmt_ok(call.ok),
                    truncate(&call.arguments, 200)
                ));
                if !call.output.is_empty() {
                    for line in truncate(&call.output, DIVERGENCE_OUTPUT_CHARS).lines() {
                        out.push_str(&format!("      {line}\n"));
                    }
                }
            }
            for note in &turn.annotations {
                out.push_str(&format!("    !! {note}\n"));
            }
        }
        let shown = tail.len().min(3);
        if tail.len() > shown {
            out.push_str(&format!("  … {} more turn(s)\n", tail.len() - shown));
        }
        out.push('\n');
    }

    // Asymmetric by nature, and the most likely answer to "why did ours stop": the other harness
    // has no equivalent mechanism, so there is nothing to line these up against.
    for (label, run) in [("A", a), ("B", b)] {
        let notes: Vec<String> = run
            .turns
            .iter()
            .flat_map(|t| {
                t.annotations
                    .iter()
                    .filter(|n| n.starts_with("guard "))
                    .map(move |n| format!("  turn {}: {n}\n", t.index))
            })
            .collect();
        if !notes.is_empty() {
            out.push_str(&format!("## {label} harness interventions\n"));
            out.push_str(&notes.concat());
            out.push('\n');
        }
    }

    out
}

fn fmt_ok(ok: Option<bool>) -> String {
    match ok {
        Some(true) => " ok".into(),
        Some(false) => " FAILED".into(),
        None => "".into(),
    }
}

// ── Foreign import ──────────────────────────────────────────────────────────

/// Known foreign harness formats the importer understands.
///
/// **Kilo ships two unrelated stores**, and which one you have depends on which Kilo you ran:
///
/// | | writes | shape |
/// |---|---|---|
/// | VS Code extension (7.4.x) | `…/globalStorage/kilocode.kilo-code/tasks/<id>/api_conversation_history.json` | [`Kilo`](Self::Kilo) — Anthropic messages |
/// | CLI (`kilo run`, 7.4.x) | SQLite `~/.local/share/kilo/kilo.db`, out via `kilo export <id>` | [`KiloCli`](Self::KiloCli) — `{info, messages:[{info, parts}]}` |
///
/// They share a version number and nothing else. Auto-detection tells them apart, so callers
/// normally need not care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignTraceFormat {
    /// Kilo Code **extension** `api_conversation_history.json` — a bare Anthropic message array
    /// (also accepted wrapped as `{ "messages": … }` / `{ "apiConversationHistory": … }`).
    Kilo,
    /// Kilo Code **CLI** `kilo export <sessionID>` — OpenCode-derived `{info, messages}`, where a
    /// call and its result are one `part` rather than two messages.
    KiloCli,
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
        ForeignTraceFormat::KiloCli => import_kilo_cli_messages(raw)?,
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
    // Checked first: a `kilo export` also has a top-level `messages` array, so the generic
    // message-list branch below would otherwise claim it and then fail on the missing `role`.
    if is_kilo_cli_export(raw) {
        return Ok(ForeignTraceFormat::KiloCli);
    }
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

/// Is this a `kilo export <sessionID>` payload? Keyed on the `{info, parts}` message envelope,
/// which is the one thing no other supported format has.
fn is_kilo_cli_export(raw: &Value) -> bool {
    raw.get("messages")
        .and_then(|m| m.as_array())
        .and_then(|list| list.first())
        .is_some_and(|first| first.get("info").is_some() && first.get("parts").is_some())
}

/// Import a `kilo export <sessionID>` payload (the CLI's SQLite store, dumped as JSON).
///
/// Shape confirmed against real output from Kilo CLI 7.4.11 — the two fixtures in
/// `tests/fixtures/kilo-cli-export-*.json` are verbatim `kilo export` results, not hand-built.
///
/// The structural difference from every other format here: **a call and its result are the same
/// `part`**, `{type:"tool", tool, callID, state:{status, input, output|error}}`, instead of an
/// assistant message answered by a later one. So one part expands into two of our messages — the
/// call on the assistant turn, then a synthesized `role:"tool"` reply — which is what lets a Kilo
/// run line up against ours turn for turn.
///
/// `state.status` is `"completed"` or `"error"`; an errored call has **no `output` key at all**,
/// its message is in `state.error` (verified against a real failed `read`). Parts of type
/// `step-start` / `step-finish` are scaffolding and carry no model-visible content; `reasoning` is
/// dropped for the same reason as in the extension importer.
fn import_kilo_cli_messages(raw: &Value) -> Result<Vec<Value>, String> {
    let list = raw
        .get("messages")
        .and_then(|m| m.as_array())
        .ok_or_else(|| "kilo export has no `messages` array".to_string())?;

    let mut out = Vec::with_capacity(list.len());
    for (index, entry) in list.iter().enumerate() {
        let role = entry
            .get("info")
            .and_then(|i| i.get("role"))
            .and_then(|r| r.as_str())
            .ok_or_else(|| format!("kilo export message[{index}] has no info.role"))?;

        let mut text: Vec<String> = Vec::new();
        let mut tool_calls: Vec<Value> = Vec::new();
        let mut tool_results: Vec<Value> = Vec::new();

        for part in entry
            .get("parts")
            .and_then(|p| p.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            match part.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                "text" => {
                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                        text.push(t.to_string());
                    }
                }
                "tool" => {
                    let name = part
                        .get("tool")
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown");
                    let call_id = part
                        .get("callID")
                        .and_then(|c| c.as_str())
                        .unwrap_or_default();
                    let state = part.get("state");
                    let input = state
                        .and_then(|s| s.get("input"))
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    let mut call = json!({
                        "type": "function",
                        "function": { "name": name, "arguments": input.to_string() },
                    });
                    if !call_id.is_empty() {
                        call["id"] = json!(call_id);
                    }
                    tool_calls.push(call);

                    let status = state
                        .and_then(|s| s.get("status"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    let is_error = status == "error";
                    let body = state
                        .and_then(|s| s.get("output").or_else(|| s.get("error")))
                        .map(value_as_text)
                        .unwrap_or_default();
                    let mut result = json!({
                        "role": "tool",
                        "name": name,
                        "content": body,
                        "is_error": is_error,
                    });
                    if !call_id.is_empty() {
                        result["tool_call_id"] = json!(call_id);
                    }
                    tool_results.push(result);
                }
                _ => {}
            }
        }

        let joined = text.join("\n");
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
        // Results follow the call that produced them — here the call is on *this* message, so
        // unlike the Anthropic shape they come after it rather than before.
        out.append(&mut tool_results);
    }

    if out.is_empty() {
        return Err("kilo export produced no messages".into());
    }
    Ok(out)
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
#[path = "trace_view_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "trace_view_survivor_tests.rs"]
mod survivor_tests;

#[cfg(test)]
#[path = "trace_view_importer_survivor_tests.rs"]
mod importer_survivor_tests;
