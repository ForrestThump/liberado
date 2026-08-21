//! Mechanical security rules for GitHub Actions.
//!
//! A dependency build script executes before a test can inspect it. These
//! rules therefore protect the admission workflow itself: immutable actions,
//! no persisted credentials, a read-only token, and locked Cargo commands.

use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory")
        .parent()
        .expect("repository root")
        .to_path_buf()
}

fn yaml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read GitHub configuration") {
        let path = entry.expect("GitHub configuration entry").path();
        if path.is_dir() {
            yaml_files(&path, out);
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        ) {
            out.push(path);
        }
    }
}

fn remote_action_ref(line: &str) -> Option<&str> {
    let value = line.trim().strip_prefix("uses:")?.trim();
    if value.starts_with("./") {
        return None;
    }
    value
        .split_once('@')
        .map(|(_, reference)| reference.split_whitespace().next().unwrap_or(reference))
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[test]
fn workflow_dependencies_are_immutable() {
    let github = repository_root().join(".github");
    let mut files = Vec::new();
    yaml_files(&github.join("workflows"), &mut files);
    yaml_files(&github.join("actions"), &mut files);

    let mut offenders = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read workflow");
        for (index, line) in text.lines().enumerate() {
            if let Some(reference) = remote_action_ref(line)
                && !is_full_sha(reference)
            {
                offenders.push(format!("{}:{} `{reference}`", file.display(), index + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "remote Actions must use immutable full commit SHAs:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn workflow_has_no_privileged_compilation_path() {
    let text = std::fs::read_to_string(repository_root().join(".github/workflows/ci.yml"))
        .expect("read CI workflow");
    assert!(!text.contains("pull_request_target"));
    assert!(text.contains("permissions:\n  contents: read"));

    for job in ["test", "module-health", "crap", "doc-links", "rustdoc"] {
        let marker = format!("  {job}:\n");
        let start = text
            .find(&marker)
            .unwrap_or_else(|| panic!("missing job {job}"));
        let body = text[start + marker.len()..]
            .lines()
            .take_while(|line| line.is_empty() || line.starts_with("    "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("needs: dependency-security"),
            "compiling job {job} bypasses dependency admission"
        );
    }

    assert!(text.contains("cargo metadata --locked"));
    assert!(text.contains("cargo deny --locked check"));
    assert!(text.contains("cargo vet --locked"));
}

#[test]
fn checkouts_drop_credentials_and_pin_siblings() {
    let mut files = Vec::new();
    let github = repository_root().join(".github");
    yaml_files(&github.join("workflows"), &mut files);
    yaml_files(&github.join("actions"), &mut files);

    for file in files {
        let text = std::fs::read_to_string(&file).expect("read workflow");
        let lines: Vec<_> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if line.contains("uses: actions/checkout@") {
                let tail = lines[index + 1..lines.len().min(index + 10)].join("\n");
                assert!(
                    tail.contains("persist-credentials: false"),
                    "{}:{} checkout persists credentials",
                    file.display(),
                    index + 1
                );
            }
            if line.trim_start().starts_with("repository: ForrestThump/") {
                let tail = lines[index + 1..lines.len().min(index + 6)].join("\n");
                let reference = tail
                    .lines()
                    .find_map(|candidate| candidate.trim().strip_prefix("ref:"))
                    .map(str::trim)
                    .expect("sibling checkout must have a ref");
                assert!(
                    is_full_sha(reference),
                    "sibling ref is mutable: {reference}"
                );
            }
        }
    }
}

#[test]
fn the_action_ref_scanner_detects_a_mutable_tag() {
    assert_eq!(remote_action_ref("uses: actions/checkout@v4"), Some("v4"));
    assert!(!is_full_sha("v4"));
    assert!(is_full_sha("11d5960a326750d5838078e36cf38b85af677262"));
}
