//! MVL + execution-log contracts: pure readers and join checks for conformance fixtures.
//!
//! These implement the reconstruction and join rules in
//! `docs/spec/reference/model-view-log.md` and `docs/spec/reference/execution-log.md`.
//! They do **not** emit production logs — that is backlog 0.6.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;

/// One parsed JSONL event (envelope + body as a map).
#[derive(Debug, Clone, PartialEq)]
pub struct JsonlEvent {
    pub v: i64,
    pub type_name: String,
    pub ts: String,
    pub run: String,
    pub seq: i64,
    pub body: BTreeMap<String, Value>,
}

/// Parse a JSONL document into ordered events. Blank lines are skipped.
pub fn parse_jsonl(text: &str) -> Result<Vec<JsonlEvent>, String> {
    let mut out = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(line).map_err(|e| format!("line {}: parse: {e}", line_no + 1))?;
        let obj = value
            .as_object()
            .ok_or_else(|| format!("line {}: expected object", line_no + 1))?;
        let v = obj
            .get("v")
            .and_then(|x| x.as_i64())
            .ok_or_else(|| format!("line {}: missing v", line_no + 1))?;
        let type_name = obj
            .get("type")
            .and_then(|x| x.as_str())
            .ok_or_else(|| format!("line {}: missing type", line_no + 1))?
            .to_string();
        let ts = obj
            .get("ts")
            .and_then(|x| x.as_str())
            .ok_or_else(|| format!("line {}: missing ts", line_no + 1))?
            .to_string();
        let run = obj
            .get("run")
            .and_then(|x| x.as_str())
            .ok_or_else(|| format!("line {}: missing run", line_no + 1))?
            .to_string();
        let seq = obj
            .get("seq")
            .and_then(|x| x.as_i64())
            .ok_or_else(|| format!("line {}: missing seq", line_no + 1))?;
        let mut body = BTreeMap::new();
        for (k, val) in obj {
            if matches!(k.as_str(), "v" | "type" | "ts" | "run" | "seq") {
                continue;
            }
            body.insert(k.clone(), val.clone());
        }
        out.push(JsonlEvent {
            v,
            type_name,
            ts,
            run,
            seq,
            body,
        });
    }
    Ok(out)
}

/// `seq` must be 0..n-1 with no gaps.
pub fn assert_seq_gap_free(events: &[JsonlEvent]) -> Result<(), String> {
    let run = events.first().map(|event| event.run.as_str());
    for (i, ev) in events.iter().enumerate() {
        if Some(ev.run.as_str()) != run {
            return Err(format!(
                "run changed within stream at index {i}: expected {}, got {}",
                run.unwrap_or_default(),
                ev.run
            ));
        }
        if ev.seq != i as i64 {
            return Err(format!(
                "seq gap: expected {i} at index {i}, got {} (type={})",
                ev.seq, ev.type_name
            ));
        }
    }
    Ok(())
}

/// What the model was sent on one turn — recovered from the MVL alone.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconstructedTurn {
    pub turn: i64,
    pub system_text: String,
    pub system_sha256: String,
    pub tool_definitions: Value,
    pub tool_catalog_sha256: String,
    pub messages: Vec<Value>,
    pub params: BTreeMap<String, Value>,
    pub tools_offered: Vec<String>,
}

/// Accumulated reconstruction state as the MVL event list is walked.
#[derive(Default)]
struct ReconstructState {
    /// tool_catalog sha256 -> tools array
    catalogs: HashMap<String, Value>,
    /// prompt.system sha256 -> text
    systems: HashMap<String, String>,
    messages: Vec<Value>,
    last_params: BTreeMap<String, Value>,
    last_tools_offered: Vec<String>,
    last_system_sha: String,
    last_catalog_sha: String,
    prompt_seen: bool,
    full_prompt_required: bool,
}

/// How the walk should proceed after ingesting one prompt.
enum PromptAction {
    /// Keep scanning for the target turn.
    Continue,
    /// Reached the target turn.
    Found,
    /// Passed the target turn; the log has no prompt for it.
    Passed,
}

/// Rebuild the request view for `turn` from an MVL event list.
///
/// Implements the reconstruction checklist in `model-view-log.md`: system text by hash,
/// tool catalogue by digest, full/delta message assembly, sampling params, tools_offered.
pub fn reconstruct_turn(events: &[JsonlEvent], turn: i64) -> Result<ReconstructedTurn, String> {
    let mut state = ReconstructState::default();
    let mut found = false;
    for ev in events {
        match ev.type_name.as_str() {
            "tool_catalog" => ingest_catalog(&ev.body, &mut state.catalogs)?,
            "context_changed" => state.full_prompt_required = true,
            "prompt" => match ingest_prompt(ev, turn, &mut state)? {
                PromptAction::Continue => {}
                PromptAction::Found => {
                    found = true;
                    break;
                }
                PromptAction::Passed => break,
            },
            _ => {}
        }
    }

    if !found {
        return Err(format!("no prompt for turn {turn}"));
    }

    let system_text = state
        .systems
        .get(&state.last_system_sha)
        .cloned()
        .ok_or_else(|| {
            format!(
                "system text not recoverable for sha {}",
                state.last_system_sha
            )
        })?;
    let tool_definitions = state
        .catalogs
        .get(&state.last_catalog_sha)
        .cloned()
        .ok_or_else(|| {
            format!(
                "tool catalog not recoverable for sha {}",
                state.last_catalog_sha
            )
        })?;

    Ok(ReconstructedTurn {
        turn,
        system_text,
        system_sha256: state.last_system_sha,
        tool_definitions,
        tool_catalog_sha256: state.last_catalog_sha,
        messages: state.messages,
        params: state.last_params,
        tools_offered: state.last_tools_offered,
    })
}

/// Index a tool catalogue by its digest.
fn ingest_catalog(
    body: &BTreeMap<String, Value>,
    catalogs: &mut HashMap<String, Value>,
) -> Result<(), String> {
    let sha = body
        .get("sha256")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "tool_catalog missing sha256".to_string())?
        .to_string();
    let tools = body
        .get("tools")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "tool_catalog tools must be an array".to_string())?
        .clone();
    catalogs.insert(sha, Value::Array(tools));
    Ok(())
}

/// Read one prompt event, fold its metadata and messages into `state`, and say how the
/// walk should continue.
fn ingest_prompt(
    ev: &JsonlEvent,
    turn: i64,
    state: &mut ReconstructState,
) -> Result<PromptAction, String> {
    let t = ev
        .body
        .get("turn")
        .and_then(|x| x.as_i64())
        .ok_or_else(|| "prompt missing turn".to_string())?;
    if t > turn {
        return Ok(PromptAction::Passed);
    }

    // Each prompt carries the complete request-time metadata. Do not inherit a
    // missing field from an earlier prompt: that would make an incomplete log look
    // reconstructable.
    ingest_system(ev, state)?;
    state.last_catalog_sha = ev
        .body
        .get("tool_catalog_sha256")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "prompt missing tool_catalog_sha256".to_string())?
        .to_string();

    let params = ev
        .body
        .get("params")
        .and_then(|x| x.as_object())
        .ok_or_else(|| "prompt params must be an object".to_string())?;
    state.last_params = params.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

    let offered = ev
        .body
        .get("tools_offered")
        .and_then(|x| x.as_array())
        .ok_or_else(|| "prompt tools_offered must be an array".to_string())?;
    state.last_tools_offered = offered
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "prompt tools_offered entries must be strings".to_string())
        })
        .collect::<Result<_, _>>()?;

    apply_messages(ev, state)?;

    if t == turn {
        Ok(PromptAction::Found)
    } else {
        Ok(PromptAction::Continue)
    }
}

/// Read and record the prompt's system text (rejecting a sha that already has text).
fn ingest_system(ev: &JsonlEvent, state: &mut ReconstructState) -> Result<(), String> {
    let system = ev
        .body
        .get("system")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "prompt missing system object".to_string())?;
    let sha = system
        .get("sha256")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "prompt.system missing sha256".to_string())?
        .to_string();
    match system.get("text") {
        Some(Value::String(text)) => {
            if state.systems.contains_key(&sha) {
                return Err(format!("system text for sha {sha} appears more than once"));
            }
            state.systems.insert(sha.clone(), text.clone());
        }
        Some(Value::Null) => {}
        _ => return Err("prompt.system text must be a string or null".to_string()),
    }
    state.last_system_sha = sha;
    Ok(())
}

/// Apply the prompt's messages — full replace or delta append — to the assembled view.
fn apply_messages(ev: &JsonlEvent, state: &mut ReconstructState) -> Result<(), String> {
    let messages_obj = ev
        .body
        .get("messages")
        .ok_or_else(|| "prompt missing messages".to_string())?;
    let mode = messages_obj
        .get("mode")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "messages missing mode".to_string())?;
    let items = messages_obj
        .get("items")
        .and_then(|x| x.as_array())
        .ok_or_else(|| "messages missing items".to_string())?;
    if state.full_prompt_required && mode != "full" {
        let reason = if state.prompt_seen {
            "prompt after context_changed"
        } else {
            "first prompt"
        };
        return Err(format!("{reason} must use messages.mode=full"));
    }
    match mode {
        "full" => state.messages = items.clone(),
        "delta" => state.messages.extend(items.iter().cloned()),
        other => return Err(format!("unknown messages.mode: {other}")),
    }
    state.prompt_seen = true;
    state.full_prompt_required = false;
    Ok(())
}

/// Every execution `call_id` for tools must match exactly one MVL completion tool call.
/// Every execution `context_transform` with a `turn` must join an MVL `context_changed` (or a
/// following full `prompt`) for that turn — see execution-log.md conformance item 1.
pub fn assert_join_integrity(mvl: &[JsonlEvent], execution: &[JsonlEvent]) -> Result<(), String> {
    let mut mvl_calls: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut mvl_context_turns: BTreeSet<(String, i64)> = BTreeSet::new();
    let mut mvl_full_prompt_turns: BTreeSet<(String, i64)> = BTreeSet::new();
    for ev in mvl {
        match ev.type_name.as_str() {
            "completion" => {
                if let Some(calls) = ev.body.get("tool_calls").and_then(|x| x.as_array()) {
                    for c in calls {
                        if let Some(id) = c.get("id").and_then(|x| x.as_str()) {
                            *mvl_calls
                                .entry((ev.run.clone(), id.to_string()))
                                .or_default() += 1;
                        }
                    }
                }
            }
            "context_changed" => {
                if let Some(t) = ev.body.get("turn").and_then(|x| x.as_i64()) {
                    mvl_context_turns.insert((ev.run.clone(), t));
                }
            }
            "prompt" => {
                let mode = ev
                    .body
                    .get("messages")
                    .and_then(|m| m.get("mode"))
                    .and_then(|x| x.as_str());
                if mode == Some("full")
                    && let Some(t) = ev.body.get("turn").and_then(|x| x.as_i64())
                {
                    mvl_full_prompt_turns.insert((ev.run.clone(), t));
                }
            }
            _ => {}
        }
    }

    for ev in execution {
        if matches!(
            ev.type_name.as_str(),
            "tool_started" | "tool_finished" | "retry"
        ) {
            let id = ev
                .body
                .get("call_id")
                .and_then(|x| x.as_str())
                .ok_or_else(|| format!("execution {} missing call_id", ev.type_name))?;
            let matches = mvl_calls
                .get(&(ev.run.clone(), id.to_string()))
                .copied()
                .unwrap_or_default();
            if matches != 1 {
                return Err(format!(
                    "execution {} call_id={id} has {matches} MVL tool-call joins for run={} (expected exactly one)",
                    ev.type_name, ev.run,
                ));
            }
        }
        if ev.type_name == "context_transform"
            && let Some(t) = ev.body.get("turn").and_then(|x| x.as_i64())
        {
            let key = (ev.run.clone(), t);
            let has_changed = mvl_context_turns.contains(&key);
            // "Following full prompt" = a full reset at this turn or a later one on the same run.
            let has_following_full = mvl_full_prompt_turns
                .iter()
                .any(|(run, pt)| run == &ev.run && *pt >= t);
            if !has_changed && !has_following_full {
                return Err(format!(
                    "execution context_transform turn={t} has no MVL context_changed \
                     (or following full prompt) for run={}",
                    ev.run
                ));
            }
        }
    }
    Ok(())
}

/// Every attempt_ended has a prior attempt_started with the same attempt index.
pub fn assert_attempt_brackets(execution: &[JsonlEvent]) -> Result<(), String> {
    let mut open: BTreeSet<(String, i64)> = BTreeSet::new();
    for ev in execution {
        match ev.type_name.as_str() {
            "attempt_started" => {
                let a = ev
                    .body
                    .get("attempt")
                    .and_then(|x| x.as_i64())
                    .ok_or_else(|| "attempt_started missing attempt".to_string())?;
                if !open.insert((ev.run.clone(), a)) {
                    return Err(format!(
                        "attempt_started {a} appears twice for run={}",
                        ev.run
                    ));
                }
            }
            "attempt_ended" => {
                let a = ev
                    .body
                    .get("attempt")
                    .and_then(|x| x.as_i64())
                    .ok_or_else(|| "attempt_ended missing attempt".to_string())?;
                if !open.remove(&(ev.run.clone(), a)) {
                    return Err(format!(
                        "attempt_ended {a} without unmatched attempt_started for run={}",
                        ev.run
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// MVL must not contain execution-stream event types (scheduler state).
///
/// Optional timing on `tool_result` is allowed by the MVL example; attempt counters,
/// worker edges and resource samples are not.
pub fn assert_mvl_has_no_scheduler_leakage(mvl: &[JsonlEvent]) -> Result<(), String> {
    for ev in mvl {
        if matches!(
            ev.type_name.as_str(),
            "tool_started"
                | "tool_finished"
                | "attempt_started"
                | "attempt_ended"
                | "worker_edge"
                | "resource_sample"
                | "retry"
                | "gate_result"
                | "run_linked"
                | "context_transform"
        ) {
            return Err(format!(
                "MVL stream contains execution-only type {}",
                ev.type_name
            ));
        }
        // Fields that only the execution log owns.
        if ev.body.contains_key("rss_bytes") || ev.body.contains_key("cpu_ms") {
            return Err(format!(
                "MVL event {} carries resource fields reserved for the execution log",
                ev.type_name
            ));
        }
        if ev.body.contains_key("attempt") && ev.type_name != "run_ended" {
            // coding multi-attempt is execution-side; MVL uses turn indices.
            return Err(format!(
                "MVL event {} carries attempt index (use the execution log)",
                ev.type_name
            ));
        }
    }
    Ok(())
}

/// Crash survival: every non-empty line is a complete JSON object.
///
/// A trailing partial line fails this rule. Producers must append-and-flush complete
/// lines, so a reader must not need to drop a suffix to parse the prefix.
pub fn assert_crash_survival(text: &str) -> Result<Vec<JsonlEvent>, String> {
    let mut complete = String::new();
    for (line_no, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if serde_json::from_str::<Value>(trimmed).is_err() {
            return Err(format!(
                "crash survival: trailing incomplete or invalid JSONL at line {}",
                line_no + 1
            ));
        }
        complete.push_str(trimmed);
        complete.push('\n');
    }
    parse_jsonl(&complete).map_err(|e| format!("crash survival: {e}"))
}

/// Every distinct system prompt appears in full exactly once; every `prompt` carries its hash.
pub fn assert_system_prompt_once(events: &[JsonlEvent]) -> Result<(), String> {
    let mut full_text: HashMap<String, String> = HashMap::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for ev in events {
        if ev.type_name != "prompt" {
            continue;
        }
        let system = ev
            .body
            .get("system")
            .and_then(|value| value.as_object())
            .ok_or_else(|| "system prompt: prompt missing system object".to_string())?;
        let sha = system
            .get("sha256")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "system prompt: prompt.system missing sha256".to_string())?
            .to_string();
        used.insert(sha.clone());
        match system.get("text") {
            Some(Value::String(text)) => {
                if full_text.contains_key(&sha) {
                    return Err(format!(
                        "system prompt: text for sha {sha} appears more than once"
                    ));
                }
                full_text.insert(sha, text.clone());
            }
            Some(Value::Null) => {}
            _ => {
                return Err("system prompt: prompt.system text must be a string or null".into());
            }
        }
    }
    for sha in &used {
        if !full_text.contains_key(sha) {
            return Err(format!("system prompt: text not recoverable for sha {sha}"));
        }
    }
    Ok(())
}

/// Every distinct ordered tool catalogue appears in full exactly once; every `prompt` carries its hash.
pub fn assert_tool_catalog_once(events: &[JsonlEvent]) -> Result<(), String> {
    let mut catalogs: HashMap<String, Value> = HashMap::new();
    for ev in events {
        if ev.type_name != "tool_catalog" {
            continue;
        }
        let sha = ev
            .body
            .get("sha256")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "tool catalog: missing sha256".to_string())?
            .to_string();
        let tools = ev
            .body
            .get("tools")
            .cloned()
            .ok_or_else(|| "tool catalog: missing tools".to_string())?;
        if catalogs.contains_key(&sha) {
            return Err(format!("tool catalog: sha {sha} appears more than once"));
        }
        catalogs.insert(sha, tools);
    }
    for ev in events {
        if ev.type_name != "prompt" {
            continue;
        }
        let sha = ev
            .body
            .get("tool_catalog_sha256")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "tool catalog: prompt missing tool_catalog_sha256".to_string())?;
        if !catalogs.contains_key(sha) {
            return Err(format!("tool catalog: digest {sha} is not recoverable"));
        }
    }
    Ok(())
}

fn string_set(value: Option<&Value>, field: &str) -> Result<BTreeSet<String>, String> {
    let arr = value
        .and_then(|x| x.as_array())
        .ok_or_else(|| format!("{field} must be an array"))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{field} entries must be strings"))
        })
        .collect()
}

/// Whenever consecutive prompts differ in `tools_offered`, an intervening `tools_changed`
/// must list the removal and addition. A narrowed offered list alone is not enough.
pub fn assert_tools_changed_covers_offered_diff(events: &[JsonlEvent]) -> Result<(), String> {
    let mut last_offered: Option<BTreeSet<String>> = None;
    let mut pending_removed = BTreeSet::new();
    let mut pending_added = BTreeSet::new();
    let mut saw_change_event = false;

    for ev in events {
        match ev.type_name.as_str() {
            "tools_changed" => {
                let removed = string_set(ev.body.get("removed"), "tools_changed.removed")?;
                let added = string_set(ev.body.get("added"), "tools_changed.added")?;
                pending_removed.extend(removed);
                pending_added.extend(added);
                saw_change_event = true;
            }
            "prompt" => {
                let offered = string_set(ev.body.get("tools_offered"), "prompt.tools_offered")?;
                if let Some(prev) = &last_offered
                    && offered != *prev
                {
                    if !saw_change_event {
                        return Err(
                            "withdrawal: tools_offered changed without intervening tools_changed"
                                .into(),
                        );
                    }
                    let expected_removed: BTreeSet<_> =
                        prev.difference(&offered).cloned().collect();
                    let expected_added: BTreeSet<_> = offered.difference(prev).cloned().collect();
                    if pending_removed != expected_removed || pending_added != expected_added {
                        return Err(format!(
                            "withdrawal: tools_changed does not cover offered-set diff \
                             (removed={pending_removed:?} expected {expected_removed:?}; \
                             added={pending_added:?} expected {expected_added:?})"
                        ));
                    }
                }
                last_offered = Some(offered);
                pending_removed.clear();
                pending_added.clear();
                saw_change_event = false;
            }
            _ => {}
        }
    }
    Ok(())
}

/// `content_shown` on each supplied `call_id` must byte-equal the caller-provided ground truth.
pub fn assert_tool_honesty(
    events: &[JsonlEvent],
    expected: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for ev in events {
        if ev.type_name != "tool_result" {
            continue;
        }
        let call_id = ev
            .body
            .get("call_id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "tool honesty: tool_result missing call_id".to_string())?;
        let Some(want) = expected.get(call_id) else {
            continue;
        };
        let shown = ev
            .body
            .get("content_shown")
            .and_then(|x| x.as_str())
            .ok_or_else(|| format!("tool honesty: tool_result {call_id} missing content_shown"))?;
        if shown.as_bytes() != want.as_bytes() {
            return Err(format!(
                "tool honesty: content_shown != ground truth for call_id={call_id} \
                 shown_len={} want_len={} shown_prefix={:02x?} want_prefix={:02x?}",
                shown.len(),
                want.len(),
                &shown.as_bytes()[..shown.len().min(12)],
                &want.as_bytes()[..want.len().min(12)],
            ));
        }
        seen.insert(call_id.to_string());
    }
    for id in expected.keys() {
        if !seen.contains(id) {
            return Err(format!(
                "tool honesty: no tool_result for expected call_id={id}"
            ));
        }
    }
    Ok(())
}

/// Reconstruct every turn that has a `prompt` event.
pub fn reconstruct_all_turns(events: &[JsonlEvent]) -> Result<Vec<ReconstructedTurn>, String> {
    let mut turns: BTreeSet<i64> = BTreeSet::new();
    for ev in events {
        if ev.type_name == "prompt"
            && let Some(t) = ev.body.get("turn").and_then(|x| x.as_i64())
        {
            turns.insert(t);
        }
    }
    turns
        .into_iter()
        .map(|t| reconstruct_turn(events, t))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MVL: &str = r#"
{"v":1,"type":"run_started","ts":"2026-08-11T00:00:00.000Z","run":"r1","seq":0,"harness":{"name":"liberado","version":"0.1.0"}}
{"v":1,"type":"tool_catalog","ts":"2026-08-11T00:00:00.001Z","run":"r1","seq":1,"sha256":"cat-aaa","tools":[{"name":"grep","description":"Search","input_schema":{"type":"object"}}]}
{"v":1,"type":"prompt","ts":"2026-08-11T00:00:00.002Z","run":"r1","seq":2,"turn":0,"messages":{"mode":"full","items":[{"role":"user","content":"fix it"}]},"system":{"sha256":"sys-1","text":"You are the coder."},"tool_catalog_sha256":"cat-aaa","tools_offered":["grep"],"params":{"temperature":0.0,"max_tokens":100}}
{"v":1,"type":"completion","ts":"2026-08-11T00:00:00.003Z","run":"r1","seq":3,"turn":0,"text":"searching","tool_calls":[{"id":"c1","name":"grep","arguments":{"pattern":"x"}}],"finish_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"2026-08-11T00:00:00.004Z","run":"r1","seq":4,"turn":0,"call_id":"c1","name":"grep","ok":true,"content_shown":"hit"}
{"v":1,"type":"prompt","ts":"2026-08-11T00:00:00.005Z","run":"r1","seq":5,"turn":1,"messages":{"mode":"delta","items":[{"role":"tool","content":"hit"}]},"system":{"sha256":"sys-1","text":null},"tool_catalog_sha256":"cat-aaa","tools_offered":["grep"],"params":{"temperature":0.0,"max_tokens":100}}
{"v":1,"type":"completion","ts":"2026-08-11T00:00:00.006Z","run":"r1","seq":6,"turn":1,"text":"done","tool_calls":[],"finish_reason":"stop"}
{"v":1,"type":"run_ended","ts":"2026-08-11T00:00:00.007Z","run":"r1","seq":7,"outcome":"succeeded","reason":"model finished","gates":[]}
"#;

    const SAMPLE_EXEC: &str = r#"
{"v":1,"type":"attempt_started","ts":"2026-08-11T00:00:00.000Z","run":"r1","seq":0,"attempt":0,"workspace":"/ws"}
{"v":1,"type":"tool_started","ts":"2026-08-11T00:00:00.003Z","run":"r1","seq":1,"turn":0,"call_id":"c1","name":"grep"}
{"v":1,"type":"tool_finished","ts":"2026-08-11T00:00:00.004Z","run":"r1","seq":2,"turn":0,"call_id":"c1","name":"grep","ok":true,"duration_ms":12,"bytes_out":3}
{"v":1,"type":"gate_result","ts":"2026-08-11T00:00:00.006Z","run":"r1","seq":3,"attempt":0,"name":"nonempty-diff","passed":true}
{"v":1,"type":"attempt_ended","ts":"2026-08-11T00:00:00.007Z","run":"r1","seq":4,"attempt":0,"outcome":"succeeded","reason":"ok"}
"#;

    #[test]
    fn reconstructs_system_messages_catalog_and_params_for_turn_1() {
        let events = parse_jsonl(SAMPLE_MVL).expect("parse");
        assert_seq_gap_free(&events).expect("seq");
        let turn = reconstruct_turn(&events, 1).expect("turn 1");
        assert_eq!(turn.system_text, "You are the coder.");
        assert_eq!(turn.system_sha256, "sys-1");
        assert_eq!(turn.tool_catalog_sha256, "cat-aaa");
        assert_eq!(
            turn.tool_definitions,
            serde_json::json!([{"name":"grep","description":"Search","input_schema":{"type":"object"}}])
        );
        assert_eq!(turn.messages.len(), 2);
        assert_eq!(turn.messages[0]["role"], "user");
        assert_eq!(turn.messages[1]["role"], "tool");
        assert_eq!(
            turn.params.get("temperature").and_then(|v| v.as_f64()),
            Some(0.0)
        );
        assert_eq!(
            turn.params.get("max_tokens").and_then(|v| v.as_i64()),
            Some(100)
        );
        assert_eq!(turn.tools_offered, vec!["grep".to_string()]);
    }

    #[test]
    fn full_prompt_resets_message_list() {
        let text = r#"
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":0,"sha256":"c","tools":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":1,"turn":0,"messages":{"mode":"full","items":[{"role":"user","content":"a"}]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":2,"turn":1,"messages":{"mode":"full","items":[{"role":"user","content":"b"}]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
        let events = parse_jsonl(text).unwrap();
        let turn = reconstruct_turn(&events, 1).unwrap();
        assert_eq!(turn.messages.len(), 1);
        assert_eq!(turn.messages[0]["content"], "b");
    }

    #[test]
    fn missing_catalog_fails_reconstruction() {
        let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"missing","tools_offered":[],"params":{}}
"#;
        let events = parse_jsonl(text).unwrap();
        let err = reconstruct_turn(&events, 0).unwrap_err();
        assert!(err.contains("tool catalog"), "{err}");
    }

    #[test]
    fn target_prompt_must_carry_its_request_metadata() {
        let text = r#"
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":0,"sha256":"c","tools":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":1,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{"temperature":0.0}}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":2,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":[]}
"#;
        let events = parse_jsonl(text).unwrap();
        let err = reconstruct_turn(&events, 1).unwrap_err();
        assert!(err.contains("params"), "{err}");
    }

    #[test]
    fn prompt_after_context_change_must_be_full() {
        let text = r#"
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":0,"sha256":"c","tools":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":1,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
{"v":1,"type":"context_changed","ts":"t","run":"r","seq":2,"turn":1,"kind":"offload","removed_messages":1}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":3,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
        let events = parse_jsonl(text).unwrap();
        let err = reconstruct_turn(&events, 1).unwrap_err();
        assert!(
            err.contains("context_changed") && err.contains("full"),
            "{err}"
        );
    }

    #[test]
    fn execution_joins_mvl_by_call_id() {
        let mvl = parse_jsonl(SAMPLE_MVL).unwrap();
        let ex = parse_jsonl(SAMPLE_EXEC).unwrap();
        assert_seq_gap_free(&ex).unwrap();
        assert_join_integrity(&mvl, &ex).unwrap();
        assert_attempt_brackets(&ex).unwrap();
        assert_mvl_has_no_scheduler_leakage(&mvl).unwrap();
    }

    #[test]
    fn join_fails_when_execution_call_has_no_mvl() {
        let mvl = parse_jsonl(SAMPLE_MVL).unwrap();
        let bad = r#"
{"v":1,"type":"tool_started","ts":"t","run":"r1","seq":0,"turn":0,"call_id":"orphan","name":"grep"}
"#;
        let ex = parse_jsonl(bad).unwrap();
        let err = assert_join_integrity(&mvl, &ex).unwrap_err();
        assert!(err.contains("orphan"), "{err}");
    }

    #[test]
    fn execution_call_does_not_join_an_orphan_tool_result() {
        let mvl = parse_jsonl(
            r#"{"v":1,"type":"tool_result","ts":"t","run":"r","seq":0,"turn":0,"call_id":"c1","name":"x","ok":true,"content_shown":"x"}"#,
        )
        .unwrap();
        let ex = parse_jsonl(
            r#"{"v":1,"type":"tool_started","ts":"t","run":"r","seq":0,"turn":0,"call_id":"c1","name":"x"}"#,
        )
        .unwrap();
        let err = assert_join_integrity(&mvl, &ex).unwrap_err();
        assert!(err.contains("0 MVL tool-call joins"), "{err}");
    }

    #[test]
    fn execution_call_rejects_ambiguous_mvl_tool_calls() {
        let mvl = parse_jsonl(
            r#"
{"v":1,"type":"completion","ts":"t","run":"r","seq":0,"turn":0,"text":"","tool_calls":[{"id":"c1","name":"x","arguments":{}}],"finish_reason":"tool_calls"}
{"v":1,"type":"completion","ts":"t","run":"r","seq":1,"turn":1,"text":"","tool_calls":[{"id":"c1","name":"x","arguments":{}}],"finish_reason":"tool_calls"}
"#,
        )
        .unwrap();
        let ex = parse_jsonl(
            r#"{"v":1,"type":"tool_started","ts":"t","run":"r","seq":0,"turn":0,"call_id":"c1","name":"x"}"#,
        )
        .unwrap();
        let err = assert_join_integrity(&mvl, &ex).unwrap_err();
        assert!(err.contains("2 MVL tool-call joins"), "{err}");
    }

    /// Spec conformance item 1: context_transform + turn must join MVL context_changed (or a
    /// following full prompt). Mutation: drop this check — a green suite would accept the old
    /// non-conforming sample pair that had execution offload without an MVL counterpart.
    #[test]
    fn join_fails_when_context_transform_has_no_mvl_match() {
        let mvl = parse_jsonl(SAMPLE_MVL).unwrap();
        let bad = r#"
{"v":1,"type":"context_transform","ts":"t","run":"r1","seq":0,"turn":1,"kind":"offload","duration_ms":1,"removed_messages":0,"summary_bytes":0}
"#;
        let ex = parse_jsonl(bad).unwrap();
        let err = assert_join_integrity(&mvl, &ex).unwrap_err();
        assert!(
            err.contains("context_transform") && err.contains("context_changed"),
            "{err}"
        );
    }

    #[test]
    fn context_transform_joins_via_context_changed() {
        let mvl = parse_jsonl(
            r#"
{"v":1,"type":"run_started","ts":"t","run":"r1","seq":0,"harness":{"name":"x","version":"0"}}
{"v":1,"type":"context_changed","ts":"t","run":"r1","seq":1,"turn":1,"kind":"offload","removed_messages":0}
{"v":1,"type":"prompt","ts":"t","run":"r1","seq":2,"turn":2,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#,
        )
        .unwrap();
        let ex = parse_jsonl(
            r#"
{"v":1,"type":"context_transform","ts":"t","run":"r1","seq":0,"turn":1,"kind":"offload","duration_ms":1,"removed_messages":0,"summary_bytes":0}
"#,
        )
        .unwrap();
        assert_join_integrity(&mvl, &ex).expect("context_changed joins transform");
    }

    #[test]
    fn mvl_rejects_execution_types() {
        let bad = r#"
{"v":1,"type":"tool_started","ts":"t","run":"r","seq":0,"turn":0,"call_id":"c","name":"x"}
"#;
        let mvl = parse_jsonl(bad).unwrap();
        assert!(assert_mvl_has_no_scheduler_leakage(&mvl).is_err());
    }

    #[test]
    fn attempt_ended_without_start_fails() {
        let bad = r#"
{"v":1,"type":"attempt_ended","ts":"t","run":"r","seq":0,"attempt":0,"outcome":"x","reason":"y"}
"#;
        let ex = parse_jsonl(bad).unwrap();
        assert!(assert_attempt_brackets(&ex).is_err());
    }

    #[test]
    fn attempt_start_matches_only_one_end() {
        let bad = r#"
{"v":1,"type":"attempt_started","ts":"t","run":"r","seq":0,"attempt":0,"workspace":"/ws"}
{"v":1,"type":"attempt_ended","ts":"t","run":"r","seq":1,"attempt":0,"outcome":"x","reason":"y"}
{"v":1,"type":"attempt_ended","ts":"t","run":"r","seq":2,"attempt":0,"outcome":"x","reason":"y"}
"#;
        let ex = parse_jsonl(bad).unwrap();
        let err = assert_attempt_brackets(&ex).unwrap_err();
        assert!(err.contains("without unmatched"), "{err}");
    }

    #[test]
    fn sequence_check_rejects_mixed_runs() {
        let text = r#"
{"v":1,"type":"run_started","ts":"t","run":"r1","seq":0}
{"v":1,"type":"run_ended","ts":"t","run":"r2","seq":1}
"#;
        let events = parse_jsonl(text).unwrap();
        let err = assert_seq_gap_free(&events).unwrap_err();
        assert!(err.contains("run changed"), "{err}");
    }

    #[test]
    fn crash_survival_accepts_complete_prefix() {
        let prefix = SAMPLE_MVL
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(4)
            .collect::<Vec<_>>()
            .join("\n");
        let events = assert_crash_survival(&prefix).expect("complete prefix");
        assert_eq!(events.len(), 4);
        assert_seq_gap_free(&events).expect("prefix seq");
    }

    #[test]
    fn crash_survival_rejects_trailing_partial() {
        let text = format!(
            "{}\n{{\"v\":1,\"type\":\"prompt\",\"ts\":\"t\",\"run\":\"r1\",\"seq\":",
            SAMPLE_MVL.trim()
        );
        let err = assert_crash_survival(&text).unwrap_err();
        assert!(
            err.contains("crash survival") && err.contains("incomplete"),
            "{err}"
        );
    }

    #[test]
    fn system_prompt_once_accepts_sample() {
        let events = parse_jsonl(SAMPLE_MVL).unwrap();
        assert_system_prompt_once(&events).expect("sample system");
    }

    #[test]
    fn system_prompt_once_rejects_duplicate_full_text() {
        let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":1,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
        let events = parse_jsonl(text).unwrap();
        let err = assert_system_prompt_once(&events).unwrap_err();
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn system_prompt_once_rejects_unrecoverable_hash() {
        let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"missing","text":null},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
        let events = parse_jsonl(text).unwrap();
        let err = assert_system_prompt_once(&events).unwrap_err();
        assert!(err.contains("not recoverable"), "{err}");
    }

    #[test]
    fn tool_catalog_once_accepts_sample() {
        let events = parse_jsonl(SAMPLE_MVL).unwrap();
        assert_tool_catalog_once(&events).expect("sample catalog");
    }

    #[test]
    fn tool_catalog_once_rejects_duplicate_sha() {
        let text = r#"
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":0,"sha256":"c","tools":[]}
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":1,"sha256":"c","tools":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":2,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
        let events = parse_jsonl(text).unwrap();
        let err = assert_tool_catalog_once(&events).unwrap_err();
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn tool_catalog_once_rejects_unresolvable_hash() {
        let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"missing","tools_offered":[],"params":{}}
"#;
        let events = parse_jsonl(text).unwrap();
        let err = assert_tool_catalog_once(&events).unwrap_err();
        assert!(err.contains("not recoverable"), "{err}");
    }

    #[test]
    fn withdrawal_accepts_explicit_tools_changed() {
        let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":["a","b"],"params":{}}
{"v":1,"type":"tools_changed","ts":"t","run":"r","seq":1,"turn":0,"removed":["b"],"added":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":2,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":["a"],"params":{}}
"#;
        let events = parse_jsonl(text).unwrap();
        assert_tools_changed_covers_offered_diff(&events).expect("covered");
    }

    #[test]
    fn withdrawal_rejects_offered_shrink_without_tools_changed() {
        let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":["a","b"],"params":{}}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":1,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":["a"],"params":{}}
"#;
        let events = parse_jsonl(text).unwrap();
        let err = assert_tools_changed_covers_offered_diff(&events).unwrap_err();
        assert!(err.contains("without intervening tools_changed"), "{err}");
    }

    #[test]
    fn honesty_accepts_matching_bytes() {
        let events = parse_jsonl(SAMPLE_MVL).unwrap();
        let mut expected = BTreeMap::new();
        expected.insert("c1".into(), "hit".into());
        assert_tool_honesty(&events, &expected).expect("honest");
    }

    #[test]
    fn honesty_rejects_mismatched_content_shown() {
        let events = parse_jsonl(SAMPLE_MVL).unwrap();
        let mut expected = BTreeMap::new();
        expected.insert("c1".into(), "DIFFERENT".into());
        let err = assert_tool_honesty(&events, &expected).unwrap_err();
        assert!(
            err.contains("content_shown != ground truth") && err.contains("c1"),
            "{err}"
        );
    }

    #[test]
    fn reconstruct_all_turns_covers_sample() {
        let events = parse_jsonl(SAMPLE_MVL).unwrap();
        let turns = reconstruct_all_turns(&events).expect("all turns");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].turn, 0);
        assert_eq!(turns[1].turn, 1);
        assert_eq!(turns[0].system_text, "You are the coder.");
    }

    /// Only `prompt` events seed turn reconstruction. The SAMPLE_MVL carries `completion`,
    /// `tool_result`, and other non-prompt events with a `turn` field — cargo-mutants's
    /// `==` -> `!=` mutation on the type-name check would let those events seed turns,
    /// which then fail `reconstruct_turn` (no prompt) and the test sees an Err.
    #[test]
    fn reconstruct_all_turns_only_seeds_from_prompt_events() {
        let events = parse_jsonl(SAMPLE_MVL).unwrap();
        // The unmutated function returns Ok with 2 turns. The `!=` mutation makes
        // `reconstruct_turn` fail for turns sourced from non-prompt events; we expect
        // an error rather than a silently-shorter result.
        let result = reconstruct_all_turns(&events);
        assert!(
            result.is_ok(),
            "sample MVL has a prompt for each turn: {result:?}"
        );
    }

    /// A `context_transform` is joined by a `context_changed` event at the same turn.
    /// The `&&` -> `||` mutation on the `if !has_changed && !has_following_full` guard
    /// would error on a clean context_changed (the `!has_changed` half would flip to
    /// `!has_changed`, true, and combined with `||` the whole condition would fire).
    /// A two-event fixture with just the MVL `context_changed` and the matching
    /// exec `context_transform` (no following full prompt) proves the `&&` is correct.
    #[test]
    fn context_transform_with_only_context_changed_passes_join() {
        let mvl = parse_jsonl(
            r#"
{"v":1,"type":"run_started","ts":"t","run":"r1","seq":0,"harness":{"name":"x","version":"0"}}
{"v":1,"type":"context_changed","ts":"t","run":"r1","seq":1,"turn":1,"kind":"offload","removed_messages":0}
"#,
        )
        .unwrap();
        let ex = parse_jsonl(
            r#"
{"v":1,"type":"context_transform","ts":"t","run":"r1","seq":0,"turn":1,"kind":"offload","duration_ms":1,"removed_messages":0,"summary_bytes":0}
"#,
        )
        .unwrap();
        assert_join_integrity(&mvl, &ex).expect("context_changed alone is sufficient");
    }

    /// The MVL leakage rule rejects `rss_bytes` and `cpu_ms` as execution-only fields.
    /// cargo-mutants's `||` -> `&&` mutation requires BOTH fields to be present before
    /// flagging — a single `rss_bytes` event would pass. A test with just `rss_bytes`
    /// (no `cpu_ms`) pins the original OR semantics.
    #[test]
    fn mvl_rejects_rss_bytes_alone() {
        let mvl = parse_jsonl(
            r#"
{"v":1,"type":"prompt","ts":"t","run":"r1","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{},"rss_bytes":1000}
"#,
        )
        .unwrap();
        let err = assert_mvl_has_no_scheduler_leakage(&mvl).unwrap_err();
        assert!(
            err.contains("rss_bytes") || err.contains("resource"),
            "{err}"
        );
    }
}
