use crate::context::CommandContext;
use crate::result::CommandResult;

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
                "Model: {model}\nTokens used: {tokens} / {window}"
            ));
        } else {
            ctx.push_system_message("Model is configured server-side at daemon start.\nThe server has not yet exposed the model name via /api/status.".into());
        }
    } else {
        ctx.push_system_message(
            "Not connected to daemon.\nModel is configured server-side at daemon start.".into(),
        );
    }
    vec![CommandResult::ModelInfoShown]
}
