//! `--help` must print the usage text and exit cleanly.

use std::process::Command;

#[test]
fn help_flag_prints_usage_and_exits_cleanly() {
    let output = Command::new(env!("CARGO_BIN_EXE_liberado-sysmap"))
        .arg("--help")
        .output()
        .expect("spawn liberado-sysmap --help");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf-8 help text");
    assert!(stdout.contains("USAGE"), "{stdout}");
    assert!(stdout.contains("--write-json"), "{stdout}");
    assert!(stdout.contains("--config-dir"), "{stdout}");
}
