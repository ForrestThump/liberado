//! Assemble the harness list, run order, and job spec for submit/doctor.

use super::*;

pub(super) fn resolve_harness_ids(parsed: &SubmitArgs) -> Vec<String> {
    if let Some(ids) = &parsed.harnesses {
        return ids.clone();
    }
    if parsed.hermes_bin.is_some()
        || parsed.deep_agents_bin.is_some()
        || parsed.hermes_git_sha.is_some()
        || parsed.deep_agents_git_sha.is_some()
    {
        return default_four_way_run_order();
    }
    default_run_order()
}

pub(super) fn harness_requests(parsed: &SubmitArgs) -> Result<Vec<HarnessRequest>, Box<dyn Error>> {
    let ids = resolve_harness_ids(parsed);
    require_named_harness(
        parsed.hermes_bin.is_some() || parsed.hermes_git_sha.is_some(),
        &ids,
        "hermes",
        "--hermes-bin/--hermes-git-sha",
    )?;
    require_named_harness(
        parsed.deep_agents_bin.is_some() || parsed.deep_agents_git_sha.is_some(),
        &ids,
        "deepagents",
        "--deep-agents-bin/--deep-agents-git-sha",
    )?;
    Ok(ids.into_iter().map(|id| request_for(parsed, id)).collect())
}

fn require_named_harness(
    named: bool,
    ids: &[String],
    id: &str,
    flags: &str,
) -> Result<(), Box<dyn Error>> {
    if named && !ids.iter().any(|have| have == id) {
        return Err(format!("{flags} requires {id} in the harness list").into());
    }
    Ok(())
}

fn request_for(parsed: &SubmitArgs, id: String) -> HarnessRequest {
    HarnessRequest {
        binary: match id.as_str() {
            "liberado" => parsed.liberado_bin.clone(),
            "pi" => parsed.pi_bin.clone(),
            "hermes" => parsed.hermes_bin.clone(),
            "deepagents" => parsed.deep_agents_bin.clone(),
            _ => None,
        },
        git_sha: match id.as_str() {
            "hermes" => parsed.hermes_git_sha.clone(),
            "deepagents" => parsed.deep_agents_git_sha.clone(),
            _ => None,
        },
        id,
    }
}

pub(super) fn submit_experiment(parsed: &SubmitArgs) -> Result<Option<Experiment>, Box<dyn Error>> {
    match (&parsed.hypothesis, &parsed.variable) {
        (None, None) => Ok(None),
        (Some(hypothesis), Some(variable)) => Ok(Some(Experiment {
            hypothesis: hypothesis.clone(),
            variable: variable.clone(),
        })),
        _ => Err("--hypothesis and --variable must be supplied together".into()),
    }
}

pub(super) fn job_model_pins(parsed: &SubmitArgs) -> ModelPins {
    ModelPins {
        provider: parsed.provider.clone(),
        model: parsed.model.clone(),
        base_url: parsed.base_url.clone(),
        credential_alias: parsed.credential_alias.clone(),
        thinking: parsed.thinking.clone(),
        max_turns: parsed.max_turns,
        sampling: SAMPLING_OMITTED.to_string(),
    }
}

pub(super) fn queue_job(parsed: &SubmitArgs, repository: &Path) -> Result<JobSpec, Box<dyn Error>> {
    let task_file = parsed
        .task
        .clone()
        .ok_or("compare submit requires --task <file>")?;
    let store = JobStore::for_repository(repository);
    if RunnerLock::is_held(&store) {
        return Err("another comparison is already running in this repository".into());
    }
    let experiment = submit_experiment(parsed)?;
    let harnesses = harness_requests(parsed)?;
    let ids: Vec<&str> = harnesses.iter().map(|h| h.id.as_str()).collect();
    let run_order = rotate_run_order(store.job_count()?, &canonical_run_order(&ids));
    transport::submit(transport::SubmitOptions {
        repository: repository.to_path_buf(),
        base_revision: parsed.commit.clone(),
        task_file,
        harnesses,
        run_order,
        model: job_model_pins(parsed),
        limits: parsed.limits.clone(),
        verifier: VerifierProfile::WorkspaceTests,
        task_aware_context: parsed.task_aware_context,
        acceptance_overlay: parsed.acceptance_overlay.clone(),
        experiment,
    })
}

pub(super) fn doctor_spec(
    parsed: &SubmitArgs,
    repository: &Path,
) -> Result<JobSpec, Box<dyn Error>> {
    let task_file = parsed
        .task
        .clone()
        .ok_or("compare doctor requires --task <file>")?;
    let harnesses = harness_requests(parsed)?;
    let run_order =
        canonical_run_order(&harnesses.iter().map(|h| h.id.as_str()).collect::<Vec<_>>());
    transport::build_spec(transport::SubmitOptions {
        repository: repository.to_path_buf(),
        base_revision: parsed.commit.clone(),
        task_file,
        harnesses,
        run_order,
        model: job_model_pins(parsed),
        limits: parsed.limits.clone(),
        verifier: VerifierProfile::WorkspaceTests,
        task_aware_context: parsed.task_aware_context,
        acceptance_overlay: parsed.acceptance_overlay.clone(),
        experiment: None,
    })
}

pub(super) fn print_doctor_report(report: &preflight::PreflightReport) {
    println!("doctor=ok");
    println!("repository={}", report.repository.display());
    println!("base_commit={}", report.base_commit);
    println!("free_bytes={}", report.free_bytes);
    println!(
        "estimated_required_bytes={}",
        report.estimated_required_bytes
    );
    println!("credential_environment={}", report.credential_environment);
}
