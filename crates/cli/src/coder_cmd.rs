//! `liberado coder …` — harness observability over durable coding traces.
//!
//! Thin adapter: resolve path → call pure functions in `liberado_coder_core::trace_view` → print.
//! Domain logic stays in the pack contract crate so unit tests drive the same path the binary uses.

use std::path::{Path, PathBuf};

use liberado_coder_core::{
    ForeignTraceFormat, compare_traces, format_comparison, import_foreign_file, load_trace,
    render_transcript, resolve_trace_path, write_messages_export,
};

/// Default directories searched when resolving a session id (cwd-relative + common local path).
fn default_trace_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("coder-traces")];
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("coder-traces"));
    }
    dirs
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.next().as_deref() {
        Some("trace") => cmd_trace(&mut args),
        Some("compare") => cmd_compare(&mut args),
        Some("import") => cmd_import(&mut args),
        Some(other) => Err(format!("unknown coder subcommand '{other}'\n{}", usage()).into()),
        None => Err(usage().into()),
    }
}

fn usage() -> &'static str {
    "usage:\n  \
     liberado coder trace <session-id|path> [--dir <trace-dir>] [--path <file>]\n  \
     liberado coder compare <trace-a> <trace-b> [--dir <trace-dir>] [--json]\n  \
     liberado coder import <foreign.json> [-o <out.messages.json>] [--format kilo|openhands|auto] [--session-id <id>]"
}

fn cmd_trace(args: &mut dyn Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut id_or_path: Option<String> = None;
    let mut explicit_path: Option<PathBuf> = None;
    let mut dirs: Vec<PathBuf> = default_trace_dirs();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--path" => {
                let p = args.next().ok_or("--path requires a value")?;
                explicit_path = Some(PathBuf::from(p));
            }
            "--dir" => {
                let d = args.next().ok_or("--dir requires a value")?;
                dirs.insert(0, PathBuf::from(d));
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag for coder trace: {other}").into());
            }
            other => {
                if id_or_path.is_some() {
                    return Err("coder trace takes a single session-id or path".into());
                }
                id_or_path = Some(other.to_string());
            }
        }
    }

    let path = if let Some(p) = explicit_path {
        p
    } else {
        let id = id_or_path.ok_or("usage: liberado coder trace <session-id|path>")?;
        let dir_refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
        resolve_trace_path(&id, &dir_refs)?
    };

    let trace = load_trace(&path)?;
    print!("{}", render_transcript(&trace));
    Ok(())
}

fn cmd_compare(args: &mut dyn Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut paths: Vec<String> = Vec::new();
    let mut dirs: Vec<PathBuf> = default_trace_dirs();
    let mut as_json = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => {
                let d = args.next().ok_or("--dir requires a value")?;
                dirs.insert(0, PathBuf::from(d));
            }
            "--json" => as_json = true,
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag for coder compare: {other}").into());
            }
            other => paths.push(other.to_string()),
        }
    }

    if paths.len() != 2 {
        return Err("usage: liberado coder compare <trace-a> <trace-b>".into());
    }

    let dir_refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    let path_a = resolve_trace_path(&paths[0], &dir_refs)?;
    let path_b = resolve_trace_path(&paths[1], &dir_refs)?;
    let a = load_trace(&path_a)?;
    let b = load_trace(&path_b)?;
    let comparison = compare_traces(&a, &b);

    if as_json {
        println!("{}", serde_json::to_string_pretty(&comparison)?);
    } else {
        print!("{}", format_comparison(&comparison));
    }
    Ok(())
}

fn cmd_import(args: &mut dyn Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut format: Option<ForeignTraceFormat> = None;
    let mut session_id: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-o" | "--output" => {
                let p = args.next().ok_or("-o requires a value")?;
                output = Some(PathBuf::from(p));
            }
            "--format" => {
                let f = args.next().ok_or("--format requires kilo|openhands|auto")?;
                format = match f.as_str() {
                    "kilo" => Some(ForeignTraceFormat::Kilo),
                    "openhands" | "oh" => Some(ForeignTraceFormat::OpenHands),
                    "auto" => None,
                    other => {
                        return Err(format!(
                            "unknown --format '{other}' (expected kilo|openhands|auto)"
                        )
                        .into());
                    }
                };
            }
            "--session-id" => {
                session_id = Some(args.next().ok_or("--session-id requires a value")?);
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag for coder import: {other}").into());
            }
            other => {
                if input.is_some() {
                    return Err("coder import takes a single input path".into());
                }
                input = Some(PathBuf::from(other));
            }
        }
    }

    let input =
        input.ok_or("usage: liberado coder import <foreign.json> [-o out.messages.json]")?;
    let (detected, export) = import_foreign_file(&input, format, session_id)?;

    let out_path = output.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("imported");
        PathBuf::from(format!("{stem}.messages.json"))
    });

    write_messages_export(&out_path, &export)?;
    eprintln!(
        "imported {:?} → {} ({} messages, session_id={})",
        detected,
        out_path.display(),
        export.messages.len(),
        export.session_id
    );
    // Also print the JSON so pipes/scripts can consume it without opening the file.
    println!("{}", serde_json::to_string_pretty(&export)?);
    Ok(())
}
