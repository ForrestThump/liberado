//! Split from `tests.rs` for module-health boundaries: the test-only `Display` impl
//! for `SlashCommand`, with every variant's slash text pinned.

use crate::commands::*;

/// Every SlashCommand variant renders its canonical slash text. The `Display` impl is
/// test-only, so this is also what keeps it from being dead code — and what pins the exact
/// strings the parser is expected to round-trip.
#[test]
fn every_slash_command_renders_its_slash_text() {
    use std::fmt::Write as _;

    let cases: Vec<(SlashCommand, &str)> = vec![
        (SlashCommand::Quit, "/quit"),
        (SlashCommand::Exit, "/exit"),
        (SlashCommand::New, "/new"),
        (SlashCommand::Clear, "/clear"),
        (SlashCommand::Help, "/help"),
        (SlashCommand::Status, "/status"),
        (SlashCommand::Theme(ThemeCmd::List), "/theme list"),
        (SlashCommand::Theme(ThemeCmd::Reload), "/theme reload"),
        (
            SlashCommand::Theme(ThemeCmd::Set("nord".into())),
            "/theme set nord",
        ),
        (SlashCommand::Model, "/model"),
        (SlashCommand::Profile, "/profile"),
        (SlashCommand::Session(SessionCmd::Info), "/session info"),
        (SlashCommand::Session(SessionCmd::List), "/session list"),
        (
            SlashCommand::Session(SessionCmd::Switch("abc".into())),
            "/session switch abc",
        ),
        (SlashCommand::Session(SessionCmd::Close), "/session close"),
        (
            SlashCommand::Session(SessionCmd::Unknown("zzz".into())),
            "/session zzz",
        ),
        (SlashCommand::Sessions, "/sessions"),
        (SlashCommand::Join("g_01".into()), "/join g_01"),
        (
            SlashCommand::Spawn {
                domain: "life".into(),
                goal: "plan week".into(),
            },
            "/spawn life plan week",
        ),
        (SlashCommand::Back, "/back"),
        (SlashCommand::Fork { after_turn: None }, "/fork"),
        (
            SlashCommand::Fork {
                after_turn: Some(3),
            },
            "/fork 3",
        ),
        (SlashCommand::Goal(GoalCmd::View), "/goal"),
        (
            SlashCommand::Goal(GoalCmd::Start {
                project: None,
                text: "fix it".into(),
            }),
            "/goal fix it",
        ),
        (
            SlashCommand::Goal(GoalCmd::Start {
                project: Some("lib".into()),
                text: "fix it".into(),
            }),
            "/goal in lib fix it",
        ),
        (SlashCommand::Goal(GoalCmd::Status), "/goal status"),
        (SlashCommand::Goal(GoalCmd::Pause), "/goal pause"),
        (
            SlashCommand::Goal(GoalCmd::Resume(String::new())),
            "/goal resume",
        ),
        (
            SlashCommand::Goal(GoalCmd::Resume("yes".into())),
            "/goal resume yes",
        ),
        (SlashCommand::Goal(GoalCmd::Clear), "/goal clear"),
        (
            SlashCommand::Coding {
                mode: CodingGoalMode::Plan,
                project: None,
                text: "write the design".into(),
            },
            "/plan write the design",
        ),
        (
            SlashCommand::Coding {
                mode: CodingGoalMode::Explore,
                project: Some("demo".into()),
                text: "map the code".into(),
            },
            "/explore in demo map the code",
        ),
    ];

    for (command, expected) in cases {
        let mut rendered = String::new();
        write!(rendered, "{command}").unwrap();
        assert_eq!(rendered, expected, "for {command:?}");
    }
}
