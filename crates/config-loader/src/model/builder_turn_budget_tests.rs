

#[test]
fn zero_turn_budgets_fail_validation_with_the_exact_key() {
    for key in [
        "topology.direct_max_turns",
        "topology.subagent_max_turns",
        "topology.research_max_turns",
        "topology.main_agent.max_turns",
    ] {
        let mut cfg = validatable();
        match key {
            "topology.direct_max_turns" => cfg.topology.direct_max_turns = Some(0),
            "topology.subagent_max_turns" => cfg.topology.subagent_max_turns = Some(0),
            "topology.research_max_turns" => cfg.topology.research_max_turns = Some(0),
            "topology.main_agent.max_turns" => cfg.topology.main_agent.max_turns = Some(0),
            _ => unreachable!(),
        }
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains(key), "{key} produced: {err}");
    }
}
