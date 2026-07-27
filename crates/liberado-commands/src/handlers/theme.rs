use crate::commands::ThemeCmd;
use crate::context::CommandContext;
use crate::result::CommandResult;

pub fn handle(cmd: &ThemeCmd, ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    match cmd {
        ThemeCmd::Reload => match ctx.reload_themes() {
            Ok(count) => {
                ctx.push_system_message(format!("Themes reloaded — {count} available"));
                vec![CommandResult::ThemesReloaded {
                    count,
                    errors: Vec::new(),
                }]
            }
            Err(errors) => {
                for e in &errors {
                    ctx.push_system_message(format!("theme error: {e}"));
                }
                vec![CommandResult::ThemesReloaded { count: 0, errors }]
            }
        },
        ThemeCmd::List => {
            let names = ctx.theme_names();
            let current = ctx.current_theme_name().to_string();
            let options: Vec<(String, String)> = names
                .iter()
                .map(|n| {
                    let label = if *n == current {
                        format!("  {n}  (active)")
                    } else {
                        format!("    {n}")
                    };
                    (label, n.clone())
                })
                .collect();
            // Both: `ShowOptions` is the list a text surface prints, `OpenThemeBrowser` is the
            // cue for a surface that has a picker. Emitting only the latter would silently make
            // `/theme list` print nothing on the TUI and CLI.
            vec![
                CommandResult::ShowOptions {
                    title: "Available themes".into(),
                    options,
                },
                CommandResult::OpenThemeBrowser,
            ]
        }
        ThemeCmd::Set(name) => {
            if name.is_empty() {
                ctx.push_system_message(
                    "Usage: /theme set <name>\nUse /theme list to see available themes".into(),
                );
                vec![CommandResult::None]
            } else if ctx.set_theme(name) {
                ctx.push_system_message(format!("Theme: {name}"));
                vec![CommandResult::ThemeChanged { name: name.clone() }]
            } else {
                let names = ctx.theme_names();
                ctx.push_system_message(format!(
                    "Unknown theme: {name}. Available: {}\nUsage: /theme set <name>  |  /theme list  |  /theme reload",
                    names.join(", ")
                ));
                vec![CommandResult::None]
            }
        }
    }
}
