//! Split from `mode.rs` for module-health boundaries.

use super::*;

#[test]
fn parse_aliases() {
    assert_eq!(AgentMode::parse("coding"), Some(AgentMode::Coding));
    assert_eq!(AgentMode::parse("CODE"), Some(AgentMode::Coding));
    assert_eq!(AgentMode::parse("interactive"), Some(AgentMode::Coding));
    assert_eq!(AgentMode::parse("goal"), Some(AgentMode::Goal));
    assert_eq!(AgentMode::parse("pack"), Some(AgentMode::Goal));
    assert_eq!(AgentMode::parse("unattended"), Some(AgentMode::Goal));
    assert_eq!(AgentMode::parse("chat"), Some(AgentMode::Chat));
    assert_eq!(AgentMode::parse("face"), Some(AgentMode::Face));
    assert_eq!(AgentMode::parse("delegate"), Some(AgentMode::Face));
    assert_eq!(AgentMode::parse("nope"), None);
}

#[test]
fn coding_is_converse_with_tools_goal_is_not() {
    assert!(AgentMode::Coding.is_converse());
    assert!(AgentMode::Coding.uses_coding_tools());
    assert!(AgentMode::Chat.is_converse());
    assert!(!AgentMode::Chat.uses_coding_tools());
    assert!(!AgentMode::Goal.is_converse());
    assert!(!AgentMode::Goal.uses_coding_tools());
    assert!(!AgentMode::Face.is_converse());
}

#[test]
fn mode_state_lists_all_four() {
    let v = mode_state_json(AgentMode::Chat);
    assert_eq!(v["currentModeId"], "chat");
    let modes = v["availableModes"].as_array().unwrap();
    assert_eq!(modes.len(), 4);
    let ids: Vec<&str> = modes.iter().filter_map(|m| m["id"].as_str()).collect();
    assert_eq!(ids, ["coding", "goal", "chat", "face"]);
}

#[test]
fn info_table_matches_enum_order() {
    for (i, mode) in AgentMode::ALL.iter().enumerate() {
        assert_eq!(
            mode.id(),
            MODE_INFO[i].id,
            "AgentMode discriminant {i} must match MODE_INFO"
        );
    }
}

#[test]
fn expected_string_matches_parseable_ids() {
    for id in AgentMode::EXPECTED.split('|') {
        assert!(
            AgentMode::parse(id).is_some(),
            "{id} is listed in EXPECTED but parse rejects it"
        );
    }
}

/// Names and descriptions are pinned as literals: an accessor replaced by "" or a
/// placeholder must fail here, not just fail to be empty.
#[test]
fn names_and_descriptions_are_pinned() {
    use AgentMode::*;
    assert_eq!(Coding.name(), "Coding");
    assert_eq!(Goal.name(), "Goal");
    assert_eq!(Chat.name(), "Chat");
    assert_eq!(Face.name(), "Face agent");

    let descriptions = [
        (Coding.description(), "Interactive coding"),
        (Goal.description(), "One-shot /goal"),
        (Chat.description(), "conversational chat"),
        (Face.description(), "Daemon face agent"),
    ];
    for (text, needle) in descriptions {
        assert!(
            text.contains(needle),
            "description must mention {needle}: {text}"
        );
    }
}
