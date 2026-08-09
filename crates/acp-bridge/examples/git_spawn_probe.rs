//! Isolate *which* spawn shape hangs when a Rust process launches git under a piped parent.
//!
//! A Paseo coding prompt wedged for 19 minutes on `git worktree prune`. Established so far:
//! the bridge's await machinery is fine (killing the child let `.output()` return), the launch
//! tree is innocent (node -> cmd.exe -> git completes in ~30ms), both git binaries on this box
//! hang identically, and the hung git writes **no** `GIT_TRACE` line at all -- not even
//! `exec-cmd.c: resolved executable dir`, which is git's first. So it blocks before git's own
//! code runs, in process startup.
//!
//! This probe varies one thing at a time inside a Rust process, so the answer is a row in a
//! table rather than a theory. Run it two ways -- from a console, and from
//! `scripts/repro-acp-prompt.js`'s piped-stdio parent -- and compare.
//!
//! ```text
//! cargo run -p liberado-acp-bridge --example git_spawn_probe -- <repo>
//! node -e "require('child_process').spawn(process.argv[1],[process.argv[2]],{stdio:'pipe'})
//!          .stderr.pipe(process.stderr)" <probe.exe> <repo>
//! ```

use std::process::Stdio;
use std::time::{Duration, Instant};

/// Windows creation flags worth testing. `CREATE_NO_WINDOW` is the usual fix when a child
/// blocks on console setup it should never have inherited.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

const PER_CASE_TIMEOUT: Duration = Duration::from_secs(12);

#[tokio::main]
async fn main() {
    let repo = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".".to_string());

    eprintln!("probe pid {}", std::process::id());
    eprintln!("repo     {repo}");
    eprintln!(
        "stdout is a terminal: {}",
        atty_like(std::io::stdout().lock())
    );
    eprintln!();

    run("tokio .output() (what the bridge does)", || {
        let mut c = tokio::process::Command::new("git");
        c.args(["-C", &repo, "worktree", "prune"]);
        c
    })
    .await;

    run("tokio .output() + explicit null stdio", || {
        let mut c = tokio::process::Command::new("git");
        c.args(["-C", &repo, "worktree", "prune"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    })
    .await;

    #[cfg(windows)]
    run("tokio .output() + CREATE_NO_WINDOW", || {
        let mut c = tokio::process::Command::new("git");
        c.args(["-C", &repo, "worktree", "prune"])
            .creation_flags(CREATE_NO_WINDOW);
        c
    })
    .await;

    #[cfg(windows)]
    run("tokio .output() + DETACHED_PROCESS", || {
        let mut c = tokio::process::Command::new("git");
        c.args(["-C", &repo, "worktree", "prune"])
            .creation_flags(DETACHED_PROCESS);
        c
    })
    .await;

    // std, on a blocking thread: separates "tokio's process driver" from "spawning at all".
    let repo2 = repo.clone();
    let started = Instant::now();
    let handle = std::thread::spawn(move || {
        std::process::Command::new("git")
            .args(["-C", &repo2, "worktree", "prune"])
            .output()
            .map(|o| o.status.success())
    });
    let mut waited = Duration::ZERO;
    let label = "std::process .output() on a thread";
    loop {
        if handle.is_finished() {
            let r = handle.join();
            eprintln!("  {label:<44} ok       {:?} ({r:?})", started.elapsed());
            break;
        }
        if waited >= PER_CASE_TIMEOUT {
            eprintln!("  {label:<44} HUNG     >{:?}", PER_CASE_TIMEOUT);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        waited += Duration::from_millis(100);
    }

    eprintln!("\nA row marked HUNG under a piped parent but ok from a console is the bug.");
}

// Note: no `std::os::windows::process::CommandExt` import — `tokio::process::Command` has its
// own inherent `creation_flags`, and importing the std trait shadows nothing but warns.

/// Run one case under a timeout so a hang costs 12 seconds instead of the run.
async fn run<F>(label: &str, build: F)
where
    F: FnOnce() -> tokio::process::Command,
{
    let started = Instant::now();
    let mut cmd = build();
    match tokio::time::timeout(PER_CASE_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => eprintln!(
            "  {label:<44} ok       {:?} (status {})",
            started.elapsed(),
            out.status
        ),
        Ok(Err(e)) => eprintln!("  {label:<44} ERR      {e}"),
        Err(_) => eprintln!("  {label:<44} HUNG     >{PER_CASE_TIMEOUT:?}"),
    }
}

/// Crude "is this a console" check without pulling in a dependency: on Windows a console
/// handle answers `GetConsoleMode`, a pipe does not. Only used for the report header.
fn atty_like<T>(_t: T) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        unsafe extern "system" {
            fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
        }
        let mut mode = 0u32;
        let h = std::io::stdout().as_raw_handle();
        unsafe { GetConsoleMode(h.cast(), &mut mode) != 0 }
    }
    #[cfg(not(windows))]
    {
        false
    }
}
