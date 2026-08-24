//! Criteria intake: thinking model → structured [`IntakeOutcome`] → human freeze → contract.
//!
//! Does not mutate the workspace. Does not invent authoritative gates without freeze.

use liberado_coder_core::{
    CoderError, FreezeAuthority, GoalContract, IntakeOutcome, intake_outcome_schema, validate_draft,
};
use liberado_provider::{CompletionRequest, Message, Provider, complete_json};

/// The intake planner's instructions, from `prompts/coder/intake.md`.
///
/// Was a `const` here. Intake is the phase that decides whether an unattended goal asks a
/// human a question or gets on with the work — F11 in the backlog — so it is exactly the prompt
/// an operator most wants to retune, and it was the one locked inside the binary.
/// No workspace argument on purpose: intake runs *before* a workspace exists, so there is no
/// checkout to read from. It uses the process-level `prompts/coder` and the baked copy otherwise.
fn intake_system() -> String {
    liberado_coder_core::prompts::load(
        None,
        liberado_coder_core::prompts::INTAKE_FILE,
        liberado_coder_core::prompts::INTAKE,
    )
}

/// One human answer to a prior intake question (id → free text or chosen option).
#[derive(Debug, Clone)]
pub struct IntakeAnswer {
    pub question_id: String,
    pub answer: String,
}

/// Run a single intake model turn.
pub async fn run_intake(
    provider: &dyn Provider,
    writeup: &str,
    answers: &[IntakeAnswer],
    context: Option<&str>,
) -> Result<IntakeOutcome, CoderError> {
    let mut user = String::new();
    user.push_str("Human goal writeup:\n");
    user.push_str(writeup.trim());
    user.push('\n');
    if let Some(ctx) = context.filter(|c| !c.trim().is_empty()) {
        user.push_str("\nAdditional context (read-only):\n");
        user.push_str(ctx.trim());
        user.push('\n');
    }
    if !answers.is_empty() {
        user.push_str("\nHuman answers to prior questions:\n");
        for a in answers {
            user.push_str("- ");
            user.push_str(&a.question_id);
            user.push_str(": ");
            user.push_str(&a.answer);
            user.push('\n');
        }
    }
    user.push_str("\nProduce the next IntakeOutcome JSON now.");

    let request =
        CompletionRequest::new(vec![Message::system(intake_system()), Message::user(user)])
            .with_temperature(0.2)
            .with_max_tokens(4096);

    let outcome: IntakeOutcome = complete_json(provider, request, intake_outcome_schema())
        .await
        .map_err(|e| CoderError::Provider(format!("intake complete_json: {e}")))?;

    // Sanitize + soft-validate ready drafts so bad model output fails here, not at freeze.
    let outcome = match outcome {
        IntakeOutcome::ReadyForFreeze {
            mut draft,
            rationale,
        } => {
            liberado_coder_core::sanitize_draft(&mut draft);
            liberado_coder_core::expand_verify_profile_into(&mut draft);
            validate_draft(&draft)
                .map_err(|e| CoderError::Setup(format!("intake draft failed validation: {e}")))?;
            IntakeOutcome::ReadyForFreeze { draft, rationale }
        }
        IntakeOutcome::NeedsClarification {
            questions,
            mut partial_draft,
        } => {
            if let Some(draft) = partial_draft.as_mut() {
                liberado_coder_core::sanitize_draft(draft);
            }
            IntakeOutcome::NeedsClarification {
                questions,
                partial_draft,
            }
        }
    };

    Ok(outcome)
}

/// Convenience: freeze a ready draft (or error if still needs clarification).
pub fn freeze_if_ready(
    outcome: IntakeOutcome,
    contract_id: impl Into<String>,
    authority: FreezeAuthority,
) -> Result<GoalContract, CoderError> {
    match outcome {
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            GoalContract::freeze(contract_id, draft, authority).map_err(CoderError::Setup)
        }
        IntakeOutcome::NeedsClarification { questions, .. } => Err(CoderError::Setup(format!(
            "intake still needs clarification ({} question(s)); cannot freeze",
            questions.len()
        ))),
    }
}

/// Build a coding run request shell from a frozen contract + workspace + base config.
pub fn request_from_contract(
    contract: &GoalContract,
    mut base: liberado_coder_core::CoderRunRequest,
) -> liberado_coder_core::CoderRunRequest {
    contract.apply_to_request(&mut base);
    // Prefer contract id as task id when the base id is empty/default-ish.
    if base.task.id.is_empty() {
        base.task.id = contract.id.clone();
    }
    base
}

/// Multi-round helper for tests/CLI: run intake until Ready or max rounds.
pub async fn run_intake_until_ready(
    provider: &dyn Provider,
    writeup: &str,
    mut answer_fn: impl FnMut(&[liberado_coder_core::IntakeQuestion]) -> Vec<IntakeAnswer>,
    context: Option<&str>,
    max_rounds: u32,
) -> Result<(IntakeOutcome, Vec<IntakeAnswer>), CoderError> {
    let mut answers: Vec<IntakeAnswer> = Vec::new();
    let mut last = run_intake(provider, writeup, &answers, context).await?;
    for _ in 0..max_rounds.saturating_sub(1) {
        match &last {
            IntakeOutcome::ReadyForFreeze { .. } => break,
            IntakeOutcome::NeedsClarification { questions, .. } => {
                let more = answer_fn(questions);
                if more.is_empty() {
                    break;
                }
                answers.extend(more);
                last = run_intake(provider, writeup, &answers, context).await?;
            }
        }
    }
    Ok((last, answers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::{
        CoderRoleConfig, CoderRunConfig, CoderRunRequest, CoderTask, CommandPolicy,
        GoalContractDraft, LIBERADO_LOOP_BACKEND, PathPolicy, ProgressPolicy, SandboxSpec,
        VerifierSpec, WorkspaceRef,
    };
    use liberado_provider::{CompletionResponse, MockProvider};

    fn role() -> CoderRoleConfig {
        CoderRoleConfig {
            model: "mock".into(),
            prompt_path: None,
            prompt: Some("coder".into()),
            temperature: None,
            max_tokens: None,
            max_turns: Some(4),
            reasoning: None,
        }
    }

    fn base_request() -> CoderRunRequest {
        CoderRunRequest {
            task: CoderTask::new("t", "old description"),
            workspace: WorkspaceRef::new("/ws", "HEAD"),
            config: CoderRunConfig {
                backend: LIBERADO_LOOP_BACKEND.into(),
                trace_dir: None,
                trace_formats: Vec::new(),
                planner: role(),
                coder: role(),
                critic: role(),
                gate: liberado_coder_core::CoderGateConfig::default(),
                repair: None,
                sandbox: SandboxSpec::HostLocal,
                command_policy: CommandPolicy::default(),
                validation_command: None,
                verifiers: Vec::new(),
                verify_policy: Default::default(),
                path_policy: PathPolicy::default(),
                progress: ProgressPolicy::default(),
                hashline: liberado_coder_core::HashlineConfig::default(),
                session_critic: Default::default(),
                prompt_dir: None,
                edit: Default::default(),
                workspace_build: Default::default(),
                offered_tools: None,
            },
            attempt: 0,
            prior_feedback: Vec::new(),
            strategist_directive: None,
        }
    }

    #[tokio::test]
    async fn mock_intake_ready_freezes_and_applies() {
        let draft = GoalContractDraft {
            description: "Build a todo CLI".into(),
            success_criteria: vec!["add and list work".into()],
            verifiers: vec![VerifierSpec::PathsExist {
                id: "paths".into(),
                paths: vec!["src/main.rs".into(), "Cargo.toml".into()],
            }],
            out_of_scope: vec!["network".into()],
            assumed_defaults: vec!["Rust".into()],
            domain_hint: Some("coding".into()),
            verify_profile: Some("rust-check".into()),
        };
        let body = serde_json::to_string(&IntakeOutcome::ReadyForFreeze {
            draft: draft.clone(),
            rationale: "clear enough".into(),
        })
        .unwrap();
        let provider = MockProvider::with_script("mock", [CompletionResponse::text(body)]);
        let outcome = run_intake(&provider, "make a todo cli", &[], None)
            .await
            .unwrap();
        let contract = freeze_if_ready(outcome, "goal-1", FreezeAuthority::Human).unwrap();
        // Profile expanded at freeze.
        assert!(
            contract
                .draft
                .verifiers
                .iter()
                .any(|v| v.id() == "cargo-check")
        );
        let mut req = base_request();
        contract.apply_to_request(&mut req);
        assert_eq!(req.task.description, "Build a todo CLI");
        assert!(!req.config.verifiers.is_empty());
    }

    #[tokio::test]
    async fn mock_intake_needs_clarification() {
        let body = r#"{
            "status": "needs_clarification",
            "questions": [
              {"id": "stack", "prompt": "Rust or Node?", "options": ["Rust", "Node"], "affects": "profile"}
            ]
        }"#;
        let provider = MockProvider::with_script("mock", [CompletionResponse::text(body)]);
        let outcome = run_intake(&provider, "make a todo app", &[], None)
            .await
            .unwrap();
        match &outcome {
            IntakeOutcome::NeedsClarification { questions, .. } => {
                assert_eq!(questions[0].id, "stack");
            }
            other => panic!("expected clarification, got {other:?}"),
        }
        assert!(freeze_if_ready(outcome, "g", FreezeAuthority::Human).is_err());
    }

    #[test]
    fn request_from_contract_sets_fields() {
        let draft = GoalContractDraft {
            description: "desc".into(),
            success_criteria: vec!["c1".into()],
            verifiers: vec![VerifierSpec::GitNonemptyDiff { id: "diff".into() }],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        };
        let contract = GoalContract::freeze("cid", draft, FreezeAuthority::Human).unwrap();
        let req = request_from_contract(&contract, base_request());
        assert_eq!(req.task.description, "desc");
        assert_eq!(req.task.success_criteria, vec!["c1".to_string()]);
        assert_eq!(req.config.verifiers.len(), 1);
    }
}

#[cfg(test)]
#[path = "intake_coldreview_survivor_tests.rs"]
mod survivor_tests;
