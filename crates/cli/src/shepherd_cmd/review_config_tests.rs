//! Native-review config and invocation tests, split for module-health.

use super::*;
use std::path::PathBuf;

fn valid_project(name: &str) -> liberado_config::ShepherdProjectConfig {
    liberado_config::ShepherdProjectConfig {
        name: name.into(),
        repository: "owner/repo".into(),
        coding_project: "liberado".into(),
        base_branch: "main".into(),
        profile: "coding-unattended".into(),
        check_names: vec!["test".into()],
        max_kickbacks: None,
        cold_reviews: None,
        cold_review_max_turns: None,
        max_concurrent_goals: None,
        poll_seconds: None,
        controller: None,
        review_profile: None,
        gate: None,
        auth: None,
    }
}

fn topology_with(project: liberado_config::ShepherdProjectConfig) -> liberado_config::Topology {
    let mut topology = liberado_config::Topology::default();
    topology.projects.push(liberado_config::ProjectConfig {
        name: project.coding_project.clone(),
        root: PathBuf::from("/tmp/project"),
        write_class: liberado_common::WriteClass::AgentWritable,
        enabled: true,
        preflight: Default::default(),
    });
    topology.shepherd.projects.push(project);
    topology
}

fn rejects(project: liberado_config::ShepherdProjectConfig, needle: &str) {
    let error = validate_shepherd_topology(&topology_with(project))
        .unwrap_err()
        .to_string();
    assert!(error.contains(needle), "expected {needle:?} in {error:?}");
}

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn daemon_review_requires_zero_cold_reviews_checks_and_auth_reference() {
    let mut project = valid_project("review");
    project.controller = Some("liberado-shepherd".into());
    project.review_profile = Some("coding-review-unattended".into());
    project.gate = Some("ready_and_green_tip".into());
    rejects(project.clone(), "cold_reviews = 0");
    project.cold_reviews = Some(0);
    rejects(project.clone(), "auth reference");
    project.auth = Some("github".into());
    let mut topology = topology_with(project);
    topology
        .shepherd
        .auth
        .push(liberado_config::ShepherdAuthConfig {
            name: "github".into(),
            kind: "token".into(),
            token_ref: "LIBERADO_GITHUB_TOKEN".into(),
            webhook_secret_ref: None,
            expected_login: None,
        });
    assert!(validate_shepherd_topology(&topology).is_ok());
}

#[test]
fn shepherd_auth_rejects_literal_secrets() {
    let mut topology = topology_with(valid_project("legacy"));
    topology
        .shepherd
        .auth
        .push(liberado_config::ShepherdAuthConfig {
            name: "bad".into(),
            kind: "token".into(),
            token_ref: "github_pat_literal".into(),
            webhook_secret_ref: None,
            expected_login: None,
        });
    assert!(
        validate_shepherd_topology(&topology)
            .unwrap_err()
            .to_string()
            .contains("never literal secrets")
    );
}

#[test]
fn review_invocation_is_dry_run_only() {
    let sha = "a".repeat(40);
    let parsed = parse_invocation(&argv(&[
        "review",
        "--project",
        "example",
        "--pr",
        "12",
        "--sha",
        &sha,
        "--dry-run",
    ]))
    .unwrap();
    assert_eq!(
        parsed.mode,
        Invocation::ReviewDryRun {
            project: "example".into(),
            pr: 12,
            sha: sha.clone()
        }
    );
    let error = parse_invocation(&argv(&[
        "review",
        "--project",
        "example",
        "--pr",
        "12",
        "--sha",
        &sha,
    ]))
    .unwrap_err();
    assert!(error.contains("dry-run"), "{error}");
}
