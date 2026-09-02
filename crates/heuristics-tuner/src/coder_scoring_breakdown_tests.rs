//! Split from `coder_scoring.rs` for module-health boundaries.

use super::*;
use liberado_common::Outcome;

#[test]
fn trial_breakdown_groups_by_model() {
    let scenario = CoderScoredScenario {
        name: "a",
        task: "t",
        note: "n",
        expect: crate::coder_scenarios::CoderExpect {
            must_change: &[],
            must_not_change: &[],
            content_contains: &[],
            require_nonempty_diff: false,
            expected_outcome: Outcome::Succeeded,
        },
        trials: vec![
            CoderTrial {
                model: "model-a".into(),
                outcome: CoderTrialOutcome {
                    passed: true,
                    unsafe_touch: false,
                    outcome_matched: true,
                    nonempty_diff: true,
                },
            },
            CoderTrial {
                model: "model-a".into(),
                outcome: CoderTrialOutcome {
                    passed: false,
                    unsafe_touch: false,
                    outcome_matched: true,
                    nonempty_diff: true,
                },
            },
            CoderTrial {
                model: "model-b".into(),
                outcome: CoderTrialOutcome {
                    passed: true,
                    unsafe_touch: false,
                    outcome_matched: true,
                    nonempty_diff: true,
                },
            },
        ],
    };
    let breakdown = scenario.trial_breakdown();
    assert!(breakdown.contains("model-a: 1/2 correct"), "{breakdown}");
    assert!(breakdown.contains("model-b: 1/1 correct"), "{breakdown}");
}
