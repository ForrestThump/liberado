//! Rules that keep coding production surfaces on the shared assembly path.
//!
//! ## Why a scanner
//!
//! `CoderTuning::run_config` has a serde-driven test (`every_shared_field_survives_the_conversion_to_run_config`)
//! that proves each tuning field *arrives* in `CoderRunConfig`. It cannot prove the consumer then
//! reads it. Seven settings have shipped green while a call site built a literal instead, and the
//! backlog names that residue explicitly.
//!
//! `hashline` is the instance that cost a run. `coder-runner` hardcoded
//! `HashlineConfig { enabled: true, hash_length: 7 }`; the ACP path passed `tuning.hashline`,
//! whose default was `enabled: false`. So the two coding paths disagreed about whether the model
//! had `hashline_edit` at all, and the path we dogfood through was the one without it.
//!
//! Backlog **0.4** generalises that: the three production entry points must not hand-build a
//! full `CoderRunConfig { … }` tree. Shared knobs go through `assemble_production_run`. A fourth
//! site reintroducing a literal must fail this gate.
//!
//! **Deliberately narrow.** Fan-out children and in-crate unit fixtures still construct configs
//! for isolation. The ban is production code in the three surfaces (and pack build), not every
//! `CoderRunConfig` in the workspace.
//!
//! Same shape as `subprocess_rules.rs` and `layer_rules.rs`.

use std::path::{Path, PathBuf};

/// Config constructions no production surface may introduce, and what they must use instead.
///
/// `HashlineConfig` is only banned on the two outer surfaces (where the original divergence
/// lived). The pack may construct it when resolving mode policy (explore forces hashline off).
/// `CoderRunConfig` is banned on all three production assembly sites.
const BANNED_OUTER: &[(&str, &str)] = &[("HashlineConfig {", "tuning.hashline")];
const BANNED_ALL: &[(&str, &str)] = &[(
    "CoderRunConfig {",
    "liberado_coder_agent::assemble_production_run",
)];

/// Crates whose production code is checked.
///
/// `coder-agent` is scanned only under `session_pack/` (the pack build entry). Fan-out and other
/// pack-internal fixtures are outside this gate by path.
const SURFACES: &[&str] = &["coder-runner", "acp-bridge", "coder-agent"];

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

/// Whether this file is in scope for the production-construction ban.
///
/// For `coder-agent`, only the session-pack build path is a production assembly site.
/// `assemble.rs` is the shared constructor itself and must be free to build the config once.
fn production_surface_file(surface: &str, file: &Path) -> bool {
    if surface != "coder-agent" {
        return true;
    }
    let s = file.to_string_lossy().replace('\\', "/");
    s.contains("/session_pack/") && !s.contains("/assemble.rs")
}

fn violations() -> Vec<String> {
    let mut found = Vec::new();
    for surface in SURFACES {
        let src = crates_dir().join(surface).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for file in files {
            if !production_surface_file(surface, &file) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            let banned: Vec<(&str, &str)> = if *surface == "coder-agent" {
                BANNED_ALL.to_vec()
            } else {
                BANNED_OUTER
                    .iter()
                    .chain(BANNED_ALL.iter())
                    .copied()
                    .collect()
            };
            for (number, line) in production_lines(&source) {
                let code = line.split("//").next().unwrap_or(line);
                for (needle, replacement) in &banned {
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
    let sample = "fn build() {\n    hashline: HashlineConfig { enabled: true },\n    config: CoderRunConfig {\n}\n";
    let needles: Vec<&str> = BANNED_OUTER
        .iter()
        .chain(BANNED_ALL.iter())
        .map(|(n, _)| *n)
        .collect();
    let hits: Vec<(usize, &str)> = production_lines(sample)
        .into_iter()
        .filter_map(|(n, l)| {
            let code = l.split("//").next().unwrap_or(l);
            needles
                .iter()
                .find(|needle| code.contains(*needle))
                .map(|needle| (n, *needle))
        })
        .collect();
    assert_eq!(
        hits,
        vec![(2, "HashlineConfig {"), (3, "CoderRunConfig {")],
        "the matcher does not see a literal it must reject"
    );

    // And a mention inside a comment must not trip it, or the rule cannot be documented in the
    // files it governs.
    let commented =
        "    // was HashlineConfig { enabled: true } / CoderRunConfig { before the fix\n";
    let commented_hits = production_lines(commented)
        .into_iter()
        .filter(|(_, l)| {
            let code = l.split("//").next().unwrap_or(l);
            needles.iter().any(|needle| code.contains(needle))
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
