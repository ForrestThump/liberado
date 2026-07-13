use crate::commands::{SessionCmd, SlashCommand, ThemeCmd};
use crate::context::CommandContext;
use crate::handlers;
use crate::result::CommandResult;

pub fn parse(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
    let cmd = parts[0];
    match cmd {
        "/quit" => Some(SlashCommand::Quit),
        "/exit" => Some(SlashCommand::Exit),
        "/new" => Some(SlashCommand::New),
        "/clear" => Some(SlashCommand::Clear),
        "/help" => Some(SlashCommand::Help),
        "/status" => Some(SlashCommand::Status),
        "/theme" => Some(parse_theme(parts.get(1).copied(), parts.get(2).copied())),
        "/model" => Some(SlashCommand::Model),
        // `/session` (singular) = conversation subcommands; `/sessions` (plural) = the unified
        // session switcher (primary chat + goal sessions).
        "/session" => Some(parse_session(parts.get(1).copied(), parts.get(2).copied())),
        "/sessions" => Some(SlashCommand::Sessions),
        "/join" => Some(SlashCommand::Join(parts.get(1).copied().unwrap_or("").to_string())),
        "/spawn" => Some(SlashCommand::Spawn {
            domain: parts.get(1).copied().unwrap_or("").to_string(),
            goal: parts.get(2).copied().unwrap_or("").to_string(),
        }),
        "/back" => Some(SlashCommand::Back),
        "/fork" => Some(SlashCommand::Fork),
        _ => None,
    }
}

fn parse_theme(sub: Option<&str>, extra: Option<&str>) -> SlashCommand {
    match sub.unwrap_or("") {
        "reload" => SlashCommand::Theme(ThemeCmd::Reload),
        "" | "list" => SlashCommand::Theme(ThemeCmd::List),
        "set" => SlashCommand::Theme(ThemeCmd::Set(extra.unwrap_or("").to_string())),
        name => SlashCommand::Theme(ThemeCmd::Set(name.to_string())),
    }
}

fn parse_session(sub: Option<&str>, extra: Option<&str>) -> SlashCommand {
    match sub.unwrap_or("") {
        "close" => SlashCommand::Session(SessionCmd::Close),
        "switch" => SlashCommand::Session(SessionCmd::Switch(extra.unwrap_or("").to_string())),
        "list" | "" => SlashCommand::Session(SessionCmd::List),
        "info" => SlashCommand::Session(SessionCmd::Info),
        other => SlashCommand::Session(SessionCmd::Unknown(other.to_string())),
    }
}

pub fn dispatch(cmd: &SlashCommand, ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    match cmd {
        SlashCommand::Quit | SlashCommand::Exit => handlers::quit::handle(ctx),
        SlashCommand::New => handlers::new::handle(ctx),
        SlashCommand::Clear => handlers::clear::handle(ctx),
        SlashCommand::Help => handlers::help::handle(ctx),
        SlashCommand::Status => handlers::status::handle(ctx),
        SlashCommand::Theme(cmd) => handlers::theme::handle(cmd, ctx),
        SlashCommand::Model => handlers::model::handle(ctx),
        SlashCommand::Session(cmd) => handlers::session::handle(cmd, ctx),
        SlashCommand::Sessions => handlers::focus::open_switcher(ctx),
        SlashCommand::Join(id) => handlers::focus::join(id, ctx),
        SlashCommand::Spawn { domain, goal } => handlers::focus::spawn(domain, goal, ctx),
        SlashCommand::Back => handlers::focus::back(ctx),
        SlashCommand::Fork => handlers::fork::handle(ctx),
    }
}
