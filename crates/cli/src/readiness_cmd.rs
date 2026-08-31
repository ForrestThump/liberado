//! Cross-platform local readiness receipts and Debian CRAP reproduction.

use liberado_common::process::std_command;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;

#[path = "readiness_cmd/debian_crap.rs"]
mod debian_crap;

const RECEIPT_FILE: &str = ".liberado/ready.json";
const CI_RECEIPT_FILE: &str = ".liberado/ci-ready.json";
const CRAP_RECEIPT_FILE: &str = ".liberado/crap-linux-ready.json";
const RECEIPT_VERSION: u32 = 2;
const CI_RECEIPT_KIND: &str = "full-ci";
const CRAP_RECEIPT_KIND: &str = "linux-crap";
const READY_RECEIPT_KIND: &str = "ready";
const REQUIRED_READY_CHECKS: &[&str] = &["full-local-ci", "exact-linux-crap"];

#[derive(Debug, Deserialize, Serialize)]
struct Receipt {
    version: u32,
    kind: String,
    head: String,
    tree_sha256: String,
    rustc: String,
    host: String,
    completed_at: String,
    checks: Vec<String>,
}

/// Everything that compiles or lints the workspace, in dependency order. Split from `ready`
/// so the driver stays under the complexity ceiling — these steps only run for real.
fn compile_gate(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crate::ci_cmd::vacate_cargo_target_image()?;
    run(root, "cargo", &["fmt", "--check"])?;
    run_quiet(
        root,
        "cargo",
        &["metadata", "--locked", "--format-version=1", "--no-deps"],
    )?;
    run(
        root,
        "cargo",
        &[
            "clippy",
            "--locked",
            "--workspace",
            "--exclude",
            "liberado-webui",
            "--all-targets",
            "--",
            "-D",
            "warnings",
            "-D",
            "clippy::cognitive_complexity",
        ],
    )
}

/// The source-level audits after the build is green. Same thin-driver reasoning as
/// [`compile_gate`].
fn audits(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crate::module_health_cmd::check(root)?;
    crate::function_complexity_cmd::check(root)?;
    audit_docs(root)
}

pub(crate) fn audit_docs(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crate::docs_audit_cmd::run(root, ["--base".to_string(), change_base(root)?].into_iter())
}

pub fn ready(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    prepare_ready(root)?;
    finish_ready(root)
}

fn prepare_ready(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    verify_evidence(
        root,
        CI_RECEIPT_FILE,
        CI_RECEIPT_KIND,
        "run `just ci` after the final commit",
    )?;
    compile_gate(root)?;
    test_changed_packages(root)?;
    audits(root)
}

fn finish_ready(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crap_linux(root)?;
    verify_evidence(
        root,
        CRAP_RECEIPT_FILE,
        CRAP_RECEIPT_KIND,
        "run `just crap-linux` after the final commit",
    )?;
    write_receipt(root, RECEIPT_FILE, READY_RECEIPT_KIND, ready_checks())?;
    eprintln!("[ready] OK; receipt: {RECEIPT_FILE}");
    Ok(())
}

pub fn verify(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let receipt = verify_evidence(
        root,
        RECEIPT_FILE,
        READY_RECEIPT_KIND,
        "run `just ready` after the last commit or rebase",
    )?;
    if !REQUIRED_READY_CHECKS
        .iter()
        .all(|required| receipt.checks.iter().any(|check| check == required))
    {
        return Err("readiness receipt predates the full-CI and exact-Linux-CRAP contract; run `just ready` again".into());
    }
    eprintln!("[ready] receipt matches HEAD {}", receipt.head);
    Ok(())
}

fn verify_evidence(
    root: &Path,
    file: &str,
    kind: &str,
    recovery: &str,
) -> Result<Receipt, Box<dyn std::error::Error>> {
    let bytes =
        std::fs::read(root.join(file)).map_err(|_| format!("no {kind} receipt; {recovery}"))?;
    let receipt: Receipt = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid {kind} receipt; {recovery}: {error}"))?;
    let head = git_text(root, &["rev-parse", "HEAD"])?;
    let tree_sha256 = tree_fingerprint(root)?;
    if receipt.version != RECEIPT_VERSION
        || receipt.kind != kind
        || receipt.head != head
        || receipt.tree_sha256 != tree_sha256
    {
        return Err(format!(
            "{kind} receipt is stale or from an older contract; HEAD or the working tree changed. {recovery}"
        )
        .into());
    }
    Ok(receipt)
}

pub fn crap_linux(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    debian_crap::run(root)?;
    write_receipt(
        root,
        CRAP_RECEIPT_FILE,
        CRAP_RECEIPT_KIND,
        vec!["exact-linux-crap".into()],
    )?;
    eprintln!("[ready] exact Linux CRAP receipt: {CRAP_RECEIPT_FILE}");
    Ok(())
}

pub(crate) fn record_full_ci(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    write_receipt(
        root,
        CI_RECEIPT_FILE,
        CI_RECEIPT_KIND,
        vec!["full-local-ci".into()],
    )?;
    eprintln!("[liberado ci] readiness receipt: {CI_RECEIPT_FILE}");
    Ok(())
}

fn ready_checks() -> Vec<String> {
    [
        "full-local-ci",
        "fmt",
        "locked-metadata",
        "clippy",
        "changed-package-tests",
        "module-health",
        "function-complexity",
        "docs-audit",
        "exact-linux-crap",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn write_receipt(
    root: &Path,
    file: &str,
    kind: &str,
    checks: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(root.join(".liberado"))?;
    let receipt = Receipt {
        version: RECEIPT_VERSION,
        kind: kind.into(),
        head: git_text(root, &["rev-parse", "HEAD"])?,
        tree_sha256: tree_fingerprint(root)?,
        rustc: command_text(root, "rustc", &["--version"])?,
        host: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        completed_at: chrono::Utc::now().to_rfc3339(),
        checks,
    };
    let mut json = serde_json::to_string_pretty(&receipt)?;
    json.push('\n');
    std::fs::write(root.join(file), json)?;
    Ok(())
}

fn tree_fingerprint(root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let mut digest = Sha256::new();
    digest.update(git_bytes(root, &["diff", "--binary", "HEAD", "--"])?);
    let untracked = git_bytes(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for raw in untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        digest.update(raw);
        let relative = std::str::from_utf8(raw)?;
        let path = root.join(relative);
        if path.is_file() {
            digest.update(std::fs::read(path)?);
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[ready] {program} {}", args.join(" "));
    let status = std_command(program).args(args).current_dir(root).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("readiness gate failed: {program} {}", args.join(" ")).into())
    }
}

fn run_quiet(root: &Path, program: &str, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[ready] {program} {}", args.join(" "));
    let status = std_command(program)
        .args(args)
        .current_dir(root)
        .stdout(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("readiness gate failed: {program} {}", args.join(" ")).into())
    }
}

fn test_changed_packages(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut packages = changed_packages(root)?;
    packages.insert("liberado-test-support".to_string());
    let mut arguments = vec!["test".to_string(), "--locked".to_string()];
    for package in packages {
        arguments.push("-p".to_string());
        arguments.push(package);
    }
    arguments.push("--no-fail-fast".to_string());
    let borrowed: Vec<_> = arguments.iter().map(String::as_str).collect();
    run(root, "cargo", &borrowed)
}

fn changed_packages(root: &Path) -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
    let base = change_base(root)?;
    let names = git_text(root, &["diff", "--name-only", &base, "--"])?;
    let mut packages = BTreeSet::new();
    for name in names.lines() {
        let mut parts = name.split('/');
        if parts.next() != Some("crates") {
            continue;
        }
        let Some(directory) = parts.next() else {
            continue;
        };
        let manifest = root.join("crates").join(directory).join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let value: toml::Value = toml::from_str(&std::fs::read_to_string(manifest)?)?;
        if let Some(package) = value
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
        {
            packages.insert(package.to_string());
        }
    }
    Ok(packages)
}

fn change_base(root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    git_text(root, &["merge-base", "HEAD", "origin/main"])
        .or_else(|_| git_text(root, &["rev-parse", "HEAD^"]))
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    Ok(String::from_utf8(git_bytes(root, args)?)?
        .trim()
        .to_string())
}

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let output = std_command("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into())
    }
}

fn command_text(
    root: &Path,
    program: &str,
    args: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let output = std_command(program).args(args).current_dir(root).output()?;
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    } else {
        Err(format!("{program} {} failed", args.join(" ")).into())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CI_RECEIPT_KIND, RECEIPT_VERSION, REQUIRED_READY_CHECKS, Receipt, ready_checks,
        tree_fingerprint, verify_evidence, write_receipt,
    };
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(root: &std::path::Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn fingerprint_changes_with_tracked_and_untracked_content() {
        let temp = tempdir().unwrap();
        git(temp.path(), &["init"]);
        git(temp.path(), &["config", "user.email", "test@example.com"]);
        git(temp.path(), &["config", "user.name", "Test"]);
        fs::write(temp.path().join("tracked.txt"), "one").unwrap();
        fs::write(temp.path().join(".gitignore"), ".liberado/\n").unwrap();
        git(temp.path(), &["add", "tracked.txt", ".gitignore"]);
        git(temp.path(), &["commit", "-m", "base"]);
        let base = tree_fingerprint(temp.path()).unwrap();
        fs::write(temp.path().join("tracked.txt"), "two").unwrap();
        assert_ne!(base, tree_fingerprint(temp.path()).unwrap());
        fs::write(temp.path().join("tracked.txt"), "one").unwrap();
        fs::write(temp.path().join("new.txt"), "new").unwrap();
        assert_ne!(base, tree_fingerprint(temp.path()).unwrap());
    }

    #[test]
    fn receipt_schema_round_trips() {
        let receipt = Receipt {
            version: RECEIPT_VERSION,
            kind: "ready".into(),
            head: "abc".into(),
            tree_sha256: "def".into(),
            rustc: "rustc test".into(),
            host: "windows-x86_64".into(),
            completed_at: "2026-08-21T00:00:00Z".into(),
            checks: vec!["fmt".into()],
        };
        let json = serde_json::to_string(&receipt).unwrap();
        let decoded: Receipt = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.head, "abc");
    }

    #[test]
    fn final_receipt_contract_names_full_ci_and_exact_linux_crap() {
        let checks = ready_checks();
        assert!(
            REQUIRED_READY_CHECKS
                .iter()
                .all(|required| checks.iter().any(|check| check == required))
        );
    }

    #[test]
    fn evidence_receipt_is_bound_to_kind_head_and_tree() {
        let temp = tempdir().unwrap();
        git(temp.path(), &["init"]);
        git(temp.path(), &["config", "user.email", "test@example.com"]);
        git(temp.path(), &["config", "user.name", "Test"]);
        fs::write(temp.path().join("tracked.txt"), "one").unwrap();
        fs::write(temp.path().join(".gitignore"), ".liberado/\n").unwrap();
        git(temp.path(), &["add", "tracked.txt", ".gitignore"]);
        git(temp.path(), &["commit", "-m", "base"]);

        write_receipt(
            temp.path(),
            ".liberado/evidence.json",
            CI_RECEIPT_KIND,
            vec!["full-local-ci".into()],
        )
        .unwrap();
        verify_evidence(
            temp.path(),
            ".liberado/evidence.json",
            CI_RECEIPT_KIND,
            "rerun",
        )
        .unwrap();
        assert!(
            verify_evidence(
                temp.path(),
                ".liberado/evidence.json",
                "wrong-kind",
                "rerun"
            )
            .is_err()
        );

        fs::write(temp.path().join("tracked.txt"), "two").unwrap();
        assert!(
            verify_evidence(
                temp.path(),
                ".liberado/evidence.json",
                CI_RECEIPT_KIND,
                "rerun"
            )
            .is_err()
        );
    }

    #[test]
    fn changed_packages_reads_crate_manifests_from_the_branch_diff() {
        use super::changed_packages;

        let temp = tempdir().unwrap();
        git(temp.path(), &["init"]);
        // Identity: a merge-base with origin/main does not exist on a fresh repo, so this
        // exercises the HEAD^ fallback too.
        let env = [
            ("GIT_AUTHOR_NAME", "test"),
            ("GIT_AUTHOR_EMAIL", "test@example.com"),
            ("GIT_COMMITTER_NAME", "test"),
            ("GIT_COMMITTER_EMAIL", "test@example.com"),
        ];
        let commit = |msg: &str| {
            let mut cmd = Command::new("git");
            cmd.args(["commit", "--allow-empty", "-m", msg])
                .current_dir(temp.path());
            for (k, v) in env {
                cmd.env(k, v);
            }
            assert!(cmd.status().unwrap().success(), "commit {msg}");
        };
        commit("base");
        fs::create_dir_all(temp.path().join("crates/demo/src")).unwrap();
        fs::write(
            temp.path().join("crates/demo/Cargo.toml"),
            "[package]\nname = \"liberado-demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(temp.path().join("crates/demo/src/lib.rs"), "").unwrap();
        fs::write(temp.path().join("README.md"), "docs change").unwrap();
        git(temp.path(), &["add", "."]);
        commit("change");

        let packages = changed_packages(temp.path()).unwrap();
        assert_eq!(
            packages,
            ["liberado-demo".to_string()].into_iter().collect(),
            "only crates/ manifests from the diff are returned, not top-level files"
        );
    }
}
