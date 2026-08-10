//! One rule: **the coding surfaces may not build their own `HashlineConfig`.**
//!
//! ## Why a scanner, and why this narrow
//!
//! `CoderTuning::run_config` has a serde-driven test (`every_shared_field_survives_the_conversion_to_run_config`)
//! that proves each tuning field *arrives* in `CoderRunConfig`. It cannot prove the consumer then
//! reads it. Seven settings have shipped green while a call site built a literal instead, and the
//! backlog names that residue explicitly.
//!
//! `hashline` is the instance that cost a run. `coder-runner` hardcoded
//! `HashlineConfig { enabled: true, hash_length: 7 }`; the ACP path passed `tuning.hashline`,
//! whose default was `enabled: false`. So the two coding paths disagreed about whether the model
//! had `hashline_edit` at all, and the path we dogfood through was the one without it. A dispatched
//! run then failed on exactly that: 15 of 25 `edit_file` calls returned `old text was not found`
//! or `old text matched 2 times; provide more context`, and it filed `failed` having landed
//! nothing. The tool that makes those two errors impossible was built, worked, and was off.
//!
//! A unit test cannot catch this. Re-hardcoding the literal in `coder-runner` compiles and leaves
//! every test in `coder-core` green — measured, not assumed. Only reading the source catches it.
//!
//! **Deliberately one type, not all of them.** Several config structs are legitimately built at
//! call sites — `disabled_role` returns a `CoderRoleConfig`, and `ProgressPolicy { ..Default }`
//! is a reasonable spread. A blanket ban would be wrong on its face and would be disabled within
//! a month, which is worse than no rule. Add a type here when its divergence has actually caused
//! harm; the list is evidence, not taste.
//!
//! Same shape as `subprocess_rules.rs` and `layer_rules.rs`: the first instance was found by hand
//! and cost a run, the next should be found by CI in seconds.

use std::path::{Path, PathBuf};

/// Config types no surface may construct directly, and the field they must read instead.
const BANNED: &[(&str, &str)] = &[("HashlineConfig {", "tuning.hashline")];

/// Crates whose production code is checked. These are the two coding entry points whose
/// divergence is the failure being prevented.
const SURFACES: &[&str] = &["coder-runner", "acp-bridge"];

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .to_path_buf()
}

/// Source lines above the first `#[cfg(test)]`, 1-based.
///
/// The same crude split `subprocess_rules.rs` uses, for the same reason: parsing Rust is the
/// alternative, and this heuristic fails toward over-strictness — which someone notices at once,
/// unlike under-strictness, which is how the original bug survived.
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

fn violations() -> Vec<String> {
    let mut found = Vec::new();
    for surface in SURFACES {
        let src = crates_dir().join(surface).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for file in files {
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            for (number, line) in production_lines(&source) {
                let code = line.split("//").next().unwrap_or(line);
                for (needle, replacement) in BANNED {
                    if code.contains(needle) {
                        found.push(format!(
                            "{}:{number}: builds `{needle}` directly; read `{replacement}` instead",
                            file.display()
                        ));
                    }
                }
            }
        }
    }
    found
}

#[test]
fn coding_surfaces_read_their_config_instead_of_building_it() {
    let found = violations();
    assert!(
        found.is_empty(),
        "a coding surface is hardcoding config its sibling path reads from tuning. \
         That divergence cost a dispatched run: `hashline_edit` was on for one path and off for \
         the other, and 15 of 25 edits failed on ambiguous string anchors.\n{}",
        found.join("\n")
    );
}

/// The scanner must be able to fail.
///
/// A checker that reports all-clean because its path handling is wrong looks exactly like
/// success, and this file's whole value is that it fails when someone reintroduces a literal.
#[test]
fn the_scanner_actually_detects_a_violation() {
    let sample = "fn build() {\n    hashline: HashlineConfig { enabled: true },\n}\n";
    let hits: Vec<usize> = production_lines(sample)
        .into_iter()
        .filter(|(_, l)| {
            let code = l.split("//").next().unwrap_or(l);
            BANNED.iter().any(|(needle, _)| code.contains(needle))
        })
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        hits,
        vec![2],
        "the matcher does not see a literal it must reject"
    );

    // And a mention inside a comment must not trip it, or the rule cannot be documented in the
    // files it governs.
    let commented = "    // was HashlineConfig { enabled: true } before the fix\n";
    let commented_hits = production_lines(commented)
        .into_iter()
        .filter(|(_, l)| {
            let code = l.split("//").next().unwrap_or(l);
            BANNED.iter().any(|(needle, _)| code.contains(needle))
        })
        .count();
    assert_eq!(
        commented_hits, 0,
        "a comment naming the literal must be allowed"
    );
}

/// The surfaces named must exist. A rule that scans a directory nobody has is silently vacuous.
#[test]
fn every_named_surface_exists() {
    for surface in SURFACES {
        let src = crates_dir().join(surface).join("src");
        assert!(
            src.is_dir(),
            "SURFACES names `{surface}`, but {} does not exist — the rule covers nothing",
            src.display()
        );
    }
}
