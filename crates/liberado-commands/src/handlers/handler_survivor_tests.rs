//! Survivor tests for the focus/theme/profile handlers and the coding-mode
//! wire contract.

use crate::commands::{CodingGoalMode, GoalCmd, ThemeCmd};
use crate::handlers::{focus, profile, theme};
use crate::result::CommandResult;
use crate::test_mock::SurvivorCtx;

// ── focus routing ───────────────────────────────────────────────────────────

#[test]
fn switcher_and_back_emit_their_routing_results() {
    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        focus::open_switcher(&mut ctx),
        vec![CommandResult::OpenGoalSwitcher]
    );
    assert!(ctx.input_cleared);

    let mut ctx = SurvivorCtx::new();
    assert_eq!(focus::back(&mut ctx), vec![CommandResult::BackToPrimary]);
}

#[test]
fn join_trims_and_routes_or_prints_usage() {
    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        focus::join("  s1 ", &mut ctx),
        vec![CommandResult::JoinGoalSession { id: "s1".into() }]
    );

    let mut ctx = SurvivorCtx::new();
    assert_eq!(focus::join("   ", &mut ctx), vec![CommandResult::None]);
    assert!(
        ctx.last_message().contains("Usage: /join"),
        "{}",
        ctx.last_message()
    );
}

/// Either missing half is a usage error: a domain without a goal (or the
/// reverse) never reaches the spawner.
#[test]
fn spawn_requires_both_halves() {
    for (domain, goal) in [("", "goal"), ("life", ""), (" ", " "), ("", "")] {
        let mut ctx = SurvivorCtx::new();
        assert_eq!(
            focus::spawn(domain, goal, &mut ctx),
            vec![CommandResult::None],
            "({domain:?}, {goal:?}) must be rejected"
        );
        assert!(
            ctx.last_message().contains("Usage: /spawn"),
            "{}",
            ctx.last_message()
        );
    }

    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        focus::spawn(" life ", "build a thing", &mut ctx),
        vec![CommandResult::SpawnGoalSession {
            domain: "life".into(),
            goal: "build a thing".into(),
        }]
    );
}

// ── /goal ───────────────────────────────────────────────────────────────────

#[test]
fn goal_start_trims_and_filters_project() {
    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        focus::goal(
            &GoalCmd::Start {
                project: Some("   ".into()),
                text: "  add a flag  ".into(),
            },
            &mut ctx
        ),
        vec![CommandResult::StartCodingGoal {
            project: None,
            text: "add a flag".into(),
            mode: None,
        }]
    );
}

#[test]
fn goal_empty_text_prints_usage() {
    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        focus::goal(
            &GoalCmd::Start {
                project: None,
                text: "   ".into(),
            },
            &mut ctx
        ),
        vec![CommandResult::None]
    );
    assert!(
        ctx.last_message().contains("Usage: /goal"),
        "{}",
        ctx.last_message()
    );
}

#[test]
fn goal_lifecycle_variants_route() {
    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        focus::goal(&GoalCmd::View, &mut ctx),
        vec![CommandResult::OpenGoalView]
    );
    assert_eq!(
        focus::goal(&GoalCmd::Status, &mut ctx),
        vec![CommandResult::GoalStatus]
    );
    assert_eq!(
        focus::goal(&GoalCmd::Pause, &mut ctx),
        vec![CommandResult::ParkGoalSession]
    );
    assert_eq!(
        focus::goal(&GoalCmd::Clear, &mut ctx),
        vec![CommandResult::CancelGoalSession]
    );
    assert_eq!(
        focus::goal(&GoalCmd::Resume("  yes  ".into()), &mut ctx),
        vec![CommandResult::ResumeGoalSession {
            answer: "yes".into()
        }]
    );
}

// ── /plan and /explore ──────────────────────────────────────────────────────

#[test]
fn coding_usage_names_its_mode() {
    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        focus::coding(CodingGoalMode::Plan, None, "", &mut ctx),
        vec![CommandResult::None]
    );
    let usage = ctx.last_message().to_string();
    assert!(usage.starts_with("Usage: /plan"), "{usage}");
    assert!(usage.contains("Plan mode:"), "{usage}");

    let mut ctx = SurvivorCtx::new();
    let _ = focus::coding(CodingGoalMode::Explore, None, "", &mut ctx);
    let usage = ctx.last_message().to_string();
    assert!(usage.starts_with("Usage: /explore"), "{usage}");
    assert!(usage.contains("Explore mode:"), "{usage}");
}

#[test]
fn coding_payload_carries_mode_and_trimmed_project() {
    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        focus::coding(
            CodingGoalMode::Explore,
            Some("  proj  "),
            "how it parks",
            &mut ctx
        ),
        vec![CommandResult::StartCodingGoal {
            project: Some("proj".into()),
            text: "how it parks".into(),
            mode: Some(CodingGoalMode::Explore),
        }]
    );
}

/// The wire string is the pack-facing contract; it must survive verbatim.
#[test]
fn coding_mode_wire_strings_are_pinned() {
    assert_eq!(CodingGoalMode::Plan.as_wire_str(), "plan");
    assert_eq!(CodingGoalMode::Explore.as_wire_str(), "explore");
    assert_eq!(CodingGoalMode::Plan.slash(), "/plan");
    assert_eq!(CodingGoalMode::Explore.slash(), "/explore");
}

// ── /theme ──────────────────────────────────────────────────────────────────

#[test]
fn theme_list_marks_only_the_active_theme() {
    let mut ctx = SurvivorCtx::new();
    let results = theme::handle(&ThemeCmd::List, &mut ctx);
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(matches!(results[0], CommandResult::ShowOptions { .. }));
    assert!(matches!(results[1], CommandResult::OpenThemeBrowser));

    let CommandResult::ShowOptions { options, .. } = &results[0] else {
        panic!("expected options");
    };
    assert_eq!(
        options[0],
        ("  dark  (active)".to_string(), "dark".to_string())
    );
    assert_eq!(options[1], ("    light".to_string(), "light".to_string()));
}

#[test]
fn theme_set_paths_agree_with_the_catalog() {
    // Unknown name.
    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        theme::handle(&ThemeCmd::Set("nosuch".into()), &mut ctx),
        vec![CommandResult::None]
    );
    assert!(
        ctx.last_message().starts_with("Unknown theme: nosuch"),
        "{}",
        ctx.last_message()
    );

    // Known name.
    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        theme::handle(&ThemeCmd::Set("light".into()), &mut ctx),
        vec![CommandResult::ThemeChanged {
            name: "light".into()
        }]
    );
    assert_eq!(ctx.last_message(), "Theme: light");

    // Empty name prints usage instead of guessing.
    let mut ctx = SurvivorCtx::new();
    assert_eq!(
        theme::handle(&ThemeCmd::Set(String::new()), &mut ctx),
        vec![CommandResult::None]
    );
    assert!(
        ctx.last_message().contains("Usage: /theme set"),
        "{}",
        ctx.last_message()
    );
}

#[test]
fn theme_reload_reports_count_and_errors() {
    let mut ctx = SurvivorCtx::new();
    let results = theme::handle(&ThemeCmd::Reload, &mut ctx);
    assert_eq!(ctx.last_message(), "Themes reloaded — 2 available");
    assert_eq!(
        results,
        vec![CommandResult::ThemesReloaded {
            count: 2,
            errors: Vec::new()
        }]
    );

    let mut failing = SurvivorCtx::new();
    failing.reload = Err(vec!["bad line 3".into()]);
    let results = theme::handle(&ThemeCmd::Reload, &mut failing);
    assert_eq!(failing.last_message(), "theme error: bad line 3");
    assert_eq!(
        results,
        vec![CommandResult::ThemesReloaded {
            count: 0,
            errors: vec!["bad line 3".into()]
        }]
    );
}

// ── /profile ────────────────────────────────────────────────────────────────

#[test]
fn profile_opens_the_browser_and_explains_itself() {
    let mut ctx = SurvivorCtx::new();
    let results = profile::handle(&mut ctx);
    assert_eq!(
        results,
        vec![
            CommandResult::ProfileInfoShown,
            CommandResult::OpenProfileBrowser
        ]
    );
    assert!(
        ctx.last_message().contains("session profiles"),
        "{}",
        ctx.last_message()
    );
}
