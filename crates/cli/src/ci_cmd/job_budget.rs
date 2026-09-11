//! Cap rustc jobs for `liberado ci` so a 16G host does not pin during workspace Clippy.

use std::ffi::OsStr;
use std::process::Command;

/// Leave this much free RAM for the OS, desktop, daemon, and Cargo itself.
const RESERVE_MIB: u64 = 4096;
/// One rustc/clippy-driver on this workspace often needs several GiB.
const JOB_MIB: u64 = 4096;
/// When RAM cannot be read, do not keep the full CPU count.
const UNKNOWN_MEMORY_CAP: usize = 2;
pub(crate) const JOBS_ENV: &str = "LIBERADO_CI_JOBS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobBudget {
    pub jobs: u32,
    pub reason: String,
}

impl JobBudget {
    pub(crate) fn detect() -> Self {
        from_inputs(
            std::env::var(JOBS_ENV).ok().as_deref(),
            available_memory_mib(),
            cpu_count(),
        )
    }

    pub(crate) fn summary(&self) -> String {
        format!("cargo jobs={} ({})", self.jobs, self.reason)
    }
}

pub(crate) fn from_inputs(
    env_jobs: Option<&str>,
    available_mib: Option<u64>,
    cpus: usize,
) -> JobBudget {
    if let Some(jobs) = env_jobs.and_then(parse_jobs) {
        return JobBudget {
            jobs,
            reason: format!("{JOBS_ENV}={jobs}"),
        };
    }
    let cpus = cpus.max(1);
    let Some(available_mib) = available_mib else {
        let jobs = cpus.min(UNKNOWN_MEMORY_CAP) as u32;
        return JobBudget {
            jobs,
            reason: format!("cpus={cpus}, memory unknown; cap {UNKNOWN_MEMORY_CAP}"),
        };
    };
    let from_memory = available_mib.saturating_sub(RESERVE_MIB) / JOB_MIB;
    let jobs = (from_memory as usize).clamp(1, cpus) as u32;
    JobBudget {
        jobs,
        reason: format!("{available_mib} MiB available, {cpus} cpus"),
    }
}

pub(crate) fn parse_jobs(value: &str) -> Option<u32> {
    value.parse().ok().filter(|jobs| *jobs >= 1)
}

pub(crate) fn parse_meminfo_available_mib(text: &str) -> Option<u64> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("MemAvailable:") {
            continue;
        }
        let kb: u64 = parts.next()?.parse().ok()?;
        return Some(kb / 1024);
    }
    None
}

pub(crate) fn needs_compile_job_limit(program: &OsStr, args: &[&str]) -> bool {
    program == "cargo"
        && matches!(
            args.first().copied(),
            Some("clippy" | "test" | "llvm-cov" | "check" | "build")
        )
}

pub(crate) fn apply_compile_job_limit(command: &mut Command, program: &OsStr, args: &[&str]) {
    if !needs_compile_job_limit(program, args) {
        return;
    }
    detach_inherited_jobserver(command);
    command.env("CARGO_BUILD_JOBS", JobBudget::detect().jobs.to_string());
}

fn detach_inherited_jobserver(command: &mut Command) {
    command.env_remove("CARGO_MAKEFLAGS");
    command.env_remove("MAKEFLAGS");
    command.env_remove("MFLAGS");
}

fn cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
}

fn available_memory_mib() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo_available_mib(&text)
}

#[cfg(test)]
#[path = "job_budget_tests.rs"]
mod tests;
