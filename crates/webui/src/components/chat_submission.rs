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
