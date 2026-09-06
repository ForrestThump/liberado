//! Read-only diagnostic for the daemon review observer.

use super::*;
use liberado_coder_core::pr_review::{
    PullRequestSnapshot, ReviewCycle, ReviewPolicy, ShaChecks, WakeSignal, check_endpoints,
    collect_github_checks, observe, pull_request_snapshot, review_eligible, valid_full_sha,
};

pub(super) fn validate_review_config(
    topology: &liberado_config::Topology,
) -> Result<(), Box<dyn std::error::Error>> {
    let review = &topology.shepherd.review;
    if review.enabled
        && (review.delivery != "poll"
            || review.poll_seconds == 0
            || review.max_pages == 0
            || !(1..=100).contains(&review.page_size))
    {
        return Err("shepherd.review polling bounds are invalid".into());
    }
    let auth_names: BTreeSet<_> = topology
        .shepherd
        .auth
        .iter()
        .map(|auth| auth.name.as_str())
        .collect();
    if auth_names.len() != topology.shepherd.auth.len() {
        return Err("shepherd auth names must be non-empty and unique".into());
    }
    for auth in &topology.shepherd.auth {
        if auth.name.trim().is_empty()
            || auth.kind != "token"
            || !environment_reference(&auth.token_ref)
            || auth
                .webhook_secret_ref
                .as_deref()
                .is_some_and(|value| !environment_reference(value))
        {
            return Err(format!(
                "shepherd auth '{}' must contain environment references, never literal secrets",
                auth.name
            )
            .into());
        }
    }
    for project in &topology.shepherd.projects {
        if project.controller.as_deref() != Some("liberado-shepherd") {
            continue;
        }
        if project.cold_reviews != Some(0) {
            return Err(format!(
                "shepherd project '{}' must set cold_reviews = 0 for daemon review",
                project.name
            )
            .into());
        }
        if project.check_names.is_empty() {
            return Err(format!(
                "shepherd project '{}' check_names must be non-empty for daemon review",
                project.name
            )
            .into());
        }
        if project.review_profile.as_deref().is_none_or(str::is_empty)
            || project.gate.as_deref() != Some("ready_and_green_tip")
        {
            return Err(format!(
                "shepherd project '{}' requires review_profile and gate='ready_and_green_tip'",
                project.name
            )
            .into());
        }
        if project
            .auth
            .as_deref()
            .is_none_or(|name| !auth_names.contains(name))
        {
            return Err(format!(
                "shepherd project '{}' requires a declared auth reference",
                project.name
            )
            .into());
        }
    }
    Ok(())
}

fn environment_reference(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(super) fn dry_run(
    project_name: &str,
    number: u64,
    requested_sha: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", dry_run_report(project_name, number, requested_sha)?);
    Ok(())
}

fn dry_run_report(
    project_name: &str,
    number: u64,
    requested_sha: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let sha = require_full_sha(requested_sha)?;
    let loaded = load_dry_run(project_name, number, sha)?;
    render_dry_run(project_name, number, sha, loaded)
}

fn require_full_sha(requested_sha: &str) -> Result<&str, Box<dyn std::error::Error>> {
    valid_full_sha(requested_sha)
        .then_some(requested_sha)
        .ok_or_else(|| "--sha must be a full 40-character hexadecimal SHA".into())
}

struct DryRunLoad {
    project: liberado_config::ShepherdProjectConfig,
    pr: PullRequestSnapshot,
    checks: ShaChecks,
}

fn load_dry_run(
    project_name: &str,
    number: u64,
    requested_sha: &str,
) -> Result<DryRunLoad, Box<dyn std::error::Error>> {
    let project = load_dry_run_project(project_name)?;
    Ok(DryRunLoad {
        checks: load_sha_checks(&project.repository, requested_sha)?,
        pr: load_dry_run_pr(&project, number)?,
        project,
    })
}

fn load_dry_run_project(
    project_name: &str,
) -> Result<liberado_config::ShepherdProjectConfig, Box<dyn std::error::Error>> {
    selected_dry_run_project(project_name, &validated_topology()?)
}

fn validated_topology() -> Result<liberado_config::Topology, Box<dyn std::error::Error>> {
    let topology = load_shepherd_topology()?;
    validate_shepherd_topology(&topology)?;
    Ok(topology)
}

fn selected_dry_run_project(
    project_name: &str,
    topology: &liberado_config::Topology,
) -> Result<liberado_config::ShepherdProjectConfig, Box<dyn std::error::Error>> {
    select_shepherd_project(Some(project_name), &topology.shepherd.projects)?
        .cloned()
        .ok_or_else(|| "missing shepherd project".into())
}

fn load_dry_run_pr(
    project: &liberado_config::ShepherdProjectConfig,
    number: u64,
) -> Result<PullRequestSnapshot, Box<dyn std::error::Error>> {
    pull_request_snapshot(
        &project.repository,
        &api(
            &project.repository,
            &format!("/repos/{}/pulls/{number}", project.repository),
        )?,
    )
    .ok_or_else(|| "pull request snapshot is incomplete".into())
}

fn load_sha_checks(
    repository: &str,
    requested_sha: &str,
) -> Result<ShaChecks, Box<dyn std::error::Error>> {
    let mut observed = Vec::new();
    for endpoint in check_endpoints(repository, requested_sha) {
        observed.extend(collect_github_checks(&api(repository, &endpoint)?));
    }
    Ok(ShaChecks {
        sha: requested_sha.to_string(),
        checks: observed,
    })
}

fn render_dry_run(
    project_name: &str,
    number: u64,
    requested_sha: &str,
    loaded: DryRunLoad,
) -> Result<String, Box<dyn std::error::Error>> {
    let policy = ReviewPolicy {
        controller: loaded.project.controller.clone().unwrap_or_default(),
        check_names: loaded.project.check_names.clone(),
        shadow: true,
    };
    let cycle = ReviewCycle {
        armed_sha: Some(requested_sha.to_string()),
        accepted_sha: None,
    };
    let eligible = review_eligible(&policy, &loaded.pr, &cycle, &loaded.checks);
    let intents = observe(
        &policy,
        &loaded.pr,
        &ReviewCycle {
            armed_sha: None,
            accepted_sha: None,
        },
        &loaded.checks,
        WakeSignal::ReadyForReview {
            sha: requested_sha.to_string(),
        },
    );
    let ledger_pr = Pr {
        number,
        title: String::new(),
        branch: String::new(),
        base_sha: String::new(),
        head_sha: loaded.pr.head_sha.clone(),
        url: String::new(),
        labels: Vec::new(),
    };
    let cfg = Config::load(Some(project_name))?;
    let _ = record::record_observer_intents(&cfg, &ledger_pr, true, &intents)?;
    Ok(json!({
        "repository": loaded.project.repository,
        "pr": number,
        "requested_sha": requested_sha,
        "observed_sha": loaded.pr.head_sha,
        "eligible": eligible,
        "writes": false,
        "lease": false
    })
    .to_string())
}

fn api(_repository: &str, endpoint: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let output = std_command("gh").args(["api", endpoint]).output()?;
    if !output.status.success() {
        return Err(format!(
            "GitHub read failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::pr_review::CheckConclusion;
    use liberado_config::Topology;
    #[test]
    fn check_parser_accepts_only_success() {
        let checks = collect_github_checks(
            &json!({"check_runs":[{"name":"CI","conclusion":"success"},{"name":"skip","conclusion":"neutral"}]}),
        );
        assert_eq!(
            checks,
            [
                ("CI".into(), CheckConclusion::Success),
                ("skip".into(), CheckConclusion::Failure)
            ]
        );
    }

    #[test]
    fn review_path_does_not_call_old_check_helpers() {
        let src = include_str!("review_observer.rs");
        let old_status = format!("check_{}", "status(");
        assert!(!src.contains(&old_status));
        let old_helper = format!("pr {}", "checks");
        assert!(!src.contains(&old_helper));
        let identity = format!("{}Thump", "Forrest");
        assert!(!src.contains(&identity));
    }

    #[test]
    fn dry_run_rejects_a_short_sha_before_any_process() {
        let error = dry_run("example", 1, "abc").unwrap_err().to_string();
        assert!(
            error.contains("full 40-character hexadecimal SHA"),
            "{error}"
        );
        assert!(require_full_sha("abc").is_err());
        assert!(require_full_sha(&"a".repeat(40)).is_ok());
    }

    #[test]
    fn selected_dry_run_project_requires_a_named_row() {
        let topology = Topology::default();
        let error = selected_dry_run_project("example", &topology)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown shepherd project"), "{error}");
    }
}
