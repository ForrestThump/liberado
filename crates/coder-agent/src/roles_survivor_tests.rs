//! Split from `roles.rs`: kills the baseline campaign's survivors.
//!
//! Covers repair-vs-coder role selection boundaries, empty-prompt-file fallback,
//! goal assembly (criteria, prior feedback, attempt routing), and truncation.

use super::*;
use liberado_coder_core::{CoderRunRequest, CoderTask};

fn request_from(json: serde_json::Value) -> CoderRunRequest {
    serde_json::from_value(json).expect("request json")
}

fn base_request() -> CoderRunRequest {
    request_from(serde_json::json!({
        "task": {"id": "t", "description": "do the thing"},
        "workspace": {"root": "/tmp/ws", "base_ref": "main"},
        "config": {
            "backend": "loop",
            "planner": {"model": "p"},
            "coder": {"model": "coder-model"},
            "critic": {"model": "critic-model"}
        }
    }))
}

#[test]
fn first_attempts_use_the_coder_role_even_with_repair_configured() {
    let mut request = base_request();
    request.config.repair = Some(liberado_coder_core::CoderRoleConfig {
        model: "repair-model".into(),
        ..Default::default()
    });
    assert_eq!(worker_role_name(&request), "coder");
    assert_eq!(worker_role_config(&request).model, "coder-model");
}

#[test]
fn later_attempts_without_repair_stay_on_the_coder_role() {
    let mut request = base_request();
    request.attempt = 1;
    assert_eq!(worker_role_name(&request), "coder");
    assert_eq!(worker_role_config(&request).model, "coder-model");
}

#[test]
fn repair_attempts_use_the_repair_role() {
    let mut request = base_request();
    request.attempt = 1;
    request.config.repair = Some(liberado_coder_core::CoderRoleConfig {
        model: "repair-model".into(),
        ..Default::default()
    });
    assert_eq!(worker_role_name(&request), "repair");
    assert_eq!(worker_role_config(&request).model, "repair-model");
}

/// An override file that exists but holds only whitespace is "missing" in every
/// sense that matters: the built-in prompt must take over.
#[tokio::test]
async fn a_whitespace_prompt_file_falls_back_to_the_baked_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("blank.md");
    std::fs::write(&path, "   \n\t\n").expect("write blank prompt");
    let resolved = role_instructions(
        &liberado_coder_core::CoderRoleConfig {
            prompt_path: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        },
        "coder",
    )
    .await
    .expect("blank file must fall back, not fail");
    assert_eq!(resolved, liberado_coder_core::prompts::CODER);
}

#[test]
fn success_criteria_render_under_their_own_heading() {
    let mut request = base_request();
    request.task.success_criteria = vec!["criterion-one".into(), "criterion-two".into()];
    let goal = coder_goal(&request);
    assert!(goal.contains("Success criteria:"), "{goal}");
    assert!(goal.contains("- criterion-one\n"), "{goal}");
    assert!(goal.contains("- criterion-two"), "{goal}");
}

#[test]
fn no_criteria_means_no_criteria_section() {
    let goal = coder_goal(&base_request());
    assert!(!goal.contains("Success criteria"), "{goal}");
}

#[test]
fn context_is_appended_when_present() {
    let mut request = base_request();
    request.task = CoderTask::new("t", "desc").with_context("the vault layout");
    let goal = coder_goal(&request);
    assert!(
        goal.starts_with("Task: desc\n\nContext:\nthe vault layout"),
        "{goal}"
    );
}

#[test]
fn attempt_zero_lists_prior_feedback_verbatim() {
    let mut request = base_request();
    request.prior_feedback = vec!["FEEDBACK-ONE".into(), "FEEDBACK-TWO".into()];
    let goal = coder_goal(&request);
    assert!(goal.contains("- FEEDBACK-ONE"), "{goal}");
    assert!(goal.contains("- FEEDBACK-TWO"), "{goal}");
    assert!(!goal.contains("REPAIR attempt"), "{goal}");
}

#[test]
fn empty_prior_feedback_prints_no_feedback_section() {
    let goal = coder_goal(&base_request());
    assert!(!goal.contains("Prior feedback"), "{goal}");
}

#[test]
fn repair_attempts_get_the_focus_block_not_a_raw_list() {
    let mut request = base_request();
    request.attempt = 1;
    request.prior_feedback = vec!["FEEDBACK-ONE".into()];
    let goal = coder_goal(&request);
    assert!(goal.contains("REPAIR attempt"), "{goal}");
    assert!(
        !goal.contains("- FEEDBACK-ONE"),
        "attempt>0 routes through repair_focus_block: {goal}"
    );
}

#[test]
fn truncate_chars_keeps_short_values_whole() {
    assert_eq!(truncate_chars("abcde", 5), "abcde");
    assert_eq!(truncate_chars("abc", 5), "abc");
}

#[test]
fn truncate_chars_marks_the_cut_and_counts_chars_not_bytes() {
    let out = truncate_chars("abcdefgh", 3);
    assert_eq!(out, "abc\n…[truncated]");
    // é is two bytes but one char.
    let out = truncate_chars("éééé", 2);
    assert!(out.starts_with("éé\n"), "{out}");
}
