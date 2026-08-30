//! Tests for daemon helper mappings, schedule naming, and cron delivery options.

use super::super::*;
use crate::helpers::*;
use liberado_common::event_source;
use liberado_session::TerminalKind;

#[test]
fn grant_component_maps_default_pool_to_the_dispatcher_ceiling() {
    // A permission request stamps the owning pool; the default pool's authority ceiling is the
    // "dispatcher" grant (see configure_daemon), so an "everywhere" grant must land there — not
    // on a literal "default" component that grants nothing.
    assert_eq!(grant_component_for_pool(None), "dispatcher");
    assert_eq!(grant_component_for_pool(Some(DEFAULT_POOL)), "dispatcher");
    // A named pool's ceiling is its own name.
    assert_eq!(grant_component_for_pool(Some("research")), "research");
}

#[test]
fn cron_schedule_name_only_matches_cron_sources() {
    assert_eq!(
        cron_schedule_name("cron:daily-planning"),
        Some("daily-planning")
    );
    // Names may themselves contain colons (rfc3339-ish); only the first split matters.
    assert_eq!(
        cron_schedule_name("cron:weekly:review"),
        Some("weekly:review")
    );
    // Non-cron sources must never trigger delivery.
    assert_eq!(
        cron_schedule_name(event_source::TURBOVAULT_SUBSCRIPTION),
        None
    );
    assert_eq!(cron_schedule_name("delegate"), None);
    assert_eq!(cron_schedule_name("cronies:x"), None); // kind must equal "cron", not just prefix
    assert_eq!(cron_schedule_name("cron"), None); // no name
}

/// A schedule's declared ceiling reaches the pack as `GoalSpec::max_turns`.
///
/// The daemon builds the goal from the event alone, so if the payload stopped carrying this the
/// schedule would silently fall back to the path default — the exact failure the field exists to
/// prevent, and one that looks like the agent simply running out of turns.
#[test]
fn a_schedules_max_turns_reaches_the_goal_spec() {
    use crate::helpers::reaction_goal;
    use liberado_common::{Event, EventPayload};

    let with = |data: serde_json::Value| {
        Event::trigger(
            "CronFired",
            "cron:bigjob",
            "c1",
            EventPayload {
                data,
                ..Default::default()
            },
        )
    };

    assert_eq!(
        reaction_goal(
            &with(serde_json::json!({"max_turns": 20})),
            "do it",
            "default"
        )
        .max_turns,
        20
    );

    // Absent, null, and a non-number all mean "pack default" (0) — the behaviour every schedule
    // had before the field existed. Anything else would change existing deployments silently.
    for payload in [
        serde_json::json!({}),
        serde_json::Value::Null,
        serde_json::json!({"max_turns": "20"}),
    ] {
        assert_eq!(
            reaction_goal(&with(payload), "do it", "default").max_turns,
            0
        );
    }

    // Coexists with the other payload riders.
    assert_eq!(
        reaction_goal(
            &with(serde_json::json!({"profile": "hat", "deliver": false, "max_turns": 12})),
            "do it",
            "default"
        )
        .max_turns,
        12
    );
}

#[test]
fn cron_delivery_is_suppressed_only_by_an_explicit_false() {
    use crate::helpers::cron_delivery_suppressed;
    use liberado_common::{Event, EventPayload};

    let with = |data: serde_json::Value| {
        Event::trigger(
            "CronFired",
            "cron:sweep",
            "c1",
            EventPayload {
                data,
                ..Default::default()
            },
        )
    };

    assert!(cron_delivery_suppressed(&with(
        serde_json::json!({"deliver": false})
    )));

    // Everything else delivers. `Null` and a missing key are what every event looked like before
    // the flag existed, so treating either as "suppress" would silence the whole system.
    assert!(!cron_delivery_suppressed(&with(
        serde_json::json!({"deliver": true})
    )));
    assert!(!cron_delivery_suppressed(&with(serde_json::json!({}))));
    assert!(!cron_delivery_suppressed(&with(serde_json::Value::Null)));
    // A non-bool is a config mistake; delivering is the safe reading of an unclear answer.
    assert!(!cron_delivery_suppressed(&with(
        serde_json::json!({"deliver": "false"})
    )));
    // Coexists with profile, which shares the payload map.
    assert!(cron_delivery_suppressed(&with(
        serde_json::json!({"profile": "hat", "deliver": false})
    )));
}

#[test]
fn format_cron_delivery_flags_non_success() {
    let ok = format_cron_delivery("daily-planning", "your brief", TerminalKind::Succeeded);
    assert!(ok.contains("daily-planning") && ok.contains("your brief"));
    assert!(
        !ok.contains('['),
        "success must not carry a status tag: {ok}"
    );

    for bad in [
        TerminalKind::Failed,
        TerminalKind::Cancelled,
        TerminalKind::BudgetExhausted,
    ] {
        let msg = format_cron_delivery("daily-planning", "partial", bad);
        assert!(
            msg.contains(&format!("[{bad:?}]")),
            "non-success must be tagged so it isn't mistaken for a real report: {msg}"
        );
    }
}
