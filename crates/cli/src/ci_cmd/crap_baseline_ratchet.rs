//! Preserve the best CRAP metrics when a green CI run writes its baseline.

use super::{
    BASELINE_FILE, CURRENT_REPORT, CiLog, announce_staged_baseline, stage_ratcheted_baseline,
    write_baseline, write_crap_json,
};
use serde_json::Value;
use std::path::Path;

pub(super) fn write_ratcheted_baseline(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    write_crap_json(log, CURRENT_REPORT)?;
    ratchet_crap_baseline(&log.root)
}

/// Write and stage only on Linux, the host of truth for per-function scores.
pub(super) fn write_and_stage_ratcheted_baseline(
    log: &CiLog,
) -> Result<(), Box<dyn std::error::Error>> {
    if !cfg!(target_os = "linux") {
        eprintln!(
            "[liberado ci] {BASELINE_FILE} write is Linux-only \
             (GitHub's Ubuntu job is the host of truth). Compared only."
        );
        return Ok(());
    }
    write_baseline(log)?;
    announce_staged_baseline(stage_ratcheted_baseline(&log.root)?);
    Ok(())
}

pub(crate) fn ratchet_crap_baseline(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let baseline_path = root.join(BASELINE_FILE);
    let current_path = root.join(CURRENT_REPORT);
    let current: Value = serde_json::from_slice(&std::fs::read(&current_path)?)?;
    let ratcheted = ratcheted_crap_report(&baseline_path, current)?;
    write_crap_report(&baseline_path, &ratcheted)
}

fn ratcheted_crap_report(
    baseline_path: &Path,
    mut current: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    if baseline_path.is_file() {
        let old: Value = serde_json::from_slice(&std::fs::read(baseline_path)?)?;
        keep_worse_existing_crap_entries(&old, &mut current)?;
    }
    Ok(current)
}

fn write_crap_report(
    baseline_path: &Path,
    report: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let serialized = serde_json::to_string_pretty(report)?;
    std::fs::write(baseline_path, format!("{serialized}\n"))?;
    Ok(())
}

/// Keep the complete old measurement when either ratcheted metric got worse. Keeping the
/// entry intact also prevents a coverage drop that raised CRAP from entering the baseline.
pub(crate) fn keep_worse_existing_crap_entries(
    old: &Value,
    current: &mut Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let old_entries = old
        .get("entries")
        .and_then(Value::as_array)
        .ok_or("CRAP baseline has no entries array")?;
    let current_entries = current
        .get_mut("entries")
        .and_then(Value::as_array_mut)
        .ok_or("current CRAP report has no entries array")?;
    let mut used = vec![false; old_entries.len()];
    for entry in current_entries {
        let Some((old_index, old_entry)) = closest_matching_crap_entry(old_entries, &used, entry)
        else {
            continue;
        };
        used[old_index] = true;
        if crap_metric(entry, "crap")? > crap_metric(old_entry, "crap")?
            || crap_metric(entry, "cyclomatic")? > crap_metric(old_entry, "cyclomatic")?
        {
            *entry = old_entry.clone();
        }
    }
    Ok(())
}

fn closest_matching_crap_entry<'a>(
    old_entries: &'a [Value],
    used: &[bool],
    current: &Value,
) -> Option<(usize, &'a Value)> {
    let current_line = current.get("line").and_then(Value::as_u64).unwrap_or(0);
    old_entries
        .iter()
        .enumerate()
        .filter(|(index, old)| !used[*index] && same_crap_function(old, current))
        .min_by_key(|(_, old)| {
            old.get("line")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .abs_diff(current_line)
        })
}

fn same_crap_function(left: &Value, right: &Value) -> bool {
    ["file", "function", "crate"]
        .iter()
        .all(|field| left.get(field) == right.get(field))
}

fn crap_metric(entry: &Value, field: &str) -> Result<f64, Box<dyn std::error::Error>> {
    entry
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("CRAP entry has no numeric {field}").into())
}

pub(crate) fn write_after_success(
    check: Result<(), Box<dyn std::error::Error>>,
    write: impl FnOnce() -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    check?;
    write()
}
