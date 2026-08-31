//! One rule: **harness prompts live in `prompts/`, not in Rust.**
//!
//! ## Why
//!
//! Every prompt the coding harness uses was a string literal — a 900-character block inside
//! `coder-runner`'s `run_headless`, a `const` for the diff reviewer, a function returning a
//! literal for the session critic. Three consequences, all of which cost something:
//!
//! - Retuning any of them meant a full workspace rebuild. Prompt work is iterative — the session
//!   critic went from 2 of 4 labelled traces to 4 of 4 on one wording change — so the loop that
//!   most wants a fast turnaround had the slowest one available.
//! - Two of them had drifted from `prompts/coder/coder.md`, a file that already existed and
//!   already claimed to be the coder's prompt. Nobody could tell which text a run had used.
//! - A prompt in a binary cannot be changed by the person operating a deployment.
//!
//! `liberado_coder_core::prompts` fixes it: one file per prompt, baked in with `include_str!` as
//! a fallback and read from disk when it is there. This test stops the literals coming back.
//!
//! ## What it looks for
//!
//! A long string literal that reads like instructions to a model — second person, imperative,
//! the vocabulary of the coding tools. Heuristic on purpose: the alternative is a lint nobody can
//! implement, and the failure mode of a heuristic here is a false positive that someone notices
//! in one CI run. Under-strictness is what let three prompts diverge unnoticed for months.

use std::path::{Path, PathBuf};

/// Crates whose production code must not carry a prompt.
const SURFACES: &[&str] = &["coder-runner", "coder-agent", "acp-bridge", "coder-core"];

/// A literal longer than this, that also looks like model instructions, is a prompt.
///
/// 400 characters clears every legitimate long string in these crates — error messages, JSON
/// schema descriptions, SQL — while sitting well under the shortest real prompt (the diff
/// reviewer, ~700).
const PROMPT_LENGTH: usize = 400;

/// Phrases that mark text as addressed to a model rather than to a developer.
const MODEL_VOICE: &[&str] = &[
    "You are Liberado",
    "you are liberado",
    "submit_report",
    "Respond with JSON only",
    "outcome=succeeded",
];

/// Files allowed to hold prompt text, and why.
fn is_exempt(path: &Path) -> bool {
    let p = path.to_string_lossy().replace('\\', "/");
    // The module whose entire job is to hold the baked copies.
    if p.ends_with("coder-core/src/prompts.rs") {
        return true;
    }
    // Mode presets that are policy, not tunable instructions: they describe a *capability
    // restriction* the harness enforces in code, so a deployment editing them would be describing
    // a sandbox it does not have.
    if p.ends_with("coder-core/src/lib.rs") {
        return true;
    }
    p.contains("/tests/")
        || p.ends_with("/tests.rs")
        || p.ends_with("_tests.rs")
        || p.contains("/test_support/")
}

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Production source of `file` — everything above the first `#[cfg(test)]`.
fn production(source: &str) -> String {
    match source.find("#[cfg(test)]") {
        Some(cut) => source[..cut].to_string(),
        None => source.to_string(),
    }
}

/// Every string literal in `source` at least [`PROMPT_LENGTH`] long.
///
/// A deliberately simple scan: find a `"`, read to the next unescaped `"`. It does not understand
/// raw strings or comments, which is why the caller also checks for model voice — a false hit on
/// a doc comment would still need to contain `submit_report` to be reported.
fn long_literals(source: &str) -> Vec<String> {
    let bytes: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '"' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() {
                if bytes[j] == '\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == '"' {
                    break;
                }
                j += 1;
            }
            if j <= bytes.len() && j > start {
                let literal: String = bytes[start..j.min(bytes.len())].iter().collect();
                if literal.len() >= PROMPT_LENGTH {
                    out.push(literal);
                }
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

fn looks_like_a_prompt(text: &str) -> bool {
    MODEL_VOICE.iter().any(|marker| text.contains(marker))
}

fn violations() -> Vec<String> {
    let mut found = Vec::new();
    for surface in SURFACES {
        let src = crates_dir().join(surface).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for file in files {
            if is_exempt(&file) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            for literal in long_literals(&production(&source)) {
                if looks_like_a_prompt(&literal) {
                    let head: String = literal.chars().take(70).collect();
                    found.push(format!("{}: \"{head}…\"", file.display()));
                }
            }
        }
    }
    found
}

#[test]
fn harness_prompts_are_not_compiled_into_the_binary() {
    let found = violations();
    assert!(
        found.is_empty(),
        "a model prompt is a string literal again. Put it in prompts/coder/ and load it with \
         liberado_coder_core::prompts, so it can be retuned without a rebuild and a deployment \
         can override it.\n{}",
        found.join("\n")
    );
}

/// The scanner must be able to fail. One that reports all-clean because its parser is wrong looks
/// exactly like success.
#[test]
fn the_scanner_actually_detects_a_prompt() {
    let sample = format!(
        "fn role() {{\n    prompt: Some(\"You are Liberado's coding worker. {}\".to_string()),\n}}",
        "Do the thing carefully and well. ".repeat(15)
    );
    let hits = long_literals(&production(&sample))
        .into_iter()
        .filter(|l| looks_like_a_prompt(l))
        .count();
    assert_eq!(hits, 1, "the matcher does not see a prompt literal");
}

/// A long literal that is *not* a prompt must pass. An error message or a JSON schema description
/// can be hundreds of characters, and flagging those is how a rule gets switched off.
#[test]
fn a_long_non_prompt_literal_is_allowed() {
    let sample = format!(
        "fn e() {{ return Err(\"{}\"); }}",
        "database connection failed after retrying. ".repeat(12)
    );
    let hits = long_literals(&production(&sample))
        .into_iter()
        .filter(|l| looks_like_a_prompt(l))
        .count();
    assert_eq!(hits, 0, "a long error message is not a prompt");
}

/// Test code may hold prompt text — fixtures need it, and the rule protects the shipped binary.
#[test]
fn test_code_is_not_scanned() {
    let sample = "fn a() {}\n#[cfg(test)]\nmod t {\n  const P: &str = \"You are Liberado's coding worker...\";\n}";
    assert!(
        !production(sample).contains("You are Liberado"),
        "everything below #[cfg(test)] must be out of scope"
    );
}
