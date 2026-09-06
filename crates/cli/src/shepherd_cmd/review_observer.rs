//! Read-only diagnostic for the daemon review observer.

use super::*;
use liberado_coder_core::pr_review::{
    CheckConclusion, PullRequestSnapshot, ReviewCycle, ReviewPolicy, ShaChecks, WakeSignal,
    check_endpoints, observe, review_eligible, valid_full_sha,
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
    if !valid_full_sha(requested_sha) {
        return Err("--sha must be a full 40-character hexadecimal SHA".into());
    }
    let topology = load_shepherd_topology()?;
    validate_shepherd_topology(&topology)?;
    let project = select_shepherd_project(Some(project_name), &topology.shepherd.projects)?
        .ok_or("missing shepherd project")?;
    let pr_value = api(
        &project.repository,
        &format!("/repos/{}/pulls/{number}", project.repository),
    )?;
    let head_sha = pr_value
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let head_repo = pr_value
        .pointer("/head/repo/full_name")
        .and_then(Value::as_str);
    let base_repo = pr_value
        .pointer("/base/repo/full_name")
        .and_then(Value::as_str);
    let pr = PullRequestSnapshot {
        repository: project.repository.clone(),
        number,
        head_sha: head_sha.clone(),
        open: pr_value["state"] == "open",
        draft: pr_value["draft"].as_bool().unwrap_or(true),
        fork_head: head_repo != base_repo,
    };
    let mut observed = Vec::new();
    for endpoint in check_endpoints(&project.repository, requested_sha) {
        collect_checks(&api(&project.repository, &endpoint)?, &mut observed);
    }
    let policy = ReviewPolicy {
        controller: project.controller.clone().unwrap_or_default(),
        check_names: project.check_names.clone(),
        shadow: true,
    };
    let cycle = ReviewCycle {
        armed_sha: Some(requested_sha.to_string()),
        accepted_sha: None,
    };
    let checks = ShaChecks {
        sha: requested_sha.to_string(),
        checks: observed,
    };
    let eligible = review_eligible(&policy, &pr, &cycle, &checks);
    let intents = observe(
        &policy,
        &pr,
        &ReviewCycle {
            armed_sha: None,
            accepted_sha: None,
        },
        &checks,
        WakeSignal::ReadyForReview {
            sha: requested_sha.to_string(),
        },
    );
    let ledger_pr = Pr {
        number,
        title: String::new(),
        branch: String::new(),
        base_sha: String::new(),
        head_sha: head_sha.clone(),
        url: String::new(),
        labels: Vec::new(),
    };
    let cfg = Config::load(Some(project_name))?;
    let _ = record::record_observer_intents(&cfg, &ledger_pr, true, &intents)?;
    println!(
        "{}",
        json!({"repository":project.repository,"pr":number,"requested_sha":requested_sha,"observed_sha":head_sha,"eligible":eligible,"writes":false,"lease":false})
    );
    Ok(())
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

fn collect_checks(value: &Value, target: &mut Vec<(String, CheckConclusion)>) {
    let rows = value["check_runs"]
        .as_array()
        .or_else(|| value["statuses"].as_array());
    for row in rows.into_iter().flatten() {
        let Some(name) = row["name"].as_str().or_else(|| row["context"].as_str()) else {
            continue;
        };
        let raw = row["conclusion"]
            .as_str()
            .or_else(|| row["state"].as_str())
            .unwrap_or("");
        let conclusion = if raw.eq_ignore_ascii_case("success") {
            CheckConclusion::Success
        } else if matches!(raw, "pending" | "queued" | "in_progress") {
            CheckConclusion::Pending
        } else {
            CheckConclusion::Failure
        };
        target.push((name.into(), conclusion));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn check_parser_accepts_only_success() {
        let mut checks = Vec::new();
        collect_checks(
            &json!({"check_runs":[{"name":"CI","conclusion":"success"},{"name":"skip","conclusion":"neutral"}]}),
            &mut checks,
        );
        assert_eq!(
            checks,
            [
                ("CI".into(), CheckConclusion::Success),
                ("skip".into(), CheckConclusion::Failure)
            ]
        );
    }
}
