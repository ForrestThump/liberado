//! Mechanical guardrails for roadmap step 1: no permissive CORS and no wide-open API bind.
//!
//! Same shape as `subprocess_rules.rs` — scan production Rust for patterns that used to ship
//! on `main` and must not return.

use std::path::{Path, PathBuf};

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .to_path_buf()
}

fn is_exempt(path: &Path) -> bool {
    let p = path.to_string_lossy().replace('\\', "/");
    p.contains("/tests/")
        || p.ends_with("/tests.rs")
        || p.ends_with("_tests.rs")
        || p.contains("/test_support/")
}

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

fn line_is_comment_or_doc(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("//!")
}

#[test]
fn production_code_never_uses_permissive_cors_or_a_public_api_bind() {
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
                if line_is_comment_or_doc(line) {
                    continue;
                }
                if line.contains("CorsLayer::permissive()") {
                    let rel = file.strip_prefix(&crates).expect("under crates");
                    offenders.push(format!(
                        "{}:{line_no}: CorsLayer::permissive() is forbidden — use [api] allowed_origins",
                        rel.display()
                    ));
                }
                if line.contains("\"0.0.0.0:4201\"") {
                    let rel = file.strip_prefix(&crates).expect("under crates");
                    offenders.push(format!(
                        "{}:{line_no}: API must not bind 0.0.0.0:4201 — use 127.0.0.1 or [api] bind",
                        rel.display()
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "control-plane regressions:\n{}",
        offenders.join("\n")
    );
}
