use super::chat::ChatMsg;

/// The note a reopened transcript shows when the last turn never produced a reply.
///
/// The spacing inside the sentence is the string the UI already shipped. Keep it.
pub(super) fn unanswered_turn_note() -> ChatMsg {
    ChatMsg {
        role: "system",
        content: "That turn ended without a reply — the daemon most likely                                           restarted mid-answer. Nothing was saved; send again to retry."
            .to_string(),
        thinking_steps: Vec::new(),
    }
}

/// System line after the human switches the session profile from a chat.
pub(super) fn profile_switched_note(name: Option<String>) -> ChatMsg {
    ChatMsg {
        role: "system",
        content: match name {
            Some(n) => format!("Session profile: {n} — applies from your next message."),
            None => "Session profile cleared — back to the default grant,                                              from your next message."
                .to_string(),
        },
        thinking_steps: Vec::new(),
    }
}

/// Resolve what one submit gesture runs. Palette rows carry an exact command so they never depend
/// on a preceding signal update; form/keyboard submits still accept the highlighted completion.
pub(super) fn submission_text(
    raw: &str,
    selected: usize,
    palette_dismissed: bool,
    picked_command: Option<&str>,
) -> String {
    if let Some(command) = picked_command {
        return command.trim().to_string();
    }
    match liberado_commands::accept_completion(raw, selected) {
        Some(completed) if !palette_dismissed => completed.trim().to_string(),
        _ => raw.trim().to_string(),
    }
}
