use super::*;

#[test]
fn every_turn_budget_parses_at_its_documented_scope() {
    let topology: Topology = toml::from_str(
        r#"
            vault_path = "/vault"
            direct_max_turns = 8
            subagent_max_turns = 20
            research_max_turns = 30

            [main_agent]
            max_turns = 12
        "#,
    )
    .unwrap();

    assert_eq!(topology.direct_max_turns, Some(8));
    assert_eq!(topology.subagent_max_turns, Some(20));
    assert_eq!(topology.research_max_turns, Some(30));
    assert_eq!(topology.main_agent.max_turns, Some(12));
}

#[test]
fn a_session_profile_turn_budget_parses_and_stays_absent_when_omitted() {
    let topology: Topology = toml::from_str(
        r#"
            vault_path = "/vault"
            [[session_profiles]]
            name = "budget-bot"
            max_turns = 18
            [[session_profiles]]
            name = "chat-default"
        "#,
    )
    .unwrap();

    assert_eq!(topology.session_profiles[0].max_turns, Some(18));
    assert_eq!(topology.session_profiles[1].max_turns, None);
}
