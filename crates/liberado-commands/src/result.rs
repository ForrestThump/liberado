#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    Quit,
    NewConversation { was_streaming: bool },
    ChatCleared,
    HelpShown,
    StatusShown,
    ThemeChanged { name: String },
    ThemesReloaded { count: usize, errors: Vec<String> },
    ThemeListed { names: Vec<String>, active: String },
    ModelInfoShown,
    SessionClosed { id: Option<String> },
    SessionSwitched { id: String },
    SessionInfoShown,
    SessionListed,
    ForkRequested { parent_id: String },
    ShowOptions {
        title: String,
        options: Vec<(String, String)>,
    },
    None,
}
