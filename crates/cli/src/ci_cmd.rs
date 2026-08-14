//! Repository checks that used to live in shell-specific preflight scripts.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use liberado_common::process::std_command;

/// Run the repository's local ship preflight.
///
/// The command list is deliberately kept here, rather than in a shell script, so the same
/// preflight works through the native `liberado` binary on every host OS.
pub fn check() -> Result<(), Box<dyn std::error::Error>> {
    let root = repository_root()?;
    let commands: [(&str, &[&str]); 4] = [
        ("cargo", &["fmt", "--check"]),
        (
            "cargo",
            &[
                "clippy",
                "--workspace",
                "--exclude",
                "liberado-webui",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        ("cargo", &["test", "--workspace", "--no-fail-fast"]),
        ("cargo", &["deny", "check"]),
    ];

    for (program, args) in commands {
        run(&root, program, args)?;
    }

    Ok(())
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[liberado ci] {} {}", program, args.join(" "));
    let status = std_command(program)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| {
            io::Error::new(error.kind(), format!("could not start {program}: {error}"))
        })?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} failed with {status}", args.join(" ")).into())
    }
}

fn repository_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut candidate = std::env::current_dir()?;
    loop {
        if candidate.join("Cargo.toml").is_file() && candidate.join("crates").is_dir() {
            return Ok(candidate);
        }
        if !candidate.pop() {
            break;
        }
    }

    Err("liberado ci check must run inside a Liberado repository".into())
}

#[cfg(test)]
mod tests {
    use super::repository_root;

    #[test]
    fn finds_the_workspace_from_the_checkout_root() {
        let root = repository_root().expect("test runs from the workspace");
        assert!(root.join("Cargo.toml").is_file());
        assert!(root.join("crates").is_dir());
    }
}
