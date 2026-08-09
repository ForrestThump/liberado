//! Diagnostic: does spawning `git` hang *from inside this process*, and if so, from when?
//!
//! Off unless `LIBERADO_ACP_SPAWN_PROBE=1`. Kept because the bug it chases was invisible every
//! other way: an out-of-process probe with the identical spawn shape
//! (`crates/acp-bridge/examples/git_spawn_probe.rs`) passes every variant in ~15ms, while the
//! same call from the live bridge blocks forever without git writing even its first `GIT_TRACE`
//! line. The difference is therefore *this process at that moment*, which is only observable
//! from inside it.
//!
//! Two call sites, so a hang localises rather than merely reproducing:
//!   * `main`, before the runtime does anything else -> a hang here means "always broken"
//!   * `session/prompt`, immediately before the coding run -> a hang only here means something
//!     between startup and the prompt causes it, and that is the thing to bisect.

use std::path::Path;
use std::time::{Duration, Instant};

/// How long one probe spawn may take before it is called hung. Generous: a healthy spawn on
/// this box is ~15ms, so anything near this bound is the pathology, not a slow disk.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether the probe is armed. Read per call rather than cached — a probe you cannot turn on
/// while reproducing is worse than none.
pub fn enabled() -> bool {
    std::env::var("LIBERADO_ACP_SPAWN_PROBE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Spawn a trivial `git` in `repo` and report how long it took, to stderr.
///
/// stderr, not `tracing`: the bridge owns stdout for JSON-RPC, and a probe that needs a
/// subscriber configured is a probe that reports nothing on the run where it matters.
pub async fn probe(label: &str, repo: &Path) {
    if !enabled() {
        return;
    }
    let repo_arg = repo.to_string_lossy().to_string();
    eprintln!("[spawn-probe] {label}:");

    // `--version` touches no repo state, so the probe cannot itself perturb the run it is
    // diagnosing. If even this hangs, the problem is process creation, not git's work.
    //
    // Three shapes, because they fail differently and the difference *is* the diagnosis:
    //   piped   — what the bridge does (`.output()` reads stdout+stderr through pipes)
    //   null    — no pipes at all; isolates process creation from pipe reading
    //   status  — pipes inherited rather than created, waits only on exit
    let piped = {
        let mut c = tokio::process::Command::new("git");
        c.args(["-C", &repo_arg, "--version"]);
        time_it("piped .output()", async move { c.output().await.map(|o| o.status) }).await
    };
    let null = {
        let mut c = tokio::process::Command::new("git");
        c.args(["-C", &repo_arg, "--version"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        time_it("null stdio .status()", async move { c.status().await }).await
    };

    // Same pipes as `.output()`, but stdin explicitly null. `.output()` leaves stdin to the
    // default, and if that default is a pipe the parent never closes, a child that touches
    // stdin blocks — indistinguishable from "git is hung" without this row.
    let null_stdin = {
        let mut c = tokio::process::Command::new("git");
        c.args(["-C", &repo_arg, "--version"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        time_it("null stdin + pipes", async move {
            c.output().await.map(|o| o.status)
        })
        .await
    };
    if piped.is_none() && null_stdin.is_some() {
        eprintln!(
            "[spawn-probe]   => the child's INHERITED STDIN is the blocker; null stdin fixes it"
        );
    }

    if piped.is_none() && null.is_some() {
        eprintln!(
            "[spawn-probe]   => process creation is FINE; the hang is in reading the child's pipes"
        );
    } else if piped.is_none() && null.is_none() {
        eprintln!("[spawn-probe]   => process creation itself hangs, regardless of stdio");
    }
}

/// Await `fut` under [`PROBE_TIMEOUT`], reporting the outcome. `None` means it hung.
async fn time_it<F>(what: &str, fut: F) -> Option<std::process::ExitStatus>
where
    F: std::future::Future<Output = std::io::Result<std::process::ExitStatus>>,
{
    let started = Instant::now();
    match tokio::time::timeout(PROBE_TIMEOUT, fut).await {
        Ok(Ok(status)) => {
            eprintln!("[spawn-probe]   {what:<22} OK   {:?} ({status})", started.elapsed());
            Some(status)
        }
        Ok(Err(e)) => {
            eprintln!("[spawn-probe]   {what:<22} ERR  {e}");
            None
        }
        Err(_) => {
            eprintln!("[spawn-probe]   {what:<22} HUNG >{PROBE_TIMEOUT:?}");
            None
        }
    }
}
