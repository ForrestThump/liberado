use crate::constants::CTX_PCT_DISPLAY_CAP;
use crate::context::CommandContext;
use crate::format::format_uptime;
use crate::result::CommandResult;

fn state_label(running: bool) -> &'static str {
    if running { "running" } else { "stopped" }
}

fn attached_label(attached: bool) -> &'static str {
    if attached { "attached" } else { "detached" }
}

pub fn handle(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    if let Some(st) = ctx.status_info() {
        let model = st.model_name.as_deref().unwrap_or("(unknown)");
        let tokens = st
            .token_usage_total
            .map(|t| t.to_string())
            .unwrap_or_else(|| "--".into());
        let window = st
            .context_window
            .map(|w| w.to_string())
            .unwrap_or_else(|| "--".into());
        let fill = match (st.token_usage_total, st.context_window) {
            (Some(u), Some(w)) if w > 0 => format!(
                "{}%",
                (u as f64 / w as f64 * 100.0).min(CTX_PCT_DISPLAY_CAP) as u32
            ),
            _ => "--".to_string(),
        };
        let info = format!(
            "Daemon:  {} running\nVault:   {}\nUptime:  {}\nModel:   {model}\nTokens:  {tokens} / {window}  ({fill} context)\nDispatcher:    {}\nOrchestrator:  {}\nReactions seen: {}",
            state_label(st.running),
            st.vault_path,
            format_uptime(st.uptime_seconds),
            attached_label(st.dispatcher_attached),
            attached_label(st.orchestrator_attached),
            st.reactions_seen,
        );
        ctx.push_system_message(info);
    } else {
        ctx.push_system_message("Not connected to daemon — waiting for status poll...".into());
    }
    vec![CommandResult::StatusShown]
}
#[cfg(test)]
#[path = "status_survivor_tests.rs"]
mod survivor_tests;
