use super::*;
use std::ffi::OsStr;

#[test]
fn env_override_wins_over_memory_and_cpus() {
    let budget = from_inputs(Some("1"), Some(32_000), 16);
    assert_eq!(budget.jobs, 1);
    assert!(budget.reason.contains(JOBS_ENV));
}

#[test]
fn sixteen_gig_host_with_little_free_ram_gets_one_job() {
    let budget = from_inputs(None, Some(6_000), 4);
    assert_eq!(budget.jobs, 1);
    assert!(budget.reason.contains("6000 MiB available"));
}

#[test]
fn empty_runner_with_plenty_of_ram_keeps_more_than_one_job() {
    let budget = from_inputs(None, Some(14_000), 4);
    assert_eq!(budget.jobs, 2);
}

#[test]
fn jobs_never_exceed_the_cpu_count() {
    assert_eq!(from_inputs(None, Some(64_000), 3).jobs, 3);
}

#[test]
fn unknown_memory_caps_below_the_cpu_count() {
    assert_eq!(from_inputs(None, None, 8).jobs, 2);
}

#[test]
fn zero_or_invalid_env_is_ignored() {
    assert_eq!(parse_jobs("0"), None);
    assert_eq!(parse_jobs("nope"), None);
    assert_eq!(from_inputs(Some("0"), Some(6_000), 4).jobs, 1);
}

#[test]
fn meminfo_reads_available_kibibytes() {
    let text = "MemTotal:       16373764 kB\nMemAvailable:    6234512 kB\n";
    assert_eq!(parse_meminfo_available_mib(text), Some(6_088));
}

#[test]
fn only_compile_heavy_cargo_verbs_take_the_job_limit() {
    assert!(needs_compile_job_limit(OsStr::new("cargo"), &["clippy"]));
    assert!(needs_compile_job_limit(OsStr::new("cargo"), &["test"]));
    assert!(needs_compile_job_limit(OsStr::new("cargo"), &["llvm-cov"]));
    assert!(!needs_compile_job_limit(OsStr::new("cargo"), &["fmt"]));
    assert!(!needs_compile_job_limit(OsStr::new("cargo"), &["deny"]));
    assert!(!needs_compile_job_limit(OsStr::new("git"), &["clippy"]));
}
