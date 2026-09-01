use super::*;

pub(super) fn build_liberado_runner(
    manifest: &CompareManifest,
    layout: &HarnessLayout,
) -> Result<(), Box<dyn Error>> {
    let mut cmd = command("cargo");
    cmd.args(["build", "--locked", "-p", "liberado-coder-runner"])
        .current_dir(&layout.worktree)
        .env("CARGO_TARGET_DIR", &layout.target_dir);
    let output = run_async_command(
        &mut cmd,
        "cargo build --locked -p liberado-coder-runner",
        Duration::from_secs(manifest.compile_timeout_secs),
    );
    record_runner_build(layout, output)
}

fn record_runner_build(
    layout: &HarnessLayout,
    output: Result<std::process::Output, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    match output {
        Ok(output) => record_runner_build_output(layout, output),
        Err(error) => {
            fs::write(
                layout.artifacts.join("runner-build.stderr.log"),
                format!("{error}\n"),
            )?;
            Err(format!("Liberado runner build failed: {error}").into())
        }
    }
}

fn record_runner_build_output(
    layout: &HarnessLayout,
    output: std::process::Output,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        layout.artifacts.join("runner-build.stdout.log"),
        &output.stdout,
    )?;
    fs::write(
        layout.artifacts.join("runner-build.stderr.log"),
        &output.stderr,
    )?;
    if !output.status.success() {
        return Err(format!("Liberado runner build failed with {}", output.status).into());
    }
    Ok(())
}

pub(super) fn require_runner(binary: PathBuf, message: &str) -> Result<PathBuf, Box<dyn Error>> {
    if binary.is_file() {
        Ok(binary)
    } else {
        Err(format!("{message}: {}", binary.display()).into())
    }
}
