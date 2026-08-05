//! Session-focus commands (`/sessions`, `/join <id>`, `/back`) for the unified-`Session` model.
//!
//! These are thin: the handler validates input and emits a [`CommandResult`] the surface maps to
//! its own focus machinery (open the switcher, subscribe to a goal-session stream, or return to the
//! primary chat). No shared state changes here — focus is a per-surface concern.

use crate::commands::GoalCmd;
use crate::context::CommandContext;
use crate::result::CommandResult;

/// `/sessions` — open the unified session switcher (primary chat + goal sessions in one list).
pub fn open_switcher(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    vec![CommandResult::OpenGoalSwitcher]
}

/// `/join <id>` — move input focus onto a goal session by id (or id prefix). Empty id prints usage.
pub fn join(id: &str, ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    let id = id.trim();
    if id.is_empty() {
        ctx.push_system_message(
            "Usage: /join <session-id>\nOr run /sessions to pick one from the switcher.".into(),
        );
        return vec![CommandResult::None];
    }
    vec![CommandResult::JoinGoalSession { id: id.to_string() }]
}

/// `/back` — return input focus to the primary chat, leaving any joined goal session.
pub fn back(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    vec![CommandResult::BackToPrimary]
}

/// `/spawn <profile|domain> <goal>` — start a new interactive goal session and focus it. The first
/// argument may name a domain pack (`life`, `coding`) or a `[[session_profiles]]` hat (`research`);
/// the server resolves profile-first. Empty argument prints usage.
pub fn spawn(domain: &str, goal: &str, ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    let domain = domain.trim();
    let goal = goal.trim();
    if domain.is_empty() || goal.is_empty() {
        ctx.push_system_message(
            "Usage: /spawn <profile|domain> <goal>\n\
             e.g. /spawn life \"plan my week\"   (the life pack)\n\
             e.g. /spawn research \"compare X\"  (a narrower hat on the same pack)"
                .into(),
        );
        return vec![CommandResult::None];
    }
    vec![CommandResult::SpawnGoalSession {
        domain: domain.to_string(),
        goal: goal.to_string(),
    }]
}

/// `/goal …` — the coding-goal surface (S2/G2).
///
/// Every arm returns a `CommandResult` for the surface to act on rather than doing HTTP here: this
/// crate is shared by the TUI and Telegram, and only the surface knows its own client and its
/// current project/session context.
pub fn goal(cmd: &GoalCmd, ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    match cmd {
        GoalCmd::Start { project, text } => {
            let text = text.trim();
            if text.is_empty() {
                ctx.push_system_message(
                    "Usage: /goal <what you want built>
                     e.g. /goal add a --version flag to the CLI
                     e.g. /goal in liberado add a --version flag   (explicit project)
                     
                     Also: /goal status | /goal pause | /goal resume [answer] | /goal clear"
                        .into(),
                );
                return vec![CommandResult::None];
            }
            vec![CommandResult::StartCodingGoal {
                project: project.clone().filter(|p| !p.trim().is_empty()),
                text: text.to_string(),
                explore_mode: false,
            }]
        }
        GoalCmd::View => vec![CommandResult::OpenGoalView],
        GoalCmd::Status => vec![CommandResult::GoalStatus],
        GoalCmd::Pause => vec![CommandResult::ParkGoalSession],
        GoalCmd::Resume(answer) => vec![CommandResult::ResumeGoalSession {
            answer: answer.trim().to_string(),
        }],
        GoalCmd::Clear => vec![CommandResult::CancelGoalSession],
    }
}

/// `/explore …` — coding goal in explore mode (read-only tools, no shell).
pub fn explore(
    project: Option<&str>,
    text: &str,
    ctx: &mut dyn CommandContext,
) -> Vec<CommandResult> {
    ctx.clear_input();
    let text = text.trim();
    if text.is_empty() {
        ctx.push_system_message(
            "Usage: /explore <what to investigate>
             e.g. /explore how auth middleware is wired
             e.g. /explore in liberado how sessions park

             Explore mode: read-only tools only (list/search/read/git status|diff); no writes or shell.
             Findings return as the session summary — use a normal /goal to implement afterward."
                .into(),
        );
        return vec![CommandResult::None];
    }
    vec![CommandResult::StartCodingGoal {
        project: project
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string),
        text: text.to_string(),
        explore_mode: true,
    }]
}
