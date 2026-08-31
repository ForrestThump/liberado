//! Locate the pinned rustup LLVM tools when another Rust installation shadows rustup.

use liberado_common::process::std_command;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::CiLog;

pub(super) fn spawn_to_log(
    log: &CiLog,
    program: &OsStr,
    args: &[&str],
) -> io::Result<std::process::ExitStatus> {
    let stdout = std::fs::OpenOptions::new().append(true).open(&log.path)?;
    let stderr = std::fs::OpenOptions::new().append(true).open(&log.path)?;
    let mut command = std_command(program);
    configure(&mut command, &log.root, args)?;
    command
        .args(args)
        .current_dir(&log.root)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .env("CARGO_TERM_COLOR", "never")
        .status()
}

pub(super) fn configure(command: &mut Command, root: &Path, args: &[&str]) -> io::Result<()> {
    if args.first() != Some(&"llvm-cov") {
        return Ok(());
    }
    if let Some(tools) = tools_from_rustc(root, None)? {
        set_environment(command, &tools);
        return Ok(());
    }
    let toolchain = active_rustup_toolchain(root)?;
    let tools = tools_from_rustc(root, Some(&toolchain))?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "the pinned rustup toolchain has no llvm-tools-preview component",
        )
    })?;
    set_environment(command, &tools);
    Ok(())
}

struct LlvmTools {
    cov: PathBuf,
    profdata: PathBuf,
}

fn set_environment(command: &mut Command, tools: &LlvmTools) {
    command.env("LLVM_COV", &tools.cov);
    command.env("LLVM_PROFDATA", &tools.profdata);
}

fn tools_from_rustc(root: &Path, toolchain: Option<&str>) -> io::Result<Option<LlvmTools>> {
    let mut command = match toolchain {
        Some(name) => {
            let mut command = std_command("rustup");
            command.args(["run", name, "rustc"]);
            command
        }
        None => std_command("rustc"),
    };
    let output = command
        .args(["--print", "target-libdir"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(tools_beside_target_libdir(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

fn active_rustup_toolchain(root: &Path) -> io::Result<String> {
    let output = std_command("rustup")
        .args(["show", "active-toolchain"])
        .current_dir(root)
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "could not resolve the pinned rustup toolchain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    text.split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| io::Error::other("rustup returned no active toolchain"))
}

fn tools_beside_target_libdir(target_libdir: &str) -> Option<LlvmTools> {
    let bin = Path::new(target_libdir).parent()?.join("bin");
    let cov = executable(&bin, "llvm-cov");
    let profdata = executable(&bin, "llvm-profdata");
    (cov.is_file() && profdata.is_file()).then_some(LlvmTools { cov, profdata })
}

fn executable(directory: &Path, name: &str) -> PathBuf {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    directory.join(OsStr::new(&file))
}

#[cfg(test)]
mod tests {
    use super::tools_beside_target_libdir;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn llvm_tools_are_resolved_beside_the_target_library_directory() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("lib/rustlib/host");
        let lib = target.join("lib");
        let bin = target.join("bin");
        fs::create_dir_all(&lib).unwrap();
        fs::create_dir_all(&bin).unwrap();
        fs::write(
            bin.join(format!("llvm-cov{}", std::env::consts::EXE_SUFFIX)),
            "",
        )
        .unwrap();
        fs::write(
            bin.join(format!("llvm-profdata{}", std::env::consts::EXE_SUFFIX)),
            "",
        )
        .unwrap();

        let tools = tools_beside_target_libdir(lib.to_str().unwrap()).unwrap();
        assert!(tools.cov.starts_with(&bin));
        assert!(tools.profdata.starts_with(&bin));
    }
}
