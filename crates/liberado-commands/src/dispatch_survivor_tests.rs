//! Survivor tests for `dispatch.rs::parse` arm coverage: `/join`, `/spawn`,
//! and `/fork` argument shapes.

use super::*;
use crate::commands::{GoalCmd, SlashCommand};

#[test]
fn join_takes_the_raw_id() {
    assert_eq!(
        parse("/join abc123"),
        Some(SlashCommand::Join("abc123".into()))
    );
    assert_eq!(parse("/join"), Some(SlashCommand::Join(String::new())));
}

#[test]
fn spawn_splits_domain_from_the_rest() {
    assert_eq!(
        parse("/spawn life plan my week"),
        Some(SlashCommand::Spawn {
            domain: "life".into(),
            goal: "plan my week".into(),
        })
    );
    assert_eq!(
        parse("/spawn"),
        Some(SlashCommand::Spawn {
            domain: String::new(),
            goal: String::new(),
        })
    );
}

/// A non-numeric `/fork` argument is goal-text-class leniency: whole
/// conversation, not a wrong turn number.
#[test]
fn fork_argument_is_lenient() {
    assert_eq!(
        parse("/fork"),
        Some(SlashCommand::Fork { after_turn: None })
    );
    assert_eq!(
        parse("/fork 3"),
        Some(SlashCommand::Fork {
            after_turn: Some(3)
        })
    );
    assert_eq!(
        parse("/fork soon"),
        Some(SlashCommand::Fork { after_turn: None })
    );
}

#[test]
fn coding_modes_carry_their_tier() {
    assert_eq!(
        parse("/plan write the thing"),
        Some(SlashCommand::Coding {
            mode: CodingGoalMode::Plan,
            project: None,
            text: "write the thing".into(),
        })
    );
    assert_eq!(
        parse("/explore in proj how sessions park"),
        Some(SlashCommand::Coding {
            mode: CodingGoalMode::Explore,
            project: Some("proj".into()),
            text: "how sessions park".into(),
        })
    );
}

#[test]
fn goal_lifecycle_first_words_stay_reserved() {
    assert_eq!(parse("/goal"), Some(SlashCommand::Goal(GoalCmd::View)));
    assert_eq!(
        parse("/goal status"),
        Some(SlashCommand::Goal(GoalCmd::Status))
    );
    assert_eq!(
        parse("/goal resume yes do it"),
        Some(SlashCommand::Goal(GoalCmd::Resume("yes do it".into())))
    );
}
