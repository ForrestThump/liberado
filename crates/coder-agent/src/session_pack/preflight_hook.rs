//! Ship preflight before terminal `Succeeded` for coding goals that claim ship/PR readiness.
//!
//! Spec comes from goal payload (server may inject topology steps) or built-in liberado defaults.
//! Language-agnostic runner lives in `liberado_coder_sandbox::preflight`.

use liberado_coder_sandbox::{
    PreflightReport, PreflightSpec, PreflightStep, liberado_ship_preflight_spec, resolve_ship_spec,
    run_preflight,
};
use liberado_session::{GoalSpec, SessionEvent, SessionEventKind};
use serde_json::Value;
use std::path::Path;
use tokio::sync::mpsc::Sender;

/// Whether this goal must pass ship preflight before `Succeeded`.
///
/// Required when:
/// - `payload.skip_preflight` is not true, and
/// - (`payload.preflight.required` is true, or project is set / steps present, or profile ship)
pub fn ship_preflight_required(goal: &GoalSpec) -> bool {
    ship_preflight_required_for(&goal.payload)
}

/// [`ship_preflight_required`] against a bare payload.
///
/// Split out because the ACP bridge dispatches coding runs without ever building a [`GoalSpec`],
/// and for its whole life that meant it skipped this decision entirely rather than answering it
/// differently. One function, so a run started from Paseo and a run started from the HTTP API
/// cannot end up held to different bars.
pub fn ship_preflight_required_for(payload: &Value) -> bool {
    if payload.get("skip_preflight").and_then(|v| v.as_bool()) == Some(true) {
        return false;
    }
    // Explore / plan are research or planning — not ship/PR outcomes by default.
    if payload.get("explore_mode").and_then(|v| v.as_bool()) == Some(true)
        || payload.get("plan_mode").and_then(|v| v.as_bool()) == Some(true)
        || payload
            .get("mode")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.eq_ignore_ascii_case("explore") || m.eq_ignore_ascii_case("plan"))
    {
        return false;
    }
    if payload
        .get("preflight")
        .and_then(|v| v.get("required"))
        .and_then(|v| v.as_bool())
        == Some(false)
    {
        return false;
    }
    // Explicit required, or any steps, or a coding project name (liberado defaults apply).
    if payload
        .get("preflight")
        .and_then(|v| v.get("required"))
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        return true;
    }
    if payload
        .get("preflight")
        .and_then(|v| v.get("steps"))
        .is_some()
    {
        return true;
    }
    if payload
        .get("project")
        .and_then(|v| v.as_str())
        .is_some_and(|p| !p.is_empty())
    {
        return true;
    }
    false
}

/// Build a ship [`PreflightSpec`] from payload, or liberado defaults, or `None` if nothing applies.
pub fn ship_spec_from_goal(goal: &GoalSpec) -> Option<PreflightSpec> {
    ship_spec_for(&goal.payload)
}

/// [`ship_spec_from_goal`] against a bare payload — see [`ship_preflight_required_for`].
pub fn ship_spec_for(payload: &Value) -> Option<PreflightSpec> {
    let project = payload
        .get("project")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(steps) = steps_from_payload(payload)
        && !steps.is_empty()
    {
        let id = payload
            .get("preflight")
            .and_then(|v| v.get("profile"))
            .and_then(|v| v.as_str())
            .unwrap_or("ship")
            .to_string();
        return Some(PreflightSpec::new(id, steps));
    }

    // profile: "ship" with no steps → liberado defaults when project is liberado
    resolve_ship_spec(project, None).or_else(|| {
        if payload
            .get("preflight")
            .and_then(|v| v.get("profile"))
            .and_then(|v| v.as_str())
            .is_some_and(|p| p.eq_ignore_ascii_case("ship"))
        {
            Some(liberado_ship_preflight_spec())
        } else {
            None
        }
    })
}

fn steps_from_payload(payload: &Value) -> Option<Vec<PreflightStep>> {
    let steps = payload.get("preflight")?.get("steps")?.as_array()?;
    let mut out = Vec::new();
    for s in steps {
        let name = s.get("name")?.as_str()?.to_string();
        let run = s.get("run")?.as_str()?.to_string();
        if name.is_empty() || run.is_empty() {
            continue;
        }
        let mut step = PreflightStep::new(name, run);
        if let Some(t) = s.get("timeout_secs").and_then(|v| v.as_u64()) {
            step.timeout_secs = Some(t);
        }
        if let Some(r) = s.get("required").and_then(|v| v.as_bool()) {
            step.required = r;
        }
        out.push(step);
    }
    Some(out)
}

/// Where preflight baselines are cached, keyed by base commit.
///
/// Beside the coding worktrees rather than inside a session's own tree, so every session sharing
/// a base pays for the baseline once instead of once each.
fn baseline_cache_dir() -> std::path::PathBuf {
    let data = std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into());
    std::path::PathBuf::from(data).join("preflight-baselines")
}

/// The commit this line of work started from — what "already broken" is measured against.
///
/// `merge-base` against the default branch, not `HEAD`: the question is what was failing before
/// this branch existed, and `HEAD` would move with every commit the agent makes.
async fn base_commit(workspace: &Path) -> Option<String> {
    for base in ["origin/main", "main", "origin/develop", "develop"] {
        if let Ok(sha) =
            liberado_coder_sandbox::run_git(workspace, &["merge-base", "HEAD", base]).await
            && !sha.trim().is_empty()
        {
            return Some(sha.trim().to_string());
        }
    }
    None
}

/// Run ship preflight under `workspace`, emit progress + validation events, return the report.
///
/// A failing run is compared against the base commit before it is believed — see the body.
pub async fn run_ship_preflight(
    session_id: &str,
    workspace: &Path,
    spec: &PreflightSpec,
    events: &Sender<SessionEvent>,
) -> Result<PreflightReport, String> {
    let _ = events
        .send(SessionEvent::new(
            session_id,
            SessionEventKind::Progress {
                message: format!(
                    "ship preflight '{}': {} step(s) under {}",
                    spec.id,
                    spec.steps.len(),
                    workspace.display()
                ),
            },
        ))
        .await;

    let mut report = run_preflight(workspace, spec)
        .await
        .map_err(|e| e.to_string())?;

    // Only a failing run needs a baseline, so a clean one costs exactly what it always did.
    //
    // Demanding absolute green conflates "you broke it" with "it was already broken", and since
    // this gate sits before terminal `Succeeded`, the second case traps the agent: it spends its
    // whole attempt budget on a failure it did not cause and cannot fix. Compare instead.
    if !report.ok {
        let current = liberado_coder_sandbox::report_failures(&report);
        let failing_steps: std::collections::BTreeSet<String> = current.keys().cloned().collect();

        match base_commit(workspace).await {
            Some(base_sha) => {
                let cache_dir = baseline_cache_dir();
                let target_dir = workspace.join("target");
                let opts = liberado_coder_sandbox::BaselineOptions {
                    project_root: workspace,
                    base_sha: &base_sha,
                    cache_dir: &cache_dir,
                    target_dir: Some(&target_dir),
                };
                match liberado_coder_sandbox::compute_baseline(&opts, spec, &failing_steps).await {
                    Ok(baseline) => {
                        let new =
                            liberado_coder_sandbox::diff_against_baseline(&current, &baseline);
                        let preexisting = liberado_coder_sandbox::describe_failures(&current).len()
                            - liberado_coder_sandbox::describe_failures(&new).len();
                        if new.is_empty() {
                            report.ok = true;
                            report.summary = format!(
                                "preflight '{}': no new failures ({preexisting} pre-existing at \
                                 {}, ignored)",
                                spec.id,
                                &base_sha[..12.min(base_sha.len())]
                            );
                        } else {
                            report.summary = format!(
                                "preflight '{}': {} new failure(s) vs {}: {}",
                                spec.id,
                                liberado_coder_sandbox::describe_failures(&new).len(),
                                &base_sha[..12.min(base_sha.len())],
                                liberado_coder_sandbox::describe_failures(&new).join(", ")
                            );
                        }
                    }
                    // A baseline we could not compute is not evidence of innocence — stay
                    // fail-closed and say why, rather than waving the run through.
                    Err(e) => {
                        report.summary = format!("{} (baseline unavailable: {e})", report.summary);
                    }
                }
            }
            None => {
                report.summary = format!(
                    "{} (no base commit found; cannot tell new failures from pre-existing)",
                    report.summary
                );
            }
        }
    }

    let _ = events
        .send(SessionEvent::new(
            session_id,
            SessionEventKind::ValidationFinished {
                ok: report.ok,
                summary: report.summary.clone(),
            },
        ))
        .await;

    if !report.ok {
        let detail = report
            .steps
            .iter()
            .filter(|s| !s.ok)
            .map(|s| {
                format!(
                    "{}: exit={:?} timeout={} log={}",
                    s.name,
                    s.exit_code,
                    s.timed_out,
                    s.log_excerpt.chars().take(400).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::Progress {
                    message: format!("ship preflight failed: {detail}"),
                },
            ))
            .await;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn goal_with(payload: Value) -> GoalSpec {
        GoalSpec {
            id: Some("t".into()),
            domain: liberado_session::DomainHint::Coding,
            description: "x".into(),
            success_criteria: vec![],
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload,
        }
    }

    #[test]
    fn skip_preflight_disables_requirement() {
        let g = goal_with(json!({
            "project": "liberado",
            "skip_preflight": true
        }));
        assert!(!ship_preflight_required(&g));
    }

    #[test]
    fn project_liberado_requires_ship_preflight() {
        let g = goal_with(json!({ "project": "liberado" }));
        assert!(ship_preflight_required(&g));
        let spec = ship_spec_from_goal(&g).unwrap();
        assert!(spec.steps.iter().any(|s| s.name == "test"));
    }

    #[test]
    fn explore_mode_skips_ship_preflight() {
        let g = goal_with(json!({
            "project": "liberado",
            "explore_mode": true
        }));
        assert!(!ship_preflight_required(&g));
    }

    #[test]
    fn payload_steps_override_defaults() {
        let g = goal_with(json!({
            "project": "liberado",
            "preflight": {
                "required": true,
                "steps": [
                    { "name": "only", "run": "echo hi" }
                ]
            }
        }));
        let spec = ship_spec_from_goal(&g).unwrap();
        assert_eq!(spec.steps.len(), 1);
        assert_eq!(spec.steps[0].name, "only");
    }

    #[test]
    fn no_project_no_steps_not_required() {
        let g = goal_with(json!({}));
        assert!(!ship_preflight_required(&g));
        assert!(ship_spec_from_goal(&g).is_none());
    }

    #[test]
    fn payload_steps_skip_empty_name_or_run() {
        // An entry missing "name" causes steps_from_payload to propagate None for the
        // whole array, so the function falls through to resolve_ship_spec for project.
        let g = goal_with(json!({
            "project": "liberado",
            "preflight": {
                "steps": [
                    { "name": "good", "run": "echo y" }
                ]
            }
        }));
        let spec = ship_spec_from_goal(&g).unwrap();
        assert_eq!(spec.steps.len(), 1);
        assert_eq!(spec.steps[0].name, "good");
    }

    #[test]
    fn payload_steps_with_empty_name_field_skips_whole_array() {
        // steps_from_payload uses `?` on each field get, so a missing/bad field
        // causes the whole function to return None, falling back to resolve_ship_spec.
        let g = goal_with(json!({
            "project": "liberado",
            "preflight": {
                "steps": [
                    { "name": "", "run": "echo x" }
                ]
            }
        }));
        // Empty name passes s.get("name")?.as_str()? (returns empty string "")
        // but then name.is_empty() skips it, leaving an empty vec,
        // so ship_spec_from_goal falls through to liberado default.
        let spec = ship_spec_from_goal(&g).unwrap();
        assert!(
            spec.steps.iter().any(|s| s.name == "test"),
            "should fall back to liberado default when payload steps all skip"
        );
    }

    #[test]
    fn payload_steps_empty_array_falls_back_to_default() {
        let g = goal_with(json!({
            "project": "liberado",
            "preflight": { "steps": [] }
        }));
        let spec = ship_spec_from_goal(&g).unwrap();
        assert!(
            spec.steps.iter().any(|s| s.name == "test"),
            "empty payload steps should fall back to liberado default"
        );
    }

    #[test]
    fn payload_steps_missing_top_level_fields_returns_none() {
        // Entry missing "name" causes steps_from_payload to return None via ?.
        let g = goal_with(json!({
            "project": "liberado",
            "preflight": {
                "steps": [
                    { "run": "echo x" }
                ]
            }
        }));
        // steps_from_payload returns None (name missing), falls through to default.
        let spec = ship_spec_from_goal(&g).unwrap();
        assert!(spec.steps.iter().any(|s| s.name == "test"));
    }

    #[test]
    fn payload_preflight_no_steps_falls_back_for_liberado() {
        let g = goal_with(json!({
            "project": "liberado",
            "preflight": { "required": false }
        }));
        // No steps in payload → falls through → default for liberado project.
        let spec = ship_spec_from_goal(&g).unwrap();
        assert!(spec.steps.iter().any(|s| s.name == "test"));
    }
}
