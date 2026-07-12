use crate::catalog::COMMAND_CATALOG;
use crate::context::CommandContext;
use crate::result::CommandResult;

fn help_text() -> String {
    let mut out = String::from("Slash commands:\n\n");
    // Top-level + unique names only (skip subcommand rows that share a parent line in help).
    let mut seen = std::collections::HashSet::new();
    for spec in COMMAND_CATALOG {
        let top = spec.name.split_whitespace().next().unwrap_or(spec.name);
        if !seen.insert(top) {
            continue;
        }
        // Prefer the short catalog row for the top-level name when present.
        let row = COMMAND_CATALOG
            .iter()
            .find(|s| s.name == top)
            .unwrap_or(spec);
        out.push_str(&format!("  {:12} {}\n", row.name, row.description));
    }
    out.push_str("\nType / and use ↑/↓ + Tab to autocomplete.");
    out
}

pub fn handle(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    ctx.push_system_message(help_text());
    vec![CommandResult::HelpShown]
}
