//! Mechanical enforcement of one rule: **no raw `Command::new` in production code.**
//!
//! `tokio::process::Command::output()` pipes stdout and stderr but leaves stdin *inherited*.
//! When the parent's stdin is a protocol wire rather than a terminal, every child blocks on it.
//! That is not hypothetical: the ACP bridge speaks JSON-RPC over stdin, and a Paseo coding
//! prompt hung for 19 minutes on `git worktree prune` having never called a model — the child
//! never wrote even its first `GIT_TRACE` line, so the failure was completely silent.
//!
//! One wrong default at 41 independent call sites, with no single place to fix it. This test is
//! that single place: `liberado_common::process::command` (async) and `std_command` (blocking)
//! set `Stdio::null()`, and a new `Command::new` anywhere in production fails here.
//!
//! Same spirit as `layer_rules.rs`: the first instance was found by hand and cost an afternoon;
//! the next one should be found by CI in seconds.

use std::path::{Path, PathBuf};

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .to_path_buf()
}

/// Paths allowed to construct a `Command` directly, and why.
fn is_exempt(path: &Path) -> bool {
    let p = path.to_string_lossy().replace('\\', "/");

    // The helper itself — something has to call the real constructor.
    if p.ends_with("common/src/process.rs") {
        return true;
    }
    // Build scripts run under cargo with a stdin nobody is speaking a protocol over, and
    // cannot depend on a workspace crate anyway.
    if p.ends_with("/build.rs") {
        return true;
    }
    // Test code. Fixtures spawn throwaway git repos and are free to do it directly; the rule
    // protects the daemon and the packs, not the harness.
    p.contains("/tests/")
        || p.ends_with("/tests.rs")
        || p.ends_with("_tests.rs")
        || p.contains("/test_support/")
}

/// Source lines of `file` that lie above the first `#[cfg(test)]`, with 1-based numbers.
///
/// A crude split, deliberately: the alternative is parsing Rust, and the failure mode of this
/// heuristic is over-strictness (flagging a test), which someone notices immediately — unlike
/// under-strictness, which is how the original bug survived.
fn production_lines(source: &str) -> Vec<(usize, &str)> {
    let cut = source
        .lines()
        .position(|l| l.contains("#[cfg(test)]"))
        .unwrap_or(usize::MAX);
    source
        .lines()
        .enumerate()
        .take_while(|(i, _)| *i < cut)
        .map(|(i, l)| (i + 1, l))
        .collect()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn production_code_never_constructs_a_command_directly() {
    let crates = crates_dir();
    let mut offenders: Vec<String> = Vec::new();

    for entry in std::fs::read_dir(&crates).expect("read crates/").flatten() {
        let src = entry.path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rust_sources(&src, &mut files);
        for file in files {
            if is_exempt(&file) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for (line_no, line) in production_lines(&text) {
                if line.contains("Command::new(") {
                    let rel = file
                        .strip_prefix(&crates)
                        .unwrap_or(&file)
                        .to_string_lossy()
                        .replace('\\', "/");
                    offenders.push(format!("  crates/{rel}:{line_no}  {}", line.trim()));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "raw `Command::new` in production code — a child spawned this way inherits this \
         process's stdin, which deadlocks every subprocess when stdin is a protocol wire \
         (the ACP bridge). Use `liberado_common::process::command` for async call sites or \
         `std_command` for blocking ones; both null the child's stdin.\n\n{}\n",
        offenders.join("\n")
    );
}

/// The rule is worth nothing if the scanner cannot see a violation, and a scanner that reports
/// "all clean" because its path handling is wrong looks exactly like success.
///
/// This is not paranoia: the process-tree walk in `scripts/repro-acp-prompt.js` reported "no git
/// descendants" while two hung git processes were on screen, because it filtered before walking.
#[test]
fn the_scanner_actually_detects_a_violation() {
    let sample = "fn main() {\n    let _ = Command::new(\"git\");\n}\n";
    let found: Vec<_> = production_lines(sample)
        .into_iter()
        .filter(|(_, l)| l.contains("Command::new("))
        .collect();
    assert_eq!(found.len(), 1, "scanner missed an obvious violation");
    assert_eq!(found[0].0, 2, "wrong line number reported");

    // …and that it stops at the test boundary rather than flagging fixtures.
    let with_tests =
        "fn main() {}\n#[cfg(test)]\nmod tests {\n    let _ = Command::new(\"git\");\n}\n";
    assert!(
        production_lines(with_tests)
            .iter()
            .all(|(_, l)| !l.contains("Command::new(")),
        "scanner reached past #[cfg(test)] and would flag test fixtures"
    );
}
