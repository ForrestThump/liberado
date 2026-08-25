//! Build fingerprint: crate version + `git describe` captured at compile time.
//!
//! `/health` reports it so the delegator's supervisor can log mismatches loudly — a
//! dispatched run tests the installed binary, not your working tree, and stale-binary
//! debugging once cost a whole session because nothing said so.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let describe = std::process::Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=LIBERADO_BUILD_FINGERPRINT={describe}");
}
