//! Integration coverage for `liberado-common`: exercises the public type contract the way
//! consumer crates do — serde round-trips of the typed artifacts, the security invariants, and
//! the config fail-safe defaults.

use liberado_common::*;
use liberado_config_loader::{Config, Policy, ZonePolicy};
use serde::Serialize;
use serde::de::DeserializeOwned;

fn round_trip<T>(value: T) -> T
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(&value).unwrap();
    let back: T = serde_json::from_str(&json).unwrap();
    assert_eq!(value, back, "serde round-trip mismatch");
    back
}

// --- capability ---------------------------------------------------------------------------

#[test]
fn capability_set_serde_round_trip_all_variants() {
    // "All variants" now means all of them. It previously omitted `AskHuman`, and `ExecuteTool` would
    // have made two of six unproven under a name promising otherwise — the failure class
    // `docs/spec/architecture/failure-modes.md` §1 is about.
    //
    // Serde is load-bearing for these in two places: `policy.toml` grants, and the `SessionGrant`
    // written into a session log's header line. A variant that fails to round-trip is a grant that
    // silently does not survive a daemon restart.
    let set = CapabilitySet::from_iter([
        Capability::Read(Zone::vault("tasks")),
        Capability::Write(Zone::vault("decisions")),
        Capability::ReadSummary(Zone::named("finance")),
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::ExecuteTool("turbovault:read_note".into()),
        Capability::AskHuman,
    ]);
    let decoded = round_trip(set);

    // Round-tripping the container is not the same as the semantics surviving it: a decoded set must
    // still answer the authorization question the same way.
    assert!(decoded.grants_tool("turbovault:read_note"));
    assert!(!decoded.grants_tool("turbovault:write_note"));
    assert!(decoded.grants_tool("tasks-mcp:anything"));
    assert!(decoded.grants_ask_human());
}

/// TOML specifically, because that is the shape a human writes in `policy.toml` — and the shape the
/// docs will tell them to write.
#[test]
fn execute_tool_round_trips_through_toml_as_written_in_policy() {
    #[derive(serde::Deserialize)]
    struct Grant {
        capabilities: Vec<Capability>,
    }
    let written = r#"
        capabilities = [
            { ExecuteMcp = "spider-mcp" },
            { ExecuteTool = "turbovault:read_note" },
        ]
    "#;
    let grant: Grant = toml::from_str(written).expect("policy-shaped TOML must parse");
    let set = CapabilitySet::from_iter(grant.capabilities);
    assert!(set.grants_tool("spider-mcp:fetch"));
    assert!(set.grants_tool("turbovault:read_note"));
    assert!(!set.grants_tool("turbovault:write_note"));
}

#[test]
fn grant_is_idempotent() {
    let mut set = CapabilitySet::empty();
    set.grant(Capability::Read(Zone::vault("tasks")));
    set.grant(Capability::Read(Zone::vault("tasks")));
    assert_eq!(set.capabilities.len(), 1);
}

#[test]
fn from_iter_deduplicates() {
    let set = CapabilitySet::from_iter([
        Capability::ExecuteMcp("m".into()),
        Capability::ExecuteMcp("m".into()),
    ]);
    assert_eq!(set.capabilities.len(), 1);
}

#[test]
fn narrow_with_empty_is_empty() {
    let base = CapabilitySet::from_iter([Capability::Read(Zone::vault("tasks"))]);
    assert!(base.narrow(&CapabilitySet::empty()).capabilities.is_empty());
}

#[test]
fn shared_and_agent_writable_allow_direct_write() {
    assert!(WriteClass::Shared.allows_direct_agent_write());
    assert!(WriteClass::AgentWritable.allows_direct_agent_write());
    assert!(!WriteClass::HumanOnly.allows_direct_agent_write());
    assert!(!WriteClass::ProposalOnly.allows_direct_agent_write());
}

#[test]
fn check_covers_execute_mcp_and_read_summary() {
    let set = CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::ReadSummary(Zone::named("finance")),
    ]);
    assert!(
        set.check(&Capability::ExecuteMcp("tasks-mcp".into()))
            .is_ok()
    );
    assert!(set.check(&Capability::ExecuteMcp("other".into())).is_err());
    assert!(
        set.check(&Capability::ReadSummary(Zone::named("finance")))
            .is_ok()
    );
}

// --- provenance ---------------------------------------------------------------------------

#[test]
fn is_human_is_case_insensitive() {
    for s in ["human", "Human", "HUMAN"] {
        let p = WriteProvenance {
            source: s.into(),
            correlation_id: None,
            zone: None,
            note: None,
        };
        assert!(p.is_human(), "{s} should be human");
    }
    assert!(!WriteProvenance::agent("tasks-mcp", "c1").is_human());
}

#[test]
fn malformed_provenance_metadata_is_none() {
    // Right key, wrong shape.
    let meta = serde_json::json!({ PROVENANCE_KEY: "not-an-object" });
    assert!(WriteProvenance::from_audit_metadata(&meta).is_none());
}

#[test]
fn provenance_builders_set_fields_and_omit_none() {
    let p = WriteProvenance::agent("daily-review-agent", "review-1")
        .with_zone("reviews")
        .with_note("nightly");
    assert_eq!(p.zone.as_deref(), Some("reviews"));
    assert_eq!(p.note.as_deref(), Some("nightly"));

    // None fields are skipped in the serialized form.
    let json = serde_json::to_value(WriteProvenance::agent("a", "c")).unwrap();
    assert!(json.get("zone").is_none());
    assert!(json.get("note").is_none());
    assert_eq!(json["correlation_id"], "c");
}

// --- event --------------------------------------------------------------------------------

#[test]
fn human_provenance_event_is_reactable() {
    let mut ev = Event::trigger(
        "NoteEdited",
        event_source::TURBOVAULT_SUBSCRIPTION,
        "edit-1",
        EventPayload {
            path: Some("journal/today.md".into()),
            ..Default::default()
        },
    );
    ev.provenance = Some(WriteProvenance {
        source: "human".into(),
        correlation_id: None,
        zone: None,
        note: None,
    });
    assert!(ev.is_reactable());
    round_trip(ev);
}

#[test]
fn event_with_payload_data_round_trips() {
    let ev = Event::trigger(
        "DockerEvent",
        event_source::DOCKER_EVENT,
        "evt-1",
        EventPayload {
            data: serde_json::json!({ "container": "vault", "status": "restarted" }),
            ..Default::default()
        },
    );
    let back = round_trip(ev);
    assert_eq!(back.payload.data["container"], "vault");
}

// --- dispatch -----------------------------------------------------------------------------

#[test]
fn execute_direct_decision_round_trips() {
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "tasks-mcp:add".into(),
                args: serde_json::json!({ "title": "milk" }),
            }],
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "trivial single-tool add".into(),
    };
    round_trip(decision);
}

#[test]
fn clarify_decision_round_trips() {
    let decision = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["which project?".into()],
            what_blocked: BlockReason::Ambiguous,
        },
        confidence: 0.4,
        rationale: "two plausible interpretations".into(),
    };
    round_trip(decision);
}

#[test]
fn block_reason_serializes_snake_case() {
    assert_eq!(
        serde_json::to_string(&BlockReason::CapabilityGap).unwrap(),
        "\"capability_gap\""
    );
    assert_eq!(
        serde_json::to_string(&BlockReason::LowConfidence).unwrap(),
        "\"low_confidence\""
    );
}

#[test]
fn report_round_trips_each_outcome() {
    for outcome in [
        Outcome::Succeeded,
        Outcome::PartiallySucceeded,
        Outcome::Failed,
        Outcome::Proposed,
    ] {
        round_trip(Report {
            outcome,
            summary: "did the thing".into(),
            artifacts: vec!["reviews/2026-06-21.md".into()],
            new_high_signal_facts: vec![],
            follow_up: None,
            deferred_to_human: false,
            repeat_calls: 0,
        });
    }
}

// --- proposal -----------------------------------------------------------------------------

#[test]
fn proposal_expiry() {
    use chrono::{Duration, Utc};
    let now = Utc::now();

    let mut p = Proposal::pending(
        "p1",
        "c1",
        "decisions-hook",
        ProposedAction::External {
            description: "send".into(),
        },
        "because",
    );
    assert!(!p.is_expired_at(now), "no expiry set");

    p.expires = Some(now - Duration::hours(1));
    assert!(p.is_expired_at(now), "past expiry");

    p.expires = Some(now + Duration::hours(1));
    assert!(!p.is_expired_at(now), "future expiry");
}

#[test]
fn proposed_action_variants_round_trip() {
    round_trip(ProposedAction::ToolCalls(vec![ToolCall {
        tool: "x".into(),
        args: serde_json::json!({}),
    }]));
    round_trip(ProposedAction::AdaptiveGoal {
        goal: "delete all archived tasks".into(),
        capabilities: CapabilitySet::from_iter([Capability::ExecuteMcp("tasks".into())]),
        relevant_mcps: vec!["tasks".into()],
        delivery: Delivery::Summarize,
        approved_guard: ApprovedGuard::Magnitude,
    });
    round_trip(ProposedAction::VaultWrite {
        path: "decisions/x.md".into(),
        content_summary: "log decision".into(),
    });
    round_trip(ProposedAction::External {
        description: "email".into(),
    });
    round_trip(ProposedAction::Other(serde_json::json!({ "k": 1 })));
}

#[test]
fn proposal_status_gating_is_exhaustive() {
    use ProposalStatus::*;
    assert!(Approved.is_actionable());
    for s in [Pending, Rejected, Expired, Done] {
        assert!(!s.is_actionable(), "{s:?} must not be actionable");
    }
    for s in [Rejected, Expired, Done] {
        assert!(s.is_terminal(), "{s:?} must be terminal");
    }
    for s in [Pending, Approved] {
        assert!(!s.is_terminal(), "{s:?} must not be terminal");
    }
}

// --- model --------------------------------------------------------------------------------

#[test]
fn model_without_tool_calling_fails_agent_roles() {
    let p = ModelProfile {
        name: "json-only".into(),
        tool_calling: false,
        structured_output: true,
        context_window: 32_000,
        tier: ModelTier::ControlPlane,
        cost: None,
        prices: Default::default(),
    };
    assert!(!p.meets(ModelRole::MainAgent));
    assert!(!p.meets(ModelRole::Subagent));
    // ...but it satisfies the dispatcher (which only needs structured output).
    assert!(p.meets(ModelRole::Dispatcher));
}

#[test]
fn model_role_labels_and_choice_round_trip() {
    assert_eq!(ModelRole::Dispatcher.as_str(), "dispatcher");
    assert_eq!(ModelRole::MainAgent.as_str(), "main_agent");
    assert_eq!(ModelRole::Subagent.as_str(), "subagent");
    round_trip(ModelChoice::new("deepseek-chat"));
    assert_eq!(
        serde_json::to_string(&ModelTier::WorkPlane).unwrap(),
        "\"work_plane\""
    );
}

// --- config -------------------------------------------------------------------------------

#[test]
fn write_class_is_fail_safe_for_unlisted_zones() {
    let policy = Policy {
        zones: vec![
            ZonePolicy {
                zone: "tasks".into(),
                write_class: WriteClass::Shared,
            },
            ZonePolicy {
                zone: "reviews".into(),
                write_class: WriteClass::AgentWritable,
            },
        ],
        ..Default::default()
    };
    assert_eq!(policy.write_class("tasks"), WriteClass::Shared);
    assert_eq!(policy.write_class("reviews"), WriteClass::AgentWritable);
    // Unlisted zone falls back to the conservative default.
    assert_eq!(
        policy.write_class("something-undeclared"),
        WriteClass::ProposalOnly
    );
}

#[test]
fn validate_rejects_zero_concurrent_subagents() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = "/vault".into();
    cfg.tuning.dispatch.max_concurrent_subagents = 0;
    assert!(cfg.validate().is_err());
}

#[test]
fn validate_rejects_role_referencing_undeclared_model() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = "/vault".into();
    cfg.topology
        .model_roles
        .insert(ModelRole::Dispatcher, "ghost-model".into());
    assert!(cfg.validate().is_err());
}

#[test]
fn validate_accepts_well_formed_model_assignment() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = "/vault".into();
    cfg.topology.models.push(ModelProfile {
        name: "deepseek-chat".into(),
        tool_calling: true,
        structured_output: true,
        context_window: 64_000,
        tier: ModelTier::ControlPlane,
        cost: None,
        prices: Default::default(),
    });
    cfg.topology
        .model_roles
        .insert(ModelRole::Dispatcher, "deepseek-chat".into());
    assert!(cfg.validate().is_ok());
}

#[test]
fn config_serde_round_trips() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = "/home/shiloh/vault".into();
    cfg.policy.zones.push(ZonePolicy {
        zone: "tasks".into(),
        write_class: WriteClass::Shared,
    });
    let json = serde_json::to_string(&cfg).unwrap();
    let back: Config = serde_json::from_str(&json).unwrap();
    assert_eq!(back.topology.vault_path, cfg.topology.vault_path);
    assert_eq!(back.policy.write_class("tasks"), WriteClass::Shared);
    // Defaults survive a round-trip.
    assert_eq!(back.tuning.dispatch.small_fanout, 3);
}
