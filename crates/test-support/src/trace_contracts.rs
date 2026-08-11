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

/// Rebuild the request view for `turn` from an MVL event list.
///
/// Implements the reconstruction checklist in `model-view-log.md`: system text by hash,
/// tool catalogue by digest, full/delta message assembly, sampling params, tools_offered.
pub fn reconstruct_turn(events: &[JsonlEvent], turn: i64) -> Result<ReconstructedTurn, String> {
    // Catalogues: sha256 -> tools array
    let mut catalogs: HashMap<String, Value> = HashMap::new();
    // System texts: sha256 -> text
    let mut systems: HashMap<String, String> = HashMap::new();

    let mut messages: Vec<Value> = Vec::new();
    let mut last_params: BTreeMap<String, Value> = BTreeMap::new();
    let mut last_tools_offered: Vec<String> = Vec::new();
    let mut last_system_sha = String::new();
    let mut last_catalog_sha = String::new();
    let mut found = false;
    let mut prompt_seen = false;
    let mut full_prompt_required = true;

    for ev in events {
        match ev.type_name.as_str() {
            "tool_catalog" => {
                let sha = ev
                    .body
                    .get("sha256")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| "tool_catalog missing sha256".to_string())?
                    .to_string();
                let tools = ev
                    .body
                    .get("tools")
                    .and_then(|value| value.as_array())
                    .ok_or_else(|| "tool_catalog tools must be an array".to_string())?
                    .clone();
                catalogs.insert(sha, Value::Array(tools));
            }
            "context_changed" => full_prompt_required = true,
            "prompt" => {
                let t = ev
                    .body
                    .get("turn")
                    .and_then(|x| x.as_i64())
                    .ok_or_else(|| "prompt missing turn".to_string())?;
                if t > turn {
                    break;
                }

                // Each prompt carries the complete request-time metadata. Do not inherit a
                // missing field from an earlier prompt: that would make an incomplete log look
                // reconstructable.
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
                        if systems.contains_key(&sha) {
                            return Err(format!(
                                "system text for sha {sha} appears more than once"
                            ));
                        }
                        systems.insert(sha.clone(), text.clone());
                    }
                    Some(Value::Null) => {}
                    _ => return Err("prompt.system text must be a string or null".to_string()),
                }
                last_system_sha = sha;

                last_catalog_sha = ev
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
                last_params = params.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

                let offered = ev
                    .body
                    .get("tools_offered")
                    .and_then(|x| x.as_array())
                    .ok_or_else(|| "prompt tools_offered must be an array".to_string())?;
                last_tools_offered = offered
                    .iter()
                    .map(|value| {
                        value.as_str().map(str::to_string).ok_or_else(|| {
                            "prompt tools_offered entries must be strings".to_string()
                        })
                    })
                    .collect::<Result<_, _>>()?;

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
                if full_prompt_required && mode != "full" {
                    let reason = if prompt_seen {
                        "prompt after context_changed"
                    } else {
                        "first prompt"
                    };
                    return Err(format!("{reason} must use messages.mode=full"));
                }
                match mode {
                    "full" => messages = items.clone(),
                    "delta" => messages.extend(items.iter().cloned()),
                    other => return Err(format!("unknown messages.mode: {other}")),
                }
                prompt_seen = true;
                full_prompt_required = false;

                if t == turn {
                    found = true;
                    break;
                }
            }
            _ => {}
        }
    }

    if !found {
        return Err(format!("no prompt for turn {turn}"));
    }

    let system_text = systems
        .get(&last_system_sha)
        .cloned()
        .ok_or_else(|| format!("system text not recoverable for sha {last_system_sha}"))?;
    let tool_definitions = catalogs
        .get(&last_catalog_sha)
        .cloned()
        .ok_or_else(|| format!("tool catalog not recoverable for sha {last_catalog_sha}"))?;

    Ok(ReconstructedTurn {
        turn,
        system_text,
        system_sha256: last_system_sha,
        tool_definitions,
        tool_catalog_sha256: last_catalog_sha,
        messages,
        params: last_params,
        tools_offered: last_tools_offered,
    })
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
}
