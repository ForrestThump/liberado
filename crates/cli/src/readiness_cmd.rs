//! Cross-platform local readiness receipts and Debian CRAP reproduction.

use liberado_common::process::std_command;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;

const RECEIPT_FILE: &str = ".liberado/ready.json";

#[derive(Debug, Deserialize, Serialize)]
struct Receipt {
    version: u32,
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
    compile_gate(root)?;
    test_changed_packages(root)?;
    audits(root)?;
    write_receipt(root)?;
    eprintln!("[ready] OK; receipt: {RECEIPT_FILE}");
    Ok(())
}

pub fn verify(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(root.join(RECEIPT_FILE))
        .map_err(|_| "no readiness receipt; run `just ready` after the last commit or rebase")?;
    let receipt: Receipt = serde_json::from_slice(&bytes)?;
    let head = git_text(root, &["rev-parse", "HEAD"])?;
    let tree_sha256 = tree_fingerprint(root)?;
    if receipt.head != head || receipt.tree_sha256 != tree_sha256 {
        return Err(
            "readiness receipt is stale; HEAD or the working tree changed. Run `just ready` again"
                .into(),
        );
    }
    eprintln!("[ready] receipt matches HEAD {head}");
    Ok(())
}

pub fn crap_linux(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(target_os = "linux") {
        return crate::ci_cmd::crap_for_root(root);
    }
    if !cfg!(windows) {
        return Err(
            "`just crap-linux` supports Debian/Linux and Windows with Debian under WSL".into(),
        );
    }
    let distro = std::env::var("LIBERADO_DEBIAN_WSL_DISTRO").unwrap_or_else(|_| "Debian".into());
    let linux_root = wsl_path(root, &distro)?;
    let status = std_command("wsl.exe")
        .args([
            "-d",
            &distro,
            "--cd",
            linux_root.trim(),
            "bash",
            "-lc",
            "cargo run --locked --quiet -p liberado-cli -- ci crap",
        ])
        .status()
        .map_err(|error| format!("could not start Debian under WSL: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Debian CRAP check failed; inspect .liberado/ci.log from WSL".into())
    }
}

fn wsl_path(root: &Path, distro: &str) -> Result<String, Box<dyn std::error::Error>> {
    let output = std_command("wsl.exe")
        .args(["-d", distro, "wslpath", "-a", "-u"])
        .arg(wsl_path_input(root))
        .output()
        .map_err(|error| format!("could not query Debian under WSL: {error}"))?;
    if output.status.success() {
        Ok(decode_output(&output.stdout).trim().to_string())
    } else {
        let detail = if output.stderr.is_empty() {
            decode_output(&output.stdout)
        } else {
            decode_output(&output.stderr)
        };
        Err(format!(
            "Debian WSL distribution `{distro}` could not map the repository path: {}. Install it with `wsl --install -d Debian` or set LIBERADO_DEBIAN_WSL_DISTRO",
            detail.trim()
        )
        .into())
    }
}

fn wsl_path_input(root: &Path) -> String {
    root.to_string_lossy().replace('\\', "/")
}

fn decode_output(bytes: &[u8]) -> String {
    if cfg!(windows) && bytes.chunks_exact(2).any(|pair| pair[1] == 0) {
        let words = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
        String::from_utf16_lossy(&words.collect::<Vec<_>>())
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn write_receipt(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(root.join(".liberado"))?;
    let receipt = Receipt {
        version: 1,
        head: git_text(root, &["rev-parse", "HEAD"])?,
        tree_sha256: tree_fingerprint(root)?,
        rustc: command_text(root, "rustc", &["--version"])?,
        host: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        completed_at: chrono::Utc::now().to_rfc3339(),
        checks: vec![
            "fmt".into(),
            "locked-metadata".into(),
            "clippy".into(),
            "changed-package-tests".into(),
            "module-health".into(),
            "function-complexity".into(),
            "docs-audit".into(),
        ],
    };
    let mut json = serde_json::to_string_pretty(&receipt)?;
    json.push('\n');
    std::fs::write(root.join(RECEIPT_FILE), json)?;
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
    use super::{Receipt, decode_output, tree_fingerprint, wsl_path_input};
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
        git(temp.path(), &["add", "tracked.txt"]);
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
            version: 1,
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
    fn windows_utf16_command_output_is_readable() {
        let bytes: Vec<_> = "Debian missing"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        if cfg!(windows) {
            assert_eq!(decode_output(&bytes), "Debian missing");
        }
    }

    #[test]
    fn wslpath_input_preserves_a_windows_path() {
        assert_eq!(
            wsl_path_input(std::path::Path::new(r"C:\tmp\life-os-pr216-fix")),
            "C:/tmp/life-os-pr216-fix"
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
