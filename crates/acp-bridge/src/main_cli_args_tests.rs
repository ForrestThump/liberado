//! CLI argument handling tests (moved verbatim from main.rs).

#![allow(unused_imports)]

use super::*;
use crate::provider::catalog_model_ids;
use liberado_provider::MockProvider;
use tempfile::TempDir;

use super::test_support::*;
#[test]
fn version_flag_exits_without_stdio_loop() {
    assert_eq!(handle_cli_args(["--version"]), Some(0));
    assert_eq!(handle_cli_args(["-V"]), Some(0));
    assert_eq!(handle_cli_args(["version"]), Some(0));
}

#[test]
fn help_flag_exits_without_stdio_loop() {
    assert_eq!(handle_cli_args(["--help"]), Some(0));
    assert_eq!(handle_cli_args(["-h"]), Some(0));
}

#[test]
fn no_args_enters_acp_mode() {
    assert_eq!(handle_cli_args(Vec::<String>::new()), None);
}

#[test]
fn unknown_flag_is_an_error_exit() {
    assert_eq!(handle_cli_args(["--nope"]), Some(2));
}

#[test]
fn mode_flag_continues_into_acp_loop() {
    // `--mode` sets default and continues (does not exit).
    assert_eq!(handle_cli_args(["--mode", "chat"]), None);
    assert_eq!(handle_cli_args(["--mode=face"]), None);
    assert_eq!(handle_cli_args(["-m", "coding"]), None);
    assert_eq!(handle_cli_args(["--mode", "goal"]), None);
    assert_eq!(handle_cli_args(["--mode=goal"]), None);
}

#[test]
fn unknown_mode_is_an_error_exit() {
    assert_eq!(handle_cli_args(["--mode", "banana"]), Some(2));
    assert_eq!(handle_cli_args(["--mode=nope"]), Some(2));
}

#[test]
fn positional_args_are_ignored_not_errors() {
    // A bare word is not a flag: it must neither exit nor trip the `--mode=` /
    // unknown-option arms. Guards that widen to `true` turn this into exit 2.
    assert_eq!(handle_cli_args(["extra"]), None);
    assert_eq!(handle_cli_args(["one", "two", "three"]), None);
    // A real flag after positionals is still validated.
    assert_eq!(handle_cli_args(["positional", "--later-flag"]), Some(2));
}

#[test]
fn apply_workspace_targets_sets_trimmed_and_ignores_blank() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var("CARGO_TARGET_DIR").ok();
    let cwd = std::env::current_dir().expect("cwd");

    crate::workspace_targets::apply_workspace_targets(
        &liberado_coder_core::WorkspaceBuildConfig::default(),
        &cwd,
    );
    assert_eq!(
        std::env::var("CARGO_TARGET_DIR").ok(),
        saved,
        "no setting must leave the environment alone"
    );
    crate::workspace_targets::apply_workspace_targets(
        &liberado_coder_core::WorkspaceBuildConfig {
            shared_target_dir: Some("   ".into()),
            ..Default::default()
        },
        &cwd,
    );
    assert_eq!(
        std::env::var("CARGO_TARGET_DIR").ok(),
        saved,
        "a whitespace-only setting must be ignored, not blank out the cache"
    );
    crate::workspace_targets::apply_workspace_targets(
        &liberado_coder_core::WorkspaceBuildConfig {
            shared_target_dir: Some("  target/shared  ".into()),
            ..Default::default()
        },
        &cwd,
    );
    assert_eq!(
        std::env::var("CARGO_TARGET_DIR").as_deref(),
        Ok("target/shared"),
        "a real setting must reach children trimmed"
    );

    match saved {
        Some(v) => unsafe { std::env::set_var("CARGO_TARGET_DIR", v) },
        None => unsafe { std::env::remove_var("CARGO_TARGET_DIR") },
    }
}
