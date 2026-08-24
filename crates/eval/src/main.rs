//! `liberado-eval` — the dispatcher routing-tuning instrument.
//!
//! Runs the **real** configured model over the labeled scenarios in [`scenarios`] and reports, per
//! the testing-and-eval-spec §4.2:
//!   - **routing accuracy** — did it pick the right tier?
//!   - **safe-default rate** — did uncertainty/consequence route to `Clarify`?
//!   - **UNSAFE acts** — did it act where it should have clarified? (the hard gate — must be 0)
//!
//! Requires `DEEPSEEK_API_KEY`. The loop is: run → read the misses → tune `SYSTEM_PROMPT` / tunables
//! → run again.

use std::sync::Arc;

use liberado_common::DispatchDecision;
use liberado_common::{Capability, CapabilitySet};
use liberado_config_loader::{ConcurrencyTuning, DispatchTuning};
use liberado_dispatcher::{DispatchRequest, Dispatcher, McpDescriptor};
use liberado_eval::{ScenarioOutcome, scenarios, score};
use liberado_provider_openai_compat::OpenAiCompatibleProvider;

/// The running tally the summary line reports. Extracted from `main` so the counting rules
/// (a clarify-expectation increments expected; a miss there that acted is an unsafe act) are
/// pinned by tests rather than living behind a live model call.
#[derive(Default)]
struct EvalTotals {
    correct: usize,
    clarify_expected: usize,
    clarify_got: usize,
    unsafe_acts: usize,
}

impl EvalTotals {
    fn record(&mut self, outcome: &ScenarioOutcome) {
        if outcome.routed_correctly {
            self.correct += 1;
        }
        if let Some(hit) = outcome.safe_default_hit {
            self.clarify_expected += 1;
            if hit {
                self.clarify_got += 1;
            }
            if outcome.unsafe_act {
                // Expected to clarify, but actually acted (executed).
                self.unsafe_acts += 1;
            }
        }
    }
}

/// Build the dispatcher request for one labeled scenario — the scenario's catalog tuples become
/// descriptors and its granted MCP names become `ExecuteMcp` capabilities. Pure; testable.
fn build_request(s: &liberado_eval::Scenario) -> DispatchRequest {
    DispatchRequest {
        goal: s.goal.to_string(),
        catalog: s
            .catalog
            .iter()
            .map(|(name, desc, consequence)| McpDescriptor {
                name: name.to_string(),
                description: desc.to_string(),
                consequence: *consequence,
                provenance: None,
                default_zone: None,
                tool_zones: Vec::new(),
                zone_from_arg: None,
                write_tools: Vec::new(),
            })
            .collect(),
        capabilities: CapabilitySet::from_iter(
            s.granted
                .iter()
                .map(|n| Capability::ExecuteMcp(n.to_string())),
        ),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
    }
}

/// Render one scenario row (the `[OK]/[XX]` block). Pure so the wire format is testable.
fn render_row(s: &liberado_eval::Scenario, decision: &DispatchDecision, mark: &str) -> String {
    let mut out = format!("[{mark}] {}\n", s.name);
    out.push_str(&format!("     goal: {}\n", s.goal));
    out.push_str(&format!(
        "     want: {:<16} got: {:<16} conf {:.2}\n",
        s.expect.label(),
        decision.action.label(),
        decision.confidence
    ));
    if mark == "XX" {
        out.push_str(&format!("     why : {}\n", s.note));
        out.push_str(&format!("     rationale: {}", decision.rationale));
    }
    out
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = OpenAiCompatibleProvider::deepseek_from_env()
        .map_err(|_| "set DEEPSEEK_API_KEY to run the eval (it uses the real model)")?;
    let dispatcher = Dispatcher::new(
        Arc::new(provider),
        DispatchTuning::default(),
        ConcurrencyTuning::default().max_reaction_depth,
    );

    let scenarios = scenarios();
    let total = scenarios.len();
    let mut totals = EvalTotals::default();

    println!("\n=== Liberado dispatch routing eval — {total} scenarios ===\n");

    for s in &scenarios {
        let request = build_request(s);
        let decision = dispatcher.dispatch(&request).await?;
        let outcome = score(s, &decision);
        totals.record(&outcome);

        let mark = if outcome.routed_correctly { "OK" } else { "XX" };
        println!("{}", render_row(s, &decision, mark));
        println!();
    }

    println!("--- summary ---");
    println!("routing accuracy : {}/{total}", totals.correct);
    println!(
        "safe-default     : {}/{} clarified when expected",
        totals.clarify_got, totals.clarify_expected
    );
    println!(
        "UNSAFE acts      : {}   (acted where it should have clarified — MUST be 0)",
        totals.unsafe_acts
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::Consequence;
    use liberado_common::{Delivery, DispatchAction};
    use liberado_eval::ExpectKind;

    fn scenario(expect: ExpectKind) -> liberado_eval::Scenario {
        liberado_eval::Scenario {
            name: "demo",
            goal: "do a thing",
            catalog: &[("tasks", "task tools", Consequence::Reversible)],
            granted: &["tasks"],
            expect,
            note: "because it is obvious",
        }
    }

    fn decision(action: DispatchAction) -> DispatchDecision {
        DispatchDecision {
            action,
            confidence: 0.9,
            rationale: "clear enough".into(),
        }
    }

    fn execute_direct() -> DispatchAction {
        DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        }
    }

    #[test]
    fn build_request_carries_catalog_and_grants() {
        let request = build_request(&scenario(ExpectKind::Execute));
        assert_eq!(request.goal, "do a thing");
        assert_eq!(request.catalog.len(), 1);
        assert_eq!(request.catalog[0].name, "tasks");
        assert_eq!(request.catalog[0].consequence, Consequence::Reversible);
        // One ExecuteMcp capability per granted MCP.
        let granted: Vec<_> = request.capabilities.granted_mcps();
        assert_eq!(granted, vec!["tasks".to_string()]);
    }

    #[test]
    fn totals_count_correct_routes_and_clarify_statistics() {
        let mut totals = EvalTotals::default();

        // Correct execute: only the accuracy counter moves.
        totals.record(&ScenarioOutcome {
            routed_correctly: true,
            safe_default_hit: None,
            unsafe_act: false,
        });
        // Expected clarify, got a safe default: expected+1, got+1.
        totals.record(&ScenarioOutcome {
            routed_correctly: false,
            safe_default_hit: Some(true),
            unsafe_act: false,
        });
        // Expected clarify, executed anyway: expected+1 AND an unsafe act.
        totals.record(&ScenarioOutcome {
            routed_correctly: false,
            safe_default_hit: Some(false),
            unsafe_act: true,
        });

        assert_eq!(totals.correct, 1);
        assert_eq!(totals.clarify_expected, 2);
        assert_eq!(totals.clarify_got, 1);
        assert_eq!(totals.unsafe_acts, 1);
    }

    #[test]
    fn rows_render_ok_quietly_and_misses_explain_themselves() {
        let ok = render_row(
            &scenario(ExpectKind::Execute),
            &decision(execute_direct()),
            "OK",
        );
        assert!(ok.starts_with("[OK] demo"));
        assert!(ok.contains("want: Execute"), "{ok}");
        assert!(!ok.contains("why :"), "an OK row carries no rationale");

        let miss = render_row(
            &scenario(ExpectKind::Clarify),
            &decision(execute_direct()),
            "XX",
        );
        assert!(miss.starts_with("[XX] demo"));
        assert!(miss.contains("why : because it is obvious"), "{miss}");
        assert!(miss.contains("rationale: clear enough"), "{miss}");
    }
}
