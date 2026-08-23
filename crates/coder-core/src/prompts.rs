//! Harness prompts, editable without a rebuild.
//!
//! ## The problem
//!
//! Every prompt the coding harness uses lived in Rust: a 900-character string literal inside
//! `coder-runner`'s `run_headless`, a `const` for the diff reviewer, a function returning a
//! literal for the session critic. Tuning any of them meant a compile, and on this workspace a
//! compile is minutes. Prompt work is iterative by nature — the session critic went from missing
//! two of four labelled traces to four of four on one wording change — so the loop that most
//! wants to be fast was the slowest one available.
//!
//! Worse, two of those literals had drifted from `prompts/coder/coder.md`, which already existed
//! and already claimed to be the coder's prompt. Nobody could tell which text a given run used.
//!
//! ## How it works
//!
//! Each prompt has exactly one source of truth: a file under `prompts/coder/`. That file is
//! **baked in at compile time** with `include_str!` *and* **read from disk at run time** when it
//! is there.
//!
//! - In a checkout, the file on disk wins. Edit it, run again, no rebuild.
//! - In a container that ships only the binary, the baked copy is used and nothing breaks.
//!
//! Because both come from the same file, they cannot disagree about what the default is — the
//! baked copy is just an older snapshot of the same text, and only when the file is absent.
//!
//! ## Precedence
//!
//! A role's explicit `prompt` or `prompt_path` still outranks everything here; those are how a
//! deployment overrides one role. This module supplies the default that used to be a literal.

use std::path::{Path, PathBuf};

/// The coding worker's instructions.
pub const CODER: &str = include_str!("../../../prompts/coder/coder.md");
/// The cold reviewer that sees the diff and nothing else (completion-gate / attempt critic).
pub const DIFF_REVIEWER: &str = include_str!("../../../prompts/coder/diff-reviewer.md");
/// Product cold-PR stage (backlog 0.8): severity findings with code citations; no author context.
pub const COLD_PR_REVIEWER: &str = include_str!("../../../prompts/coder/cold-pr-reviewer.md");
/// The reviewer that reads a finished run's own narration.
pub const SESSION_CRITIC: &str = include_str!("../../../prompts/coder/session-critic.md");
/// The coding worker as the daemon session pack configures it, with self-host git rules.
pub const SESSION_PACK_CODER: &str = include_str!("../../../prompts/coder/session-pack-coder.md");
/// The criteria-intake planner that turns a rough writeup into an acceptance contract.
pub const INTAKE: &str = include_str!("../../../prompts/coder/intake.md");
/// Interactive ACP coding: conversation + tools, no `submit_report`.
pub const INTERACTIVE: &str = include_str!("../../../prompts/coder/interactive.md");

/// Where prompt files live relative to a checkout root.
pub const PROMPT_DIR: &str = "prompts/coder";

/// File name for each prompt, so the on-disk copy and the baked copy stay paired.
pub const CODER_FILE: &str = "coder.md";
pub const DIFF_REVIEWER_FILE: &str = "diff-reviewer.md";
pub const COLD_PR_REVIEWER_FILE: &str = "cold-pr-reviewer.md";
pub const SESSION_CRITIC_FILE: &str = "session-critic.md";
pub const SESSION_PACK_CODER_FILE: &str = "session-pack-coder.md";
pub const INTAKE_FILE: &str = "intake.md";
pub const INTERACTIVE_FILE: &str = "interactive.md";

/// Where to look for prompt files for a run on `workspace_root`.
///
/// An explicit `[coder] prompt_dir` wins. Otherwise it is `prompts/coder` **inside the workspace
/// the run is operating on** — not relative to the process's current directory.
///
/// That distinction is not pedantry. A coding run happens in a git worktree of this repo, so the
/// prompts are right there beside the code; the process's cwd, meanwhile, is whatever launched
/// the binary, which for the headless runner is arbitrary and for a `cargo test` is the crate
/// directory. Keying on cwd made the override work in exactly one situation and silently fall
/// back to the baked copy everywhere else — including inside the worktrees where a run would
/// most want the checkout's own prompts.
pub fn dir_for(configured: Option<&str>, workspace_root: &str) -> PathBuf {
    match configured {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(workspace_root).join(PROMPT_DIR),
    }
}

/// Load `file` from disk, falling back to `baked`.
///
/// Search order:
/// 1. `dir/file`, when a `[coder] prompt_dir` is configured.
/// 2. `prompts/coder/file` under the current directory — the ordinary checkout case.
/// 3. `baked`, the copy compiled in from that same file.
///
/// A file that exists but cannot be read is a **warning, not an error**. The alternative is
/// failing a coding run over a permissions problem on an optional override, which trades a
/// slightly-wrong prompt for no run at all.
///
/// An empty or whitespace-only file is treated as absent. Truncating a prompt to zero bytes is
/// far more likely to be an accident — an interrupted write, a bad mount — than an instruction
/// to run the model with no instructions.
pub fn load(dir: Option<&Path>, file: &str, baked: &'static str) -> String {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = dir {
        candidates.push(dir.join(file));
    }
    candidates.push(Path::new(PROMPT_DIR).join(file));

    for path in candidates {
        if let Some(text) = read_prompt_candidate(&path) {
            return text;
        }
    }
    baked.to_string()
}

/// Read one prompt candidate, logging the outcome. `None` means "keep looking": the file is
/// missing, unreadable, or empty, and the next candidate (or the baked copy) should be used.
fn read_prompt_candidate(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => {
            tracing::debug!(path = %path.display(), "loaded prompt from disk");
            Some(text)
        }
        Ok(_) => {
            tracing::warn!(
                path = %path.display(),
                "prompt file is empty; using the built-in copy"
            );
            None
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "prompt file exists but could not be read; using the built-in copy"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The baked copies must not be empty. `include_str!` of a path that resolves to an empty
    /// file compiles happily and produces a model call with no instructions.
    #[test]
    fn every_baked_prompt_has_content() {
        for (name, text) in [
            ("coder", CODER),
            ("diff-reviewer", DIFF_REVIEWER),
            ("cold-pr-reviewer", COLD_PR_REVIEWER),
            ("session-critic", SESSION_CRITIC),
            ("session-pack-coder", SESSION_PACK_CODER),
            ("intake", INTAKE),
            ("interactive", INTERACTIVE),
        ] {
            assert!(
                text.trim().len() > 200,
                "{name} baked prompt is {} chars; a prompt this short is a build accident",
                text.trim().len()
            );
        }
    }

    /// Every baked prompt must come from a file that is still there, or the on-disk override
    /// silently stops working while the binary keeps using a snapshot nobody can find.
    #[test]
    fn every_baked_prompt_has_a_file_on_disk() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("repo root");
        for file in [
            CODER_FILE,
            DIFF_REVIEWER_FILE,
            COLD_PR_REVIEWER_FILE,
            SESSION_CRITIC_FILE,
            SESSION_PACK_CODER_FILE,
            INTAKE_FILE,
            INTERACTIVE_FILE,
        ] {
            let path = root.join(PROMPT_DIR).join(file);
            assert!(
                path.is_file(),
                "{} is baked in but missing from disk; the override path is dead",
                path.display()
            );
        }
    }

    /// Interactive ACP coding must not tell the model to `submit_report`: that tool is the
    /// one-shot pack's terminator, and offering the instruction without the tool is how a
    /// conversation tries to file a report it cannot file.
    #[test]
    fn interactive_prompt_does_not_offer_submit_report() {
        let text = INTERACTIVE.to_ascii_lowercase();
        assert!(
            text.contains("do **not** have `submit_report`")
                || text.contains("do not have `submit_report`")
                || text.contains("you do **not** have `submit_report`"),
            "interactive.md must tell the model it has no submit_report; got {} chars",
            INTERACTIVE.len()
        );
        assert!(
            !INTERACTIVE.contains("then submit_report"),
            "must not instruct the model to call submit_report"
        );
        assert!(
            text.contains("`done`"),
            "interactive.md must tell the model about `done` when the project configured checks"
        );
    }

    #[test]
    fn a_file_on_disk_wins_over_the_baked_copy() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("coder.md"), "OVERRIDDEN").expect("write");
        assert_eq!(load(Some(dir.path()), "coder.md", CODER), "OVERRIDDEN");
    }

    #[test]
    fn an_absent_file_falls_back_to_the_baked_copy() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(load(Some(dir.path()), "coder.md", CODER), CODER);
    }

    /// An empty prompt file is an accident, not an instruction to run with no prompt.
    #[test]
    fn an_empty_file_falls_back_rather_than_blanking_the_prompt() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("coder.md"), "   \n\n").expect("write");
        assert_eq!(load(Some(dir.path()), "coder.md", CODER), CODER);
    }

    /// The rule the whole module exists for: editing the file must change what a run sees,
    /// without a rebuild. The baked copy is a snapshot of the same text, so if the on-disk read
    /// ever silently stopped working this would be the test that noticed.
    #[test]
    fn editing_the_file_changes_the_prompt_without_touching_the_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session-critic.md");
        std::fs::write(&path, "first wording").expect("write");
        assert_eq!(
            load(Some(dir.path()), "session-critic.md", SESSION_CRITIC),
            "first wording"
        );

        std::fs::write(&path, "second wording").expect("rewrite");
        assert_eq!(
            load(Some(dir.path()), "session-critic.md", SESSION_CRITIC),
            "second wording",
            "a prompt change must take effect on the next run, not the next build"
        );
    }
}

#[cfg(test)]
mod guard_survivor_tests {
    //! The three NotFound-guard mutants in `read_prompt_candidate` are log-only —
    //! every arm returns `None`. They are observable only through which log level
    //! fires, so this test installs a capturing subscriber.
    //!
    //! Classification from mutation runs: `guard -> false` and `== -> !=` are KILLED
    //! here (a missing file must stay silent, an unreadable one must warn).
    //! `guard -> true` is EQUIVALENT — cargo-mutants keeps the arm's silent body, and
    //! both arms return None identically; only tracing output differs.

    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default, Clone)]
    struct Captured(Arc<Mutex<Vec<(tracing::Level, String)>>>);
    impl<S: tracing::Subscriber> tracing_subscriber::layer::Layer<S> for Captured {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            // Record every field, not just the message: the path that distinguishes
            // one warn from another lives in structured fields.
            struct Msg(Vec<String>);
            impl tracing::field::Visit for Msg {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    self.0.push(format!("{}={v:?}", f.name()));
                }
            }
            let mut m = Msg(Vec::new());
            event.record(&mut m);
            let text = m.0.join(" ");
            self.0
                .lock()
                .unwrap()
                .push((*event.metadata().level(), text));
        }
    }

    #[test]
    fn missing_file_is_quiet_while_unreadable_and_empty_are_loud() {
        use tracing_subscriber::layer::SubscriberExt as _;
        let captured = Captured::default();
        let sub = tracing_subscriber::registry().with(captured.clone());

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.md");
        let empty = dir.path().join("empty.md");
        std::fs::write(&empty, "   \n").unwrap();

        // An unreadable file: exists, but mode 000 denies a non-root reader.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = dir.path().join("locked.md");
            std::fs::write(&locked, "secret\n").unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

            tracing::subscriber::with_default(sub, || {
                assert_eq!(read_prompt_candidate(&missing), None);
                assert_eq!(read_prompt_candidate(&locked), None);
                assert_eq!(read_prompt_candidate(&empty), None);
            });

            let seen = captured.0.lock().unwrap();
            assert!(
                seen.iter()
                    .any(|(l, m)| *l == tracing::Level::WARN && m.contains("could not be read")),
                "an UNREADABLE file must warn loudly: {seen:?}"
            );
            assert!(
                !seen.iter().any(|(_, m)| m.contains("missing.md")),
                "a MISSING file stays silent at every level: {seen:?}"
            );
            assert!(
                seen.iter()
                    .any(|(l, m)| *l == tracing::Level::WARN && m.contains("prompt file is empty")),
                "an EMPTY file must warn: {seen:?}"
            );
        }
    }
}
