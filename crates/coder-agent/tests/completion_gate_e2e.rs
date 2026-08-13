//! The completion gate over the real coding backend (S1 proof, `docs/future-work/coding-tui-plan.md`).
//!
//! `completion_gate.rs`'s unit tests prove the kernel's quorum math in isolation. These prove the
//! part that isolation cannot: that the gate is actually *wired into the pipeline* — that a refusing
//! quorum really does stop a run whose worker succeeded and whose deterministic verifiers passed.
//! A gate that is correct but unreachable is worth nothing, and that is exactly the failure a unit
//! test cannot see.
//!
//! Mock providers only — no network, no real models.
//!
//! ```sh
//! cargo test -p liberado-coder-agent --test completion_gate_e2e
//! ```

mod common;

use common::{
    base_request, freeze_structural_todo, init_repo, mock_backend, write_todo_scaffold_then_report,
};
use liberado_coder_agent::request_from_contract;
use liberado_coder_core::{
    CoderBackend, CoderGateConfig, CoderRoleConfig, CoderRunRequest, CriticVerdict,
};
use liberado_common::Outcome;
use liberado_provider::CompletionResponse;

/// A reviewer role the gate can actually resolve instructions for. `role_instructions` needs a
/// prompt (or prompt_path); an inline prompt keeps the test off the filesystem.
fn reviewer_role() -> CoderRoleConfig {
    CoderRoleConfig {
        model: "mock".to_string(),
        prompt_path: None,
        prompt: Some("You review code changes against acceptance criteria.".to_string()),
        temperature: None,
        max_tokens: None,
        max_turns: None,
        reasoning: None,
    }
}

/// Base request with the gate enabled and `fresh` cold reviewers.
fn gated_request(root: &std::path::Path, fresh: u8) -> CoderRunRequest {
    let contract = freeze_structural_todo("goal-gate-1");
    let mut request = request_from_contract(&contract, base_request(root));
    request.config.critic = reviewer_role();
    request.config.gate = CoderGateConfig {
        enabled: true,
        fresh_reviewers: fresh,
        // Off: the strategist is a separate role and is not what these tests are about.
        strategist_after: 0,
        gatekeeper: None,
        fresh: None,
        strategist: None,
    };
    // One attempt: the backend retries on a revision request, and a retry would consume the
    // scripted responses meant for the gate.
    request.config.progress.max_attempts = 1;
    request
}

fn approve() -> CompletionResponse {
    CompletionResponse::text(r#"{"quality":"acceptable"}"#)
}

fn refute(issue: &str) -> CompletionResponse {
    CompletionResponse::text(format!(
        r#"{{"quality":"needs_revision","issues":["{issue}"]}}"#
    ))
}

#[tokio::test]
async fn unanimous_gate_lets_a_good_run_succeed() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    // Worker script, then gatekeeper + 2 fresh reviewers all approving.
    let mut script = write_todo_scaffold_then_report();
    script.extend([approve(), approve(), approve()]);

    let result = mock_backend(script)
        .run(gated_request(dir.path(), 2))
        .await
        .unwrap();

    assert_eq!(
        result.outcome,
        Outcome::Succeeded,
        "an approved run must still finish: {}",
        result.summary
    );
    assert_eq!(result.critic_verdict, Some(CriticVerdict::Acceptable));
}

#[tokio::test]
async fn a_refusing_quorum_blocks_a_run_whose_verifiers_passed() {
    // The point of the whole slice: the worker succeeded and the deterministic checks passed, and
    // the run STILL does not get to claim success because independent review disagreed.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let mut script = write_todo_scaffold_then_report();
    // Gatekeeper approves, then both cold reviewers refuse → no strict majority.
    script.extend([
        approve(),
        refute("no test covers the delete path"),
        refute("no test covers the delete path"),
    ]);

    let result = mock_backend(script)
        .run(gated_request(dir.path(), 2))
        .await
        .unwrap();

    assert_eq!(
        result.outcome,
        Outcome::Failed,
        "a refuted claim must not terminate Succeeded"
    );
    assert!(
        result.summary.contains("completion gate refused"),
        "the summary must say the gate refused, not something vague: {}",
        result.summary
    );
    match result.critic_verdict {
        Some(CriticVerdict::NeedsRevision { issues }) => assert!(
            issues.iter().any(|i| i.contains("delete path")),
            "reviewer issues must survive into the verdict for the next attempt's feedback"
        ),
        other => panic!("expected NeedsRevision, got {other:?}"),
    }
}

#[tokio::test]
async fn the_gatekeeper_veto_alone_blocks_the_run() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let mut script = write_todo_scaffold_then_report();
    // Gatekeeper refuses. The two approvals after it should never be consumed.
    script.extend([
        refute("this is the same defect as the last attempt, renamed"),
        approve(),
        approve(),
    ]);

    let result = mock_backend(script)
        .run(gated_request(dir.path(), 2))
        .await
        .unwrap();

    assert_eq!(result.outcome, Outcome::Failed);
    match result.critic_verdict {
        Some(CriticVerdict::NeedsRevision { issues }) => assert!(
            issues.iter().any(|i| i.contains("renamed")),
            "the gatekeeper's reasoning must reach the verdict"
        ),
        other => panic!("expected NeedsRevision, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unavailable_reviewer_cannot_lower_the_bar() {
    // Fail-closed, end to end. The scripted provider runs out of responses, so the third reviewer
    // call errors — the run must NOT be rescued by the two approvals that did land.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let mut script = write_todo_scaffold_then_report();
    script.extend([approve(), approve()]); // gatekeeper + one fresh; the second fresh gets nothing

    let result = mock_backend(script)
        .run(gated_request(dir.path(), 2))
        .await
        .unwrap();

    assert_eq!(
        result.outcome,
        Outcome::Failed,
        "an exhausted/failing reviewer must count as refuting, not be skipped: {}",
        result.summary
    );
}

#[tokio::test]
async fn a_disabled_gate_leaves_the_legacy_path_untouched() {
    // Back-compat: `gate.enabled = false` (the default) must not consume reviewer responses or
    // change the outcome, so existing deployments see no behavior change from this slice.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let contract = freeze_structural_todo("goal-gate-1");
    let mut request = request_from_contract(&contract, base_request(dir.path()));
    request.config.progress.max_attempts = 1;
    assert!(!request.config.gate.enabled, "the gate must default to off");

    // No reviewer responses scripted at all — proof that nothing extra is called.
    let result = mock_backend(write_todo_scaffold_then_report())
        .run(request)
        .await
        .unwrap();

    assert_eq!(result.outcome, Outcome::Succeeded);
    assert_eq!(
        result.critic_verdict, None,
        "with the gate off and no critic configured, nothing should have reviewed"
    );
}

#[tokio::test]
async fn a_refutation_feeds_the_next_attempt_and_the_gate_can_then_approve() {
    // The plan's second S1 proof: refutation must reach `prior_feedback`, and the gatekeeper (which
    // is the *remembered* reviewer) must actually be shown it on the retry. Without this the
    // gatekeeper is just a third cold reviewer with a different name, and the "catches the same
    // defect re-disguised" property — the whole reason it exists — is not real.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let issue = "the delete path is still untested";
    let mut script = write_todo_scaffold_then_report();
    script.push(refute(issue)); // attempt 0: gatekeeper vetoes (fresh never consulted)
    script.extend(write_todo_scaffold_then_report()); // attempt 1: worker runs again
    script.extend([approve(), approve()]); // attempt 1: gatekeeper + 1 fresh approve

    let provider = common::mock_provider(script);
    let backend = liberado_coder_agent::LiberadoLoopBackend::new(provider.clone());

    let mut request = gated_request(dir.path(), 1);
    request.config.progress.max_attempts = 2;

    let result = backend.run(request).await.unwrap();

    assert_eq!(
        result.outcome,
        Outcome::Succeeded,
        "attempt 2 was approved, so the run must finish: {}",
        result.summary
    );

    // The retry's gatekeeper prompt must carry the earlier refutation.
    let prompts: Vec<String> = provider
        .received_requests()
        .iter()
        .flat_map(|r| r.messages.iter().map(|m| m.content.clone()))
        .collect();
    let remembered = prompts
        .iter()
        .filter(|p| p.contains("Your previous refutations"))
        .collect::<Vec<_>>();
    assert!(
        !remembered.is_empty(),
        "the gatekeeper on the retry was never shown its refutation history"
    );
    assert!(
        remembered.iter().any(|p| p.contains(issue)),
        "the specific earlier complaint must survive into the retry's gatekeeper prompt"
    );
    // And the cold reviewers must still be cold.
    assert!(
        prompts
            .iter()
            .filter(|p| p.contains("reviewing this work COLD"))
            .all(|p| !p.contains("Your previous refutations")),
        "a fresh reviewer must never receive the refutation history"
    );
}

#[tokio::test]
async fn gate_votes_reach_the_result_so_the_pack_can_put_them_on_the_wire() {
    // The backend has no SessionEvent sender, so votes travel to the surface on the result and the
    // session pack emits them. If `gate_votes` is left empty, every reviewer vote silently vanishes
    // from the wire and the gate becomes invisible — which is exactly the bug this test exists for
    // (an earlier revision shipped the empty vec; clippy's dead-code warning on the flattening
    // helper was the only thing that noticed).
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let mut script = write_todo_scaffold_then_report();
    script.extend([
        approve(),                  // gatekeeper
        refute("missing rollback"), // fresh-0
        approve(),                  // fresh-1
    ]);

    let result = mock_backend(script)
        .run(gated_request(dir.path(), 2))
        .await
        .unwrap();

    assert_eq!(
        result.gate_votes.len(),
        3,
        "every vote must be carried on the result, not just written to the trace"
    );
    assert_eq!(result.gate_votes[0].kind, "gatekeeper");
    assert!(result.gate_votes[0].approved);
    assert_eq!(result.gate_votes[1].kind, "fresh");
    assert!(!result.gate_votes[1].approved);
    assert_eq!(
        result.gate_votes[1].issues,
        vec!["missing rollback".to_string()]
    );
    assert!(
        result.gate_votes.iter().all(|v| !v.coerced),
        "no reviewer failed here, so nothing should be marked coerced"
    );
}

#[tokio::test]
async fn a_coerced_vote_is_marked_as_such_on_the_result() {
    // An operator debugging "why did my build stop converging" has to be able to tell a broken
    // reviewer from a strict one.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let mut script = write_todo_scaffold_then_report();
    script.extend([approve(), approve()]); // gatekeeper + fresh-0; fresh-1 gets nothing

    let result = mock_backend(script)
        .run(gated_request(dir.path(), 2))
        .await
        .unwrap();

    let coerced: Vec<_> = result.gate_votes.iter().filter(|v| v.coerced).collect();
    assert_eq!(coerced.len(), 1, "the failed reviewer must be flagged");
    assert!(!coerced[0].approved);
}

// ── strategist (non-convergence) ──────────────────────────────────────────────────────

/// Directive text the scripted strategist returns. Distinctive so we can find it in prompts.
const DIRECTIVE: &str = "Move validation out of the request builder into the caller.";

#[tokio::test]
async fn the_strategist_fires_after_repeated_refutations_and_its_directive_reaches_the_worker() {
    // The strategist exists for the case where feedback has stopped working: same refusal, again.
    // Proving it fired is not enough — the directive has to actually arrive in the next attempt's
    // worker prompt, or the role is an expensive no-op.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let mut script = write_todo_scaffold_then_report(); // attempt 0
    script.push(refute("same defect")); //            attempt 0 gatekeeper veto
    script.push(CompletionResponse::text(DIRECTIVE)); // strategist (threshold 1)
    script.extend(write_todo_scaffold_then_report()); // attempt 1
    script.extend([approve(), approve()]); //           attempt 1 gate passes

    let provider = common::mock_provider(script);
    let backend = liberado_coder_agent::LiberadoLoopBackend::new(provider.clone());

    let mut request = gated_request(dir.path(), 1);
    request.config.progress.max_attempts = 2;
    request.config.gate.strategist_after = 1; // fire after the first refutation

    let result = backend.run(request).await.unwrap();
    assert_eq!(result.outcome, Outcome::Succeeded, "{}", result.summary);

    let prompts: Vec<String> = provider
        .received_requests()
        .iter()
        .flat_map(|r| r.messages.iter().map(|m| m.content.clone()))
        .collect();

    assert!(
        prompts.iter().any(|p| p.contains("You are a strategist")),
        "the strategist was never consulted"
    );
    assert!(
        prompts
            .iter()
            .any(|p| p.contains(DIRECTIVE) && p.contains("Structural directive")),
        "the directive never reached the worker's goal prompt"
    );
}

#[tokio::test]
async fn the_strategist_stays_quiet_below_the_threshold() {
    // One refusal is not non-convergence. Firing on it would spend a model call to repeat what the
    // reviewers already said.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let mut script = write_todo_scaffold_then_report();
    script.push(refute("first refusal"));
    script.extend(write_todo_scaffold_then_report());
    script.extend([approve(), approve()]);

    let provider = common::mock_provider(script);
    let backend = liberado_coder_agent::LiberadoLoopBackend::new(provider.clone());

    let mut request = gated_request(dir.path(), 1);
    request.config.progress.max_attempts = 2;
    request.config.gate.strategist_after = 3; // needs three, gets one

    backend.run(request).await.unwrap();

    assert!(
        !provider
            .received_requests()
            .iter()
            .flat_map(|r| r.messages.iter())
            .any(|m| m.content.contains("You are a strategist")),
        "the strategist fired on a single refutation"
    );
}

#[tokio::test]
async fn a_failing_strategist_does_not_break_the_run() {
    // Best-effort by contract: this role runs on top of work that already exists, so its outage
    // must cost a directive and nothing else.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let mut script = write_todo_scaffold_then_report();
    script.push(refute("same defect"));
    // No strategist response scripted — the call errors.
    script.extend(write_todo_scaffold_then_report());
    script.extend([approve(), approve()]);

    let mut request = gated_request(dir.path(), 1);
    request.config.progress.max_attempts = 2;
    request.config.gate.strategist_after = 1;

    let result = mock_backend(script).run(request).await;

    assert!(
        result.is_ok(),
        "a strategist failure must not propagate as a run error"
    );
}

#[tokio::test]
async fn the_strategist_is_told_it_may_not_weaken_the_criteria() {
    // The one instruction that makes this role safe. A strategist that can relax acceptance
    // criteria when work is hard converts every stuck goal into a passing one.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let mut script = write_todo_scaffold_then_report();
    script.push(refute("same defect"));
    script.push(CompletionResponse::text(DIRECTIVE));
    script.extend(write_todo_scaffold_then_report());
    script.extend([approve(), approve()]);

    let provider = common::mock_provider(script);
    let backend = liberado_coder_agent::LiberadoLoopBackend::new(provider.clone());

    let mut request = gated_request(dir.path(), 1);
    request.config.progress.max_attempts = 2;
    request.config.gate.strategist_after = 1;
    backend.run(request).await.unwrap();

    let system = provider
        .received_requests()
        .iter()
        .flat_map(|r| r.messages.iter().map(|m| m.content.clone()))
        .find(|c| c.contains("You are a strategist"))
        .expect("strategist prompt");

    assert!(
        system.contains("may NOT weaken"),
        "the strategist must be forbidden from lowering the bar"
    );
    assert!(
        system.contains("EXACTLY ONE"),
        "the strategist must be constrained to one structural change"
    );
}
