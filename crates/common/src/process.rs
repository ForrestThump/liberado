//! Subprocess spawning that cannot inherit this process's stdin, and cannot hang forever.
//!
//! ## Why this exists
//!
//! `tokio::process::Command::output()` pipes stdout and stderr but leaves **stdin inherited**.
//! That is harmless when stdin is a terminal and catastrophic when it is a protocol wire: the
//! ACP bridge speaks JSON-RPC over stdin/stdout, so every `git` it spawned inherited the
//! editor's pipe and blocked reading traffic that was never meant for it. A Paseo coding prompt
//! sat for 19 minutes having never called a model, and the child never got as far as writing
//! its first `GIT_TRACE` line — the failure was completely mute.
//!
//! Measured in the failing process, at the failing moment:
//!
//! | spawn shape | result |
//! |---|---|
//! | `.output()` (stdin inherited) | hung, >10s, indefinitely |
//! | null stdio, `.status()` | ok, 11.5ms |
//! | **null stdin, same pipes** | **ok, 8.1ms** |
//!
//! The second failure mode is the same event seen from outside: nothing bounded the call, and
//! its result was discarded (`let _ = ...output().await`), so a 15ms failure presented as an
//! infinite spinner. Inheriting stdin is the bug; being unbounded is why nobody could tell.
//!
//! ## Using it
//!
//! [`command`] replaces `Command::new` everywhere a subprocess is launched, and
//! [`output_within`] replaces a bare `.output().await` wherever a hang would strand a caller.
//! `crates/test-support/tests/subprocess_rules.rs` fails the build if a new `Command::new`
//! appears in the spawning crates, so this cannot quietly regrow.

use std::ffi::OsStr;
use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::process::Command;

/// Default ceiling for a subprocess that is expected to be quick (git plumbing, `--version`).
///
/// Deliberately generous — a healthy `git worktree prune` on a developer box is ~15ms, so this
/// is three orders of magnitude of headroom. It exists to convert "hangs forever, silently"
/// into "fails in half a minute, loudly", not to police slow machines.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// A [`Command`] that can never inherit this process's stdin.
///
/// Use this instead of `tokio::process::Command::new`. Stdout and stderr are left alone: callers
/// legitimately want them piped, inherited, or null depending on the job, and unlike stdin
/// neither can deadlock the child against its parent's protocol wire.
///
/// `Stdio::null()` rather than an empty pipe on purpose — a child reading a null stdin gets EOF
/// immediately, whereas an empty pipe the parent forgets to close reproduces the original bug
/// with extra steps.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    cmd
}

/// The blocking counterpart of [`command`], for the call sites that are not async.
///
/// Same rule, same reason: a synchronous `git` in a checkpoint or a preflight baseline inherits
/// stdin just as eagerly as an async one, and the parent is just as likely to be holding a
/// protocol wire. There is no blocking equivalent of [`output_within`] — bounding a blocking
/// call means a watchdog thread, and every site that needs a deadline should be async instead.
pub fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.stdin(Stdio::null());
    cmd
}

/// A subprocess that did not finish inside its deadline.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("spawning `{program}` failed: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// The child outlived its deadline and was killed.
    #[error("`{program}` did not finish within {timeout:?} and was killed")]
    TimedOut { program: String, timeout: Duration },
}

/// Run `cmd` to completion, killing it if it outlives `timeout`.
///
/// The kill matters as much as the timeout. Abandoning the future without killing leaves an
/// orphan holding the pipes — exactly what the original hang produced: git processes still
/// alive twenty minutes later, one per attempt, invisible to the surface that spawned them.
///
/// **Why this is hand-rolled rather than `timeout(t, cmd.output())`.** That shorter form does
/// not kill the child, even with `kill_on_drop(true)`: measured, a 500ms deadline returned to
/// the caller on time while the child ran its full 30 seconds and the runtime blocked on
/// shutdown waiting for it. The caller looked correct and the process leaked. So the child is
/// spawned here, and killed explicitly on the timeout branch.
///
/// stdout and stderr are drained concurrently with the wait — reading them afterwards would
/// deadlock any child that fills a pipe buffer before exiting.
///
/// **Known limit: this kills the child, not its descendants.** On Windows `TerminateProcess`
/// does not touch grandchildren, so killing a wrapper leaves whatever it launched holding the
/// pipes — and `C:\Program Files\Git\cmd\git.exe` is exactly such a wrapper, re-execing the
/// real git. The caller is still freed on time, which is the property that matters here, but a
/// grandchild can outlive it. Containing that properly needs a Windows job object with
/// `KILL_ON_JOB_CLOSE`; it is deliberately not attempted here, because a half-done process-tree
/// kill is worse than a documented one.
///
/// `program` is taken separately because [`Command`] exposes no stable accessor for it, and an
/// error that cannot name the process it is about is most of the way to being useless.
pub async fn output_within(
    cmd: &mut Command,
    program: &str,
    timeout: Duration,
) -> Result<Output, CommandError> {
    use tokio::io::AsyncReadExt;

    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|source| CommandError::Spawn {
        program: program.to_string(),
        source,
    })?;

    let mut child_stdout = child.stdout.take();
    let mut child_stderr = child.stderr.take();
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();

    let finished = tokio::time::timeout(timeout, async {
        let read_out = async {
            match child_stdout.as_mut() {
                Some(s) => s.read_to_end(&mut out_buf).await.map(|_| ()),
                None => Ok(()),
            }
        };
        let read_err = async {
            match child_stderr.as_mut() {
                Some(s) => s.read_to_end(&mut err_buf).await.map(|_| ()),
                None => Ok(()),
            }
        };
        let (out, err, status) = tokio::join!(read_out, read_err, child.wait());
        out?;
        err?;
        status
    })
    .await;

    match finished {
        Ok(Ok(status)) => Ok(Output {
            status,
            stdout: out_buf,
            stderr: err_buf,
        }),
        Ok(Err(source)) => Err(CommandError::Spawn {
            program: program.to_string(),
            source,
        }),
        Err(_) => {
            // Kill, then reap. Without the `wait` the process lingers as a zombie on unix and
            // the handle stays open on Windows, so "we timed out" would still leak.
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(CommandError::TimedOut {
                program: program.to_string(),
                timeout,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stdin-consuming child reaches EOF and exits instead of waiting for input.
    ///
    /// **What this cannot prove.** `cargo test` usually hands the harness a null stdin already,
    /// so an inherited stdin would look the same here — this test would pass against a helper
    /// that set nothing. It is a smoke check that the helper does not *break* a child, not
    /// evidence that inheritance is prevented. The guard that actually holds the invariant is
    /// `crates/test-support/tests/subprocess_rules.rs`, which fails the build when a raw
    /// `Command::new` appears in a spawning crate; the real-world proof is the ACP repro in
    /// `scripts/repro-acp-prompt.js`, where the same spawn hangs without this and returns in
    /// 8ms with it.
    ///
    /// Recorded rather than deleted because the next person will otherwise write it again and
    /// believe it.
    #[tokio::test]
    async fn a_stdin_consuming_child_exits_instead_of_waiting() {
        // Echo stdin verbatim: `findstr "^"` matches every line, `cat` copies. Both exit at EOF
        // and emit nothing for empty input — unlike `more`, which prints a bare CRLF and would
        // make an "output is empty" assertion fail for reasons unrelated to stdin.
        #[cfg(windows)]
        let mut cmd = {
            let mut c = command("cmd");
            c.args(["/c", "findstr", "\"^\""]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = command("cat");

        let out = output_within(&mut cmd, "stdin-probe", Duration::from_secs(10))
            .await
            .expect("a null stdin must give the child immediate EOF, not a hang");
        assert!(
            out.stdout.is_empty(),
            "child emitted stdin bytes it was never given: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    /// A hung child must be killed and reported, not waited on forever.
    #[tokio::test]
    async fn a_child_that_overruns_is_killed_and_named() {
        #[cfg(windows)]
        let mut cmd = {
            // `ping` directly, NOT via `cmd /c`: `timeout /t` refuses a redirected stdin, and
            // routing through a shell makes the sleeper a *grandchild*, which the kill does not
            // reach (see `output_within`). The first version of this test did exactly that and
            // took 29s — passing its own assertion while the child ran to completion, which is
            // the bug it was written to catch.
            let mut c = command("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = command("sleep");
            c.arg("30");
            c
        };

        let started = std::time::Instant::now();
        let err = output_within(&mut cmd, "slow-probe", Duration::from_millis(500))
            .await
            .expect_err("a child that outlives its deadline must be an error, not a wait");
        assert!(
            matches!(err, CommandError::TimedOut { .. }),
            "expected a timeout, got: {err}"
        );
        assert!(
            err.to_string().contains("slow-probe"),
            "the error must name the program: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline was not enforced; took {:?}",
            started.elapsed()
        );
    }

    /// A spawn failure must stay distinguishable from a timeout — they need different fixes.
    #[tokio::test]
    async fn a_missing_program_is_a_spawn_error_not_a_timeout() {
        let mut cmd = command("liberado-no-such-program-hopefully");
        let err = output_within(&mut cmd, "missing", Duration::from_secs(5))
            .await
            .expect_err("a missing binary cannot succeed");
        assert!(
            matches!(err, CommandError::Spawn { .. }),
            "expected a spawn error, got: {err}"
        );
    }
}
