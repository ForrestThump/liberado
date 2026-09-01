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
