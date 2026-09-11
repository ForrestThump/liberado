//! Child-log and gate runner for `liberado ci`.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::{CI_LOG_FILE, EXTRACT_MAX_LINES, coverage_tools, job_budget};

/// One invocation's full child log. Truncated at the start of `liberado ci`.
pub(crate) struct CiLog {
    pub(crate) root: PathBuf,
    pub(crate) path: PathBuf,
}

impl CiLog {
    pub(crate) fn create(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        std::fs::create_dir_all(root.join(".liberado"))?;
        let path = root.join(CI_LOG_FILE);
        std::fs::write(
            &path,
            format!("# liberado ci — full log\n# {CI_LOG_FILE}\n"),
        )?;
        eprintln!("[liberado ci] full log: {CI_LOG_FILE}");
        let log = Self {
            root: root.to_path_buf(),
            path,
        };
        let budget = job_budget::JobBudget::detect();
        eprintln!("[liberado ci] {}", budget.summary());
        log.writeln(&budget.summary())?;
        Ok(log)
    }

    pub(crate) fn writeln(&self, line: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut file = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        writeln!(file, "{line}")?;
        Ok(())
    }
}

pub(crate) fn run_cmd(
    log: &CiLog,
    program: impl AsRef<OsStr>,
    args: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let program = program.as_ref();
    let gate = begin_cmd(log, program, args)?;
    finish_cmd(log, gate, coverage_tools::spawn_to_log(log, program, args))
}

struct GateStart {
    command: String,
    program: String,
    start: u64,
}

fn begin_cmd(
    log: &CiLog,
    program: &OsStr,
    args: &[&str],
) -> Result<GateStart, Box<dyn std::error::Error>> {
    let shown = program.to_string_lossy().into_owned();
    let command = format!("{shown} {}", args.join(" "));
    log.writeln(&format!("=== {command} ==="))?;
    eprint!("[liberado ci] {command} ... ");
    let _ = io::stderr().flush();
    Ok(GateStart {
        command,
        program: shown,
        start: std::fs::metadata(&log.path)?.len(),
    })
}

fn finish_cmd(
    log: &CiLog,
    gate: GateStart,
    spawned: io::Result<std::process::ExitStatus>,
) -> Result<(), Box<dyn std::error::Error>> {
    match spawned {
        Ok(status) if status.success() => {
            eprintln!("ok");
            Ok(())
        }
        Ok(status) => gate_failed(log, &gate, format!("{} failed with {status}", gate.command)),
        Err(error) => gate_failed(
            log,
            &gate,
            format!("could not start {}: {error}", gate.program),
        ),
    }
}

fn gate_failed(
    log: &CiLog,
    gate: &GateStart,
    reason: String,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("FAILED");
    log.writeln(&reason)?;
    let extracted = extract_ci_failures(&read_log_since(&log.path, gate.start)?);
    if !extracted.is_empty() {
        eprintln!("\n{extracted}\n");
    }
    eprintln!("----------\nFull log: {CI_LOG_FILE}\n----------");
    if extracted.is_empty() {
        return Err(format!("{reason}\nFull log: {CI_LOG_FILE}").into());
    }
    Err(format!("{reason}\nFull log: {CI_LOG_FILE}\n\n{extracted}").into())
}

fn read_log_since(path: &Path, start: u64) -> Result<String, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let skip = (start as usize).min(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[skip..]).into_owned())
}

/// Pull compiler, test, and CRAP failures out of a child log so the agent
/// does not have to scan compile progress or passing crates.
pub(crate) fn extract_ci_failures(output: &str) -> String {
    liberado_coder_core::extract_failures_capped(output, EXTRACT_MAX_LINES, Some(CI_LOG_FILE))
}
