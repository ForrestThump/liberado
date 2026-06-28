use crossterm::event::{KeyCode, KeyEvent};
use liberado_commands::CommandContext;

use crate::app::{App, Effect};

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Some(ref mut dialog) = app.dialog else {
        return vec![Effect::None];
    };
    let total_lines = dialog.lines.len() + dialog.options.len();
    let option_start = dialog.lines.len();

    match key.code {
        KeyCode::Esc => {
            app.dialog = None;
            vec![Effect::None]
        }
        KeyCode::Enter => {
            if total_lines == 0 {
                app.dialog = None;
                return vec![Effect::None];
            }
            if dialog.cursor >= option_start {
                let idx = dialog.cursor - option_start;
                if idx < dialog.options.len() {
                    let selected = dialog.options[idx].1.clone();
                    let title = dialog.title.clone();
                    app.dialog = None;
                    return dialog_selected(app, &title, selected);
                }
            }
            app.dialog = None;
            vec![Effect::None]
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if total_lines > 0 && dialog.cursor > 0 {
                dialog.cursor -= 1;
            }
            vec![Effect::None]
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if dialog.cursor + 1 < total_lines {
                dialog.cursor += 1;
            }
            vec![Effect::None]
        }
        _ => vec![Effect::None],
    }
}

fn dialog_selected(app: &mut App, title: &str, value: String) -> Vec<Effect> {
    match title {
        "Available themes" => {
            app.set_theme(&value);
            vec![Effect::None]
        }
        "Conversations" => {
            app.pending_load = Some(value.clone());
            vec![Effect::LoadConversationHistory(value)]
        }
        _ => vec![Effect::None],
    }
}
