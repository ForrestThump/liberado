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
    assert!(text.contains("cancel-in-progress: true"));

    for job in [
        "early-lint",
        "test",
        "webui",
        "module-health",
        "crap",
        "doc-links",
        "rustdoc",
        "deploy-image",
    ] {
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
            body.contains("needs:") && body.contains("dependency-security"),
            "compiling job {job} bypasses dependency admission"
        );
    }

    assert!(text.contains("cargo metadata --locked"));
    assert!(text.contains("cargo deny --locked check"));
    assert!(text.contains("cargo vet --locked"));
    assert!(
        !text.contains("checkout-siblings"),
        "CI must let cargo fetch git+tag forks; do not check out path siblings"
    );
}

#[test]
fn deploy_image_job_publishes_to_ghcr_with_least_privilege() {
    let text = std::fs::read_to_string(repository_root().join(".github/workflows/ci.yml"))
        .expect("read CI workflow");
    assert!(text.contains("ghcr.io/forrestthump/liberado"));
    assert!(text.contains("BAKE_WEBUI=1"));
    assert!(text.contains("CARGO_BUILD_JOBS=1"));
    assert!(text.contains("provenance: false"));
    assert!(text.contains("packages: write"));
    assert!(!text.contains("contents: write"));
    assert!(text.contains("github.event.pull_request.head.sha || github.sha"));
    assert!(text.contains("sha-${COMMIT_SHA}"));

    let marker = "  deploy-image:\n";
    let start = text.find(marker).expect("missing deploy-image job");
    let body = text[start + marker.len()..]
        .lines()
        .take_while(|line| line.is_empty() || line.starts_with("    "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        body.contains("packages: write") && body.contains("contents: read"),
        "deploy-image must grant packages write without inheriting a write-all token"
    );
    assert!(
        !text[..start].contains("packages: write"),
        "packages: write must stay on the image job, not the workflow default"
    );
    assert!(
        body.contains("needs: [dependency-security, early-lint, test, webui, module-health"),
        "deploy-image must wait for the fast validation jobs"
    );
    assert!(
        body.contains("contains(github.event.pull_request.labels.*.name, 'deploy-image')"),
        "pull-request deploy images must require an explicit deploy-image label"
    );
}

#[test]
fn webui_release_build_covers_the_advertised_contract() {
    let root = repository_root();
    let workflow =
        std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read CI workflow");

    assert!(workflow.contains("  webui:\n"));
    assert!(workflow.contains("rustup target add wasm32-unknown-unknown"));
    assert!(workflow.contains("dx build -r -p liberado-webui --web"));
    assert!(workflow.contains("test -f \"${bundle}/manifest.json\""));
    assert!(workflow.contains("test -f \"${bundle}/sw.js\""));
}

#[test]
fn workspace_rust_version_matches_the_pinned_ci_toolchain() {
    let root = repository_root();
    let workspace: toml::Value = std::fs::read_to_string(root.join("Cargo.toml"))
        .expect("read workspace manifest")
        .parse()
        .expect("parse workspace manifest");
    let toolchain: toml::Value = std::fs::read_to_string(root.join("rust-toolchain.toml"))
        .expect("read pinned toolchain")
        .parse()
        .expect("parse pinned toolchain");
    let rust_version = workspace["workspace"]["package"]["rust-version"]
        .as_str()
        .expect("workspace rust-version");
    let channel = toolchain["toolchain"]["channel"]
        .as_str()
        .expect("pinned toolchain channel");

    assert_eq!(
        rust_version, channel,
        "the application supports one compiler"
    );
}

#[test]
fn pre_push_hook_requires_a_current_readiness_receipt() {
    let root = repository_root();
    let hook = std::fs::read_to_string(root.join(".githooks/pre-push"))
        .expect("read committed pre-push hook");
    let justfile = std::fs::read_to_string(root.join("justfile")).expect("read justfile");
    assert!(hook.contains("ci verify-ready"));
    assert!(justfile.contains("git config core.hooksPath .githooks"));
    assert!(
        justfile.contains("ready: setup-hooks"),
        "the readiness path must install the committed hook automatically"
    );
    assert!(
        justfile.contains("push: ci ready verify-ready"),
        "the canonical push path must run full CI and final readiness"
    );
    let readiness = std::fs::read_to_string(root.join("crates/cli/src/readiness_cmd.rs"))
        .expect("read readiness contract");
    assert!(readiness.contains("crap_linux(root)?"));
    assert!(readiness.contains("full-local-ci"));
    assert!(readiness.contains("exact-linux-crap"));
}

#[test]
fn early_complexity_uses_the_same_native_policy_as_local_readiness() {
    let root = repository_root();
    let workflow =
        std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read CI workflow");
    let policy = std::fs::read_to_string(root.join("function-complexity.toml"))
        .expect("read complexity policy");
    assert!(policy.contains("new_function_ceiling = 20"));
    assert!(workflow.contains("cargo run --locked -p liberado-cli -- ci complexity"));
    assert!(!workflow.contains("--threshold 420"));
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
