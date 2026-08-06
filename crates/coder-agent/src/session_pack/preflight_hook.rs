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
    if goal.payload.get("skip_preflight").and_then(|v| v.as_bool()) == Some(true) {
        return false;
    }
    // Explore / plan are research or planning — not ship/PR outcomes by default.
    if goal.payload.get("explore_mode").and_then(|v| v.as_bool()) == Some(true)
        || goal.payload.get("plan_mode").and_then(|v| v.as_bool()) == Some(true)
        || goal
            .payload
            .get("mode")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.eq_ignore_ascii_case("explore") || m.eq_ignore_ascii_case("plan"))
    {
        return false;
    }
    if goal.payload.get("preflight").and_then(|v| v.get("required")).and_then(|v| v.as_bool())
        == Some(false)
    {
        return false;
    }
    // Explicit required, or any steps, or a coding project name (liberado defaults apply).
    if goal
        .payload
        .get("preflight")
        .and_then(|v| v.get("required"))
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        return true;
    }
    if goal.payload.get("preflight").and_then(|v| v.get("steps")).is_some() {
        return true;
    }
    if goal
        .payload
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
    let project = goal
        .payload
        .get("project")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(steps) = steps_from_payload(&goal.payload) {
        if !steps.is_empty() {
            let id = goal
                .payload
                .get("preflight")
                .and_then(|v| v.get("profile"))
                .and_then(|v| v.as_str())
                .unwrap_or("ship")
                .to_string();
            return Some(PreflightSpec::new(id, steps));
        }
    }

    // profile: "ship" with no steps → liberado defaults when project is liberado
    resolve_ship_spec(project, None).or_else(|| {
        if goal
            .payload
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

/// Run ship preflight under `workspace`, emit progress + validation events, return the report.
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

    let report = run_preflight(workspace, spec)
        .await
        .map_err(|e| e.to_string())?;

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
}
