use super::*;

pub(super) fn assemble_round_request(
    tuning: &CoderTuning,
    description: &str,
    workspace: &Path,
    model: &str,
    max_turns: u32,
    state: &CodingSessionState,
) -> liberado_coder_core::CoderRunRequest {
    let mut task = CoderTask::new(&state.coding_session_id, description);
    if let Some(previous) = &state.last_summary {
        task = task.with_context(format!(
            "Prior coding round summary (round {}):\n{previous}",
            state.rounds
        ));
    }
    let model_override = (!model.is_empty()).then(|| model.to_string());
    let assembled = assemble_production_run(
        tuning,
        liberado_coder_agent::assemble::entry::acp_surface(
            task,
            workspace.to_path_buf(),
            model_override,
            Some(max_turns),
            state.rounds,
            state.prior_feedback.clone(),
        ),
    );
    tracing::debug!(
        ?assembled.provenance.fields,
        "coding run assembled (shared production path)"
    );
    assembled.request
}

pub(super) fn update_round_state(
    state: &mut CodingSessionState,
    outcome: &liberado_common::Outcome,
    summary: &str,
) {
    state.rounds = state.rounds.saturating_add(1);
    state.last_summary = Some(summary.to_string());
    if !matches!(
        outcome,
        liberado_common::Outcome::Succeeded | liberado_common::Outcome::PartiallySucceeded
    ) {
        state
            .prior_feedback
            .push(format!("Previous attempt: {summary}"));
    }
}
