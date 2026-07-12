use crate::context::CommandContext;
use crate::result::CommandResult;

/// Open the client model browser (live catalog from the daemon). Still prints a one-line
/// current-model summary when status is available.
pub fn handle(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    if let Some(ref st) = ctx.status_info() {
        if let Some(ref model) = st.model_name {
            let tokens = st
                .token_usage_total
                .map(|t| t.to_string())
                .unwrap_or_else(|| "--".into());
            let window = st
                .context_window
                .map(|w| w.to_string())
                .unwrap_or_else(|| "--".into());
            ctx.push_system_message(format!(
                "Current model: {model}  (tokens {tokens} / window {window})\n\
                 Opening model browser — type to filter, Enter to switch (hot-swap, no restart), Esc to close."
            ));
        } else {
            ctx.push_system_message(
                "Opening model browser (current model not reported by daemon)…".into(),
            );
        }
    } else {
        ctx.push_system_message(
            "Not connected to daemon — model browser will show an error until connected.".into(),
        );
    }
    vec![
        CommandResult::ModelInfoShown,
        CommandResult::OpenModelBrowser,
    ]
}
