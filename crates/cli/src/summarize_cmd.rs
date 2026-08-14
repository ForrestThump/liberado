//! Cross-harness compare-run summaries.
//!
//! Native replacement for scripts/summarize-compare-run.py. It reads loose JSON/JSONL
//! records so foreign harnesses do not need Liberado's trace schema.

use chrono::{DateTime, FixedOffset};
use liberado_common::process::std_command;
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

fn records(path: &Path) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    Ok(fs::read_to_string(path)?
        .lines()
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect())
}

fn text(value: Option<&Value>) -> String {
    value
        .map(|v| v.to_string().trim_matches('"').to_owned())
        .unwrap_or_default()
}

fn time(value: Option<&Value>) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value?.as_str()?.replace('Z', "+00:00").as_str()).ok()
}

fn duration(a: Option<DateTime<FixedOffset>>, b: Option<DateTime<FixedOffset>>) -> Option<f64> {
    Some((b? - a?).num_milliseconds() as f64 / 1000.0)
}

fn duration_text(value: Option<f64>) -> String {
    match value {
        None => "?".into(),
        Some(n) if n < 90.0 => format!("{n:.0}s"),
        Some(n) => format!("{} min {:.0}s", (n / 60.0).round(), n % 60.0),
    }
}

fn counts(values: &BTreeMap<String, usize>) -> String {
    format!(
        "{{{}}}",
        values
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn liberado(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    let request = data.get("request").and_then(Value::as_object);
    let config = request
        .and_then(|r| r.get("config"))
        .and_then(|v| v.get("coder"));
    let events = data
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut turn = 0;
    let mut tools = BTreeMap::new();
    let mut first_edit = None;
    println!(
        "## Liberado  {}",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    println!(
        "- attempt: {}   max_turns: {}   model: {}",
        text(request.and_then(|r| r.get("attempt"))),
        text(config.and_then(|c| c.get("max_turns"))),
        text(config.and_then(|c| c.get("model")))
    );
    println!(
        "- reasoning: {}   wall: {}",
        text(config.and_then(|c| c.get("reasoning"))),
        duration_text(duration(
            events.first().and_then(|e| time(e.get("at"))),
            events.last().and_then(|e| time(e.get("at")))
        ))
    );
    for event in &events {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "model_turn_finished" {
            turn += 1;
        }
        if kind == "tool_started" {
            let name = text(event.get("tool").or_else(|| event.get("name")));
            *tools
                .entry(if name.is_empty() {
                    "?".into()
                } else {
                    name.clone()
                })
                .or_insert(0) += 1;
            if ["edit_file", "write_file", "apply_patch", "hashline_edit"].contains(&name.as_str())
                && first_edit.is_none()
            {
                first_edit = Some(turn);
            }
        }
        if kind == "loop_guard_triggered" {
            println!(
                "- guard ~turn {turn}: {} {}",
                text(event.get("guard")),
                text(event.get("action"))
            );
        } else if ["report_filed", "validation_finished", "session_finished"].contains(&kind) {
            println!(
                "- {kind}: {}",
                text(event.get("outcome").or_else(|| event.get("summary")))
            );
        }
    }
    println!(
        "- turns: {turn}   first mutation: {}   tools: {}",
        first_edit.map_or("None".into(), |n| n.to_string()),
        counts(&tools)
    );
    let sibling = path.with_extension("mvl.jsonl");
    if sibling.is_file() {
        mvl(&sibling, false)?;
    }
    Ok(())
}

fn mvl(path: &Path, heading: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut usage = BTreeMap::<String, f64>::new();
    let mut tools = BTreeMap::new();
    let mut cargo = Vec::new();
    let mut last_text = String::new();
    let mut finish = String::new();
    let mut completions = 0;
    let mut first_edit_turn = None;
    let mut first_edit_path = String::new();
    let error = Regex::new(r"error\[E\d+\]|test \S+ \.\.\. FAILED")?;
    for object in records(path)? {
        let kind = object.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "completion" {
            completions += 1;
            if let Some(values) = object.get("usage").and_then(Value::as_object) {
                for (key, value) in values {
                    if let Some(number) = value.as_f64() {
                        *usage.entry(key.clone()).or_default() += number;
                    }
                }
            }
            if let Some(value) = object
                .get("text")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
            {
                last_text = value.into();
            }
            if let Some(value) = object.get("finish_reason").and_then(Value::as_str) {
                finish = value.into();
            }
            for call in object
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = text(call.get("name"));
                *tools
                    .entry(if name.is_empty() {
                        "?".into()
                    } else {
                        name.clone()
                    })
                    .or_insert(0) += 1;
                let args = call.get("arguments").and_then(Value::as_object);
                if ["edit_file", "write_file", "edit", "write"].contains(&name.as_str())
                    && first_edit_turn.is_none()
                {
                    first_edit_turn = Some(text(object.get("turn")));
                    first_edit_path = text(
                        args.and_then(|a| a.get("path"))
                            .or_else(|| args.and_then(|a| a.get("file_path"))),
                    );
                }
                if name == "run_command" {
                    let program = text(args.and_then(|a| a.get("program")));
                    if program == "cargo" || program == "git" {
                        cargo.push(format!(
                            "t{} {program} {}",
                            text(object.get("turn")),
                            text(args.and_then(|a| a.get("args")))
                        ));
                    }
                }
                if name == "bash" {
                    let command = text(args.and_then(|a| a.get("command")));
                    if command.contains("cargo") {
                        cargo.push(format!(
                            "t{} {}",
                            text(object.get("turn")),
                            command.chars().take(140).collect::<String>()
                        ));
                    }
                }
            }
        }
        if kind == "tool_result" {
            let shown = text(
                object
                    .get("content_shown")
                    .or_else(|| object.get("full_content")),
            )
            .replace("\\n", "\n");
            for line in shown.lines() {
                if error.is_match(line) {
                    cargo.push(format!(
                        "  fail t{}: {}",
                        text(object.get("turn")),
                        line.trim().chars().take(160).collect::<String>()
                    ));
                }
            }
        }
    }
    if heading {
        println!(
            "## MVL  {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    println!(
        "- mvl completions: {completions}   finish: {finish}   first edit turn: {}",
        first_edit_turn.unwrap_or_else(|| "None".into())
    );
    if !usage.is_empty() {
        println!(
            "- usage: {}",
            usage
                .iter()
                .map(|(k, v)| format!("{k}={v:.0}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !tools.is_empty() {
        println!("- tool calls: {}", counts(&tools));
    }
    if !first_edit_path.is_empty() {
        println!("- first edit path: {first_edit_path}");
    }
    if !cargo.is_empty() {
        println!("- cargo / named failures:");
        for line in cargo.iter().take(40) {
            println!("  {line}");
        }
    }
    if !last_text.trim().is_empty() {
        println!(
            "- last completion: {}",
            last_text
                .trim()
                .replace('\n', " ")
                .chars()
                .take(280)
                .collect::<String>()
        );
    }
    Ok(())
}

fn pi(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut turns = 0;
    let mut tools = BTreeMap::new();
    let mut first_edit = None;
    let mut cargo = Vec::new();
    let mut last_text = String::new();
    let mut timeouts = 0;
    for object in records(path)? {
        let kind = object.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "turn_start" {
            turns += 1;
        }
        if kind == "tool_execution_start" {
            let name = text(object.get("toolName").or_else(|| object.get("name")));
            *tools
                .entry(if name.is_empty() {
                    "?".into()
                } else {
                    name.clone()
                })
                .or_insert(0) += 1;
            let args = object
                .get("args")
                .or_else(|| object.get("input"))
                .and_then(Value::as_object);
            if ["edit", "write"].contains(&name.as_str()) && first_edit.is_none() {
                first_edit = Some(format!(
                    "({}, {})",
                    turns,
                    text(args.and_then(|a| a.get("path")))
                ));
            }
            if name == "bash" {
                let command = text(args.and_then(|a| a.get("command")));
                if command.contains("cargo") {
                    cargo.push(format!(
                        "t{turns} {}",
                        command.chars().take(140).collect::<String>()
                    ));
                }
            }
        }
        if let Some(content) = (kind == "message_end"
            && object
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                == Some("assistant"))
        .then(|| object.get("message").and_then(|m| m.get("content")))
        .flatten()
        .and_then(Value::as_array)
        {
            last_text = content
                .iter()
                .filter_map(|c| {
                    (c.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| c.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("");
        }
        if object
            .to_string()
            .to_lowercase()
            .contains("connect timeout")
        {
            timeouts += 1;
        }
    }
    println!(
        "## pi  {}",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    println!("- turns: {turns}   tools: {}", counts(&tools));
    println!(
        "- first edit: {}",
        first_edit.unwrap_or_else(|| "None".into())
    );
    println!("- connect-timeout mentions: {timeouts}");
    if !cargo.is_empty() {
        println!("- cargo:");
        for line in cargo.iter().take(30) {
            println!("  {line}");
        }
    }
    if !last_text.is_empty() {
        println!(
            "- last assistant: {}",
            last_text
                .trim()
                .replace('\n', " ")
                .chars()
                .take(320)
                .collect::<String>()
        );
    }
    Ok(())
}

fn kind(path: &Path) -> &'static str {
    if path.is_dir() {
        if path.join("session.jsonl").is_file() || path.join("run.mvl.jsonl").is_file() {
            "outdir"
        } else if path.join("liberado").is_dir() || path.join("pi").is_dir() {
            "compare"
        } else if fs::read_dir(path)
            .map(|r| {
                r.flatten()
                    .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            })
            .unwrap_or(false)
        {
            "liberado-dir"
        } else {
            "dir"
        }
    } else if path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .ends_with(".mvl.jsonl")
    {
        "mvl"
    } else if path.file_name().and_then(|n| n.to_str()) == Some("session.jsonl") {
        "pi"
    } else if path.extension().and_then(|x| x.to_str()) == Some("json") {
        "liberado-json"
    } else if path.extension().and_then(|x| x.to_str()) == Some("jsonl") {
        "jsonl"
    } else {
        "unknown"
    }
}

fn walk(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match kind(path) {
        "liberado-json" => liberado(path),
        "mvl" => mvl(path, true),
        "pi" => pi(path),
        "liberado-dir" => {
            for entry in fs::read_dir(path)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
            {
                liberado(&entry)?;
                println!();
            }
            Ok(())
        }
        "outdir" | "compare" | "dir" => {
            for candidate in [
                "liberado/traces",
                "traces",
                "pi/session.jsonl",
                "session.jsonl",
                "deepagents/run.mvl.jsonl",
                "run.mvl.jsonl",
            ]
            .iter()
            .map(|r| path.join(r))
            {
                if candidate.exists() {
                    println!(
                        "# {}",
                        if candidate.to_string_lossy().contains("pi") {
                            "pi"
                        } else if candidate.to_string_lossy().contains("deepagents")
                            || candidate.to_string_lossy().ends_with("run.mvl.jsonl")
                        {
                            "deepagents"
                        } else {
                            "liberado traces"
                        }
                    );
                    walk(&candidate)?;
                    println!();
                }
            }
            Ok(())
        }
        _ => Err(format!("unrecognized: {}", path.display()).into()),
    }
}

fn git_stat(workspace: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let path = workspace.to_string_lossy().to_string();
    let status = std_command("git")
        .args(["-C", &path, "status", "-sb"])
        .stdout(Stdio::piped())
        .output()?;
    let diff = std_command("git")
        .args(["-C", &path, "diff", "--stat"])
        .stdout(Stdio::piped())
        .output()?;
    println!("## git  {}", workspace.display());
    print!("{}", String::from_utf8_lossy(&status.stdout));
    print!("{}", String::from_utf8_lossy(&diff.stdout));
    Ok(())
}

pub fn run(args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args;
    let path = PathBuf::from(
        args.next()
            .ok_or("usage: liberado coder summarize <path> [--git <workspace>]")?,
    );
    let mut git = None;
    while let Some(arg) = args.next() {
        if arg == "--git" {
            git = Some(PathBuf::from(args.next().ok_or("--git requires a path")?));
        } else {
            return Err(format!("unknown summarize flag: {arg}").into());
        }
    }
    if !path.exists() {
        return Err(format!("not found: {}", path.display()).into());
    }
    walk(&path)?;
    if let Some(workspace) = git {
        println!();
        git_stat(&workspace)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reads_valid_jsonl_and_skips_malformed_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("run.mvl.jsonl");
        fs::write(&path, "{\"type\":\"completion\"}\nnot-json\n").unwrap();
        let values = records(&path).unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["type"], "completion");
        assert_eq!(kind(&path), "mvl");
    }

    #[test]
    fn detects_compare_layouts() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("liberado")).unwrap();
        assert_eq!(kind(dir.path()), "compare");
    }
}
