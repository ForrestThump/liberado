

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

#[test]
fn a_profile_turn_budget_of_zero_names_that_profile() {
    let mut cfg = validatable();
    cfg.topology.session_profiles = vec![SessionProfile {
        name: "budget-bot".into(),
        enabled: false,
        max_turns: Some(0),
        ..SessionProfile::empty("budget-bot")
    }];
    let err = cfg.validate().unwrap_err().to_string();
    assert!(
        err.contains("topology.session_profiles['budget-bot'].max_turns must be >= 1"),
        "{err}"
    );
}

#[test]
fn a_profile_turn_budget_above_zero_validates() {
    let mut cfg = validatable();
    cfg.topology.session_profiles = vec![SessionProfile {
        name: "budget-bot".into(),
        enabled: false,
        max_turns: Some(18),
        ..SessionProfile::empty("budget-bot")
    }];
    assert!(cfg.validate().is_ok(), "{:?}", cfg.validate().err());
}
