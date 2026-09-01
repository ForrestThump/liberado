use super::*;

pub(super) fn write_smoke_request(workspace: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let request_path = workspace.join("request.json");
    fs::write(
        &request_path,
        serde_json::to_vec_pretty(&smoke_request(workspace))?,
    )?;
    Ok(request_path)
}

pub(super) fn execute_smoke_runner(
    root: &Path,
    runner: &Path,
    request_path: &Path,
) -> Result<(bool, String, String, String), Box<dyn std::error::Error>> {
    let provider = smoke_provider();
    let output = std_command(runner)
        .args([
            "--request",
            request_path.to_str().ok_or("request path is not UTF-8")?,
        ])
        .env("LIBERADO_CODER_PROVIDER", provider)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((
        output.status.success(),
        stdout,
        stderr,
        format!("{}", output.status),
    ))
}

pub(super) fn classify_smoke_result(
    success: bool,
    stdout: &str,
    stderr: &str,
    status: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if success {
        println!("OK: live provider completed a coding run");
        return Ok(());
    }
    if smoke_boundary_reached(stdout, stderr) {
        println!("OK: runner reached the provider boundary without credentials");
        println!("  exit status: {status}");
        return Ok(());
    }
    let combined = format!("{stdout}\n{stderr}")
        .to_lowercase()
        .trim()
        .to_string();
    Err(format!("coder smoke failed with {status}:\n{combined}").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_result_accepts_success_or_provider_boundary_and_rejects_early_failure() {
        classify_smoke_result(true, "", "", "exit 0").unwrap();
        classify_smoke_result(false, "", "API key required", "exit 1").unwrap();
        let error = classify_smoke_result(false, "build failed", "boom", "exit 2")
            .unwrap_err()
            .to_string();
        assert!(error.contains("build failed"));
        assert!(error.contains("exit 2"));
    }

    #[test]
    fn smoke_repository_is_a_real_committed_git_workspace() {
        let root = tempfile::tempdir().unwrap();
        initialize_smoke_repository(root.path()).unwrap();
        assert!(root.path().join("README.md").is_file());
        let head = std::process::Command::new("git")
            .args(["-C", root.path().to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(head.status.success());
        assert!(!head.stdout.is_empty());
    }
}
