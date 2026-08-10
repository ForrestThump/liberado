//! Build the workspace once, before the model is given anything to read.
//!
//! ## Two reasons, and the second is the expensive one
//!
//! **It proves the baseline compiles.** Two dispatched runs were written up as "the model wrote
//! code that does not build" before anyone checked whether the worktree built to begin with. It
//! did — the errors were the model's own line — but answering that took reading a trace, and it
//! is the first question anyone asks. A run that starts from a broken baseline should say so
//! instead of spending a budget discovering it.
//!
//! **It keeps the provider's prompt cache warm.** Send the system prompt, then have the model sit
//! through a cold multi-minute build before its next message, and the cached prefix has expired
//! by the time that message goes out — so the same tokens are billed twice. Doing the slow part
//! *before* the first token means every request in the run lands close together.
//!
//! ## Why this is affordable
//!
//! Only because of the shared target directory. Cold, a full build of this workspace is minutes
//! and doing it per run would be indefensible. Pointed at one cache, the first run pays and every
//! run after it gets an incremental check. The two settings are one feature; enabling the warm-up
//! without the cache just moves the cost earlier.
//!
//! ## What it does not do
//!
//! It does not run tests, and it does not gate on clippy or fmt. This answers one question —
//! *does the tree the model is about to edit compile?* — and a warm-up that fails on a pre-existing
//! style violation would refuse to start a run over something the task was never about.

use std::path::Path;
use std::time::{Duration, Instant};

/// Outcome of the pre-run build.
#[derive(Debug, Clone, PartialEq)]
pub enum Warmup {
    /// The baseline compiles. `seconds` is how long it took, which is the number worth watching:
    /// once the shared cache is warm this should fall to seconds.
    Ready { seconds: u64 },
    /// The baseline does not compile, and no tokens should be spent. `detail` is the compiler's
    /// own last words, trimmed — a summary would drop the file and line, which is the only part
    /// the reader needs.
    BaselineBroken { detail: String },
    /// The build did not finish in time. Treated as *not* a broken baseline: a slow machine is
    /// not a broken tree, and refusing to run would strand a deployment whose first build is
    /// simply longer than the ceiling.
    TimedOut { seconds: u64 },
    /// Not attempted — no cargo manifest, or the caller switched it off.
    Skipped,
}

impl Warmup {
    /// Whether the run may proceed. Only a broken baseline stops it.
    pub fn may_proceed(&self) -> bool {
        !matches!(self, Warmup::BaselineBroken { .. })
    }
}

/// Compile `workspace` before the model is called.
///
/// `cargo check --workspace --all-targets`, not `build` and not `test`: it catches the errors a
/// baseline can actually have, at a fraction of the time and disk, and it warms the same cache
/// the run's own checks will use.
pub async fn warm_workspace(
    workspace: &Path,
    env: &std::collections::BTreeMap<String, String>,
    timeout: Duration,
) -> Warmup {
    if !workspace.join("Cargo.toml").is_file() {
        return Warmup::Skipped;
    }
    let started = Instant::now();
    let mut cmd = liberado_common::process::command("cargo");
    cmd.current_dir(workspace)
        .args(["check", "--workspace", "--all-targets"]);
    for (key, value) in env {
        cmd.env(key, value);
    }

    match liberado_common::process::output_within(&mut cmd, "cargo check (warm-up)", timeout).await
    {
        Ok(output) if output.status.success() => Warmup::Ready {
            seconds: started.elapsed().as_secs(),
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // The last lines carry the error and its location. The head of a cargo run is a list
            // of crates that compiled fine, which tells the reader nothing.
            let detail = stderr
                .lines()
                .filter(|l| !l.trim_start().starts_with("Compiling"))
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            Warmup::BaselineBroken { detail }
        }
        Err(liberado_common::process::CommandError::TimedOut { .. }) => Warmup::TimedOut {
            seconds: started.elapsed().as_secs(),
        },
        Err(e) => {
            // Cargo missing, or unspawnable. Not the tree's fault, so the run continues and the
            // verifiers will say so later if it matters.
            tracing::warn!(error = %e, "warm-up build could not run; continuing without it");
            Warmup::Skipped
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn a_directory_with_no_manifest_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            warm_workspace(dir.path(), &BTreeMap::new(), Duration::from_secs(5)).await,
            Warmup::Skipped
        );
    }

    /// A broken baseline must stop the run before a token is spent. That is the entire point:
    /// discovering it from the model's report costs a whole budget.
    #[tokio::test]
    async fn a_workspace_that_does_not_compile_is_reported_and_blocks() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"broken\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("manifest");
        std::fs::create_dir_all(dir.path().join("src")).expect("src");
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f( -> u8 { 42 }\n").expect("lib");

        let result = warm_workspace(dir.path(), &BTreeMap::new(), Duration::from_secs(180)).await;
        match &result {
            Warmup::BaselineBroken { detail } => assert!(
                detail.contains("error"),
                "the compiler's own words must reach the reader: {detail}"
            ),
            other => panic!("a broken tree must be reported, got {other:?}"),
        }
        assert!(
            !result.may_proceed(),
            "a run must not start against a tree that does not compile"
        );
    }

    /// A tree that compiles reports Ready and lets the run start.
    #[tokio::test]
    async fn a_compiling_workspace_is_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fine\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("manifest");
        std::fs::create_dir_all(dir.path().join("src")).expect("src");
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f() -> u8 { 42 }\n").expect("lib");

        let result = warm_workspace(dir.path(), &BTreeMap::new(), Duration::from_secs(180)).await;
        assert!(
            matches!(result, Warmup::Ready { .. }),
            "a compiling tree must be Ready, got {result:?}"
        );
        assert!(result.may_proceed());
    }

    /// A timeout is not a verdict on the tree. Refusing to run because a first build is slower
    /// than the ceiling would strand a deployment for a reason that has nothing to do with it.
    #[tokio::test]
    async fn a_timeout_does_not_block_the_run() {
        assert!(Warmup::TimedOut { seconds: 1 }.may_proceed());
        assert!(Warmup::Skipped.may_proceed());
        assert!(Warmup::Ready { seconds: 1 }.may_proceed());
    }

    /// The env overlay must reach cargo, or the shared cache is configured and unused — the
    /// failure this whole feature exists to avoid, one layer down.
    #[tokio::test]
    async fn the_env_overlay_reaches_the_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("elsewhere");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"envcheck\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("manifest");
        std::fs::create_dir_all(dir.path().join("src")).expect("src");
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f() {}\n").expect("lib");

        let mut env = BTreeMap::new();
        env.insert(
            "CARGO_TARGET_DIR".to_string(),
            target.to_string_lossy().to_string(),
        );
        let result = warm_workspace(dir.path(), &env, Duration::from_secs(180)).await;
        assert!(matches!(result, Warmup::Ready { .. }), "{result:?}");
        assert!(
            target.is_dir(),
            "cargo did not use the target dir it was given, so the shared cache would be inert"
        );
        assert!(
            !dir.path().join("target").exists(),
            "the default target dir must stay unused when one is configured"
        );
    }
}
