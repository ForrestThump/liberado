//! Matrix command lines for the shared slash parser.
//!
//! A command is the whole trimmed message. A leading `/` or `!` is optional.
//! The command word is case-insensitive. The words and the argument grammar come from
//! [`liberado_commands::parse`]. A sentence that only starts with a command word stays chat
//! text: `status of the build` is not `/status`.

use liberado_commands::{SlashCommand, parse};

/// Slash line to hand to the text bridge, when `text` is a command.
pub fn command_line(text: &str) -> Option<String> {
    let (prefixed, body) = split_sigil(text.trim());
    if body.is_empty() {
        return None;
    }
    let slash = slash_form(body);
    if prefixed {
        return Some(slash);
    }
    strict_unprefixed(&slash)
}

fn split_sigil(text: &str) -> (bool, &str) {
    if let Some(rest) = text.strip_prefix('/') {
        return (true, rest.trim());
    }
    if let Some(rest) = text.strip_prefix('!') {
        return (true, rest.trim());
    }
    (false, text)
}

fn slash_form(body: &str) -> String {
    let mut parts = body.split_whitespace();
    let Some(word) = parts.next() else {
        return String::new();
    };
    let mut slash = format!("/{}", word.to_ascii_lowercase());
    for part in parts {
        slash.push(' ');
        slash.push_str(part);
    }
    slash
}

fn strict_unprefixed(slash: &str) -> Option<String> {
    let parsed = parse(slash)?;
    if has_argument(slash) && ignores_arguments(&parsed) && !model_argument(&parsed, slash) {
        return None;
    }
    Some(slash.to_string())
}

fn has_argument(slash: &str) -> bool {
    slash.split_whitespace().nth(1).is_some()
}

fn ignores_arguments(command: &SlashCommand) -> bool {
    matches!(
        command,
        SlashCommand::Quit
            | SlashCommand::Exit
            | SlashCommand::New
            | SlashCommand::Clear
            | SlashCommand::Help
            | SlashCommand::Status
            | SlashCommand::Model
            | SlashCommand::Profile
            | SlashCommand::Sessions
            | SlashCommand::Back
    )
}

fn model_argument(command: &SlashCommand, slash: &str) -> bool {
    matches!(command, SlashCommand::Model) && has_argument(slash)
}

#[cfg(test)]
mod tests {
    use super::command_line;

    #[test]
    fn status_forms_are_commands_and_a_sentence_is_not() {
        assert_eq!(command_line("status").as_deref(), Some("/status"));
        assert_eq!(command_line("/status").as_deref(), Some("/status"));
        assert_eq!(command_line("!status").as_deref(), Some("/status"));
        assert_eq!(command_line("Status").as_deref(), Some("/status"));
        assert_eq!(command_line("  !Status  ").as_deref(), Some("/status"));
        assert_eq!(command_line("status of the build"), None);
    }

    #[test]
    fn model_with_a_name_follows_the_shared_parser() {
        assert_eq!(command_line("model grok").as_deref(), Some("/model grok"));
        assert_eq!(command_line("/model grok").as_deref(), Some("/model grok"));
        assert_eq!(command_line("help").as_deref(), Some("/help"));
        assert_eq!(
            command_line("theme set dark").as_deref(),
            Some("/theme set dark")
        );
    }

    #[test]
    fn a_prefixed_stop_reaches_the_bridge_and_a_bare_word_does_not() {
        assert_eq!(command_line("/stop").as_deref(), Some("/stop"));
        assert_eq!(command_line("!stop").as_deref(), Some("/stop"));
        assert_eq!(command_line("stop"), None);
        assert_eq!(command_line("/"), None);
        assert_eq!(command_line("hello there"), None);
    }
}
