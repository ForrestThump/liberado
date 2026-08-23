//! Split from `merge.rs` for module-health boundaries.

use super::*;

#[test]
fn safe_name_rejects_empty() {
    assert!(validate_safe_name("", "name").is_err());
}

#[test]
fn safe_name_rejects_dot_dot() {
    assert!(validate_safe_name("..", "name").is_err());
    assert!(validate_safe_name("a../b", "name").is_err());
}

#[test]
fn safe_name_rejects_slash() {
    assert!(validate_safe_name("a/b", "name").is_err());
}

#[test]
fn safe_name_rejects_backslash() {
    assert!(validate_safe_name("a\\b", "name").is_err());
}

#[test]
fn safe_name_rejects_dash_prefix() {
    assert!(validate_safe_name("-bad", "name").is_err());
}

#[test]
fn safe_name_accepts_valid() {
    assert!(validate_safe_name("child-1", "name").is_ok());
    assert!(validate_safe_name("task_api", "name").is_ok());
}

#[test]
fn branch_name_rejects_empty() {
    assert!(validate_branch_name("").is_err());
}

#[test]
fn branch_name_rejects_dot_dot() {
    assert!(validate_branch_name("..").is_err());
}

#[test]
fn branch_name_rejects_backslash() {
    assert!(validate_branch_name("a\\b").is_err());
}

#[test]
fn branch_name_rejects_dash_prefix() {
    assert!(validate_branch_name("-evil").is_err());
}

#[test]
fn branch_name_rejects_space() {
    assert!(validate_branch_name("bad name").is_err());
}

#[test]
fn branch_name_rejects_empty_segment() {
    assert!(validate_branch_name("fanout//child").is_err());
    assert!(validate_branch_name("/child").is_err());
    assert!(validate_branch_name("fanout/").is_err());
}

#[test]
fn branch_name_accepts_slash_delimited_paths() {
    assert!(validate_branch_name("fanout/child").is_ok());
    assert!(validate_branch_name("fanout/child/api").is_ok());
}
