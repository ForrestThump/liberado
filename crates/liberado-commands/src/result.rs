#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    Quit,
    NewConversation {
        was_streaming: bool,
    },
    ChatCleared,
    HelpShown,
    StatusShown,
    ThemeChanged {
        name: String,
    },
    ThemesReloaded {
        count: usize,
        errors: Vec<String>,
    },
    ThemeListed {
        names: Vec<String>,
        active: String,
    },
    ModelInfoShown,
    /// Open the client's searchable model browser (`/model`). TUI maps this to a full-screen
    /// picker that loads `GET /api/models`.
    OpenModelBrowser,
    SessionClosed {
        id: Option<String>,
    },
    SessionSwitched {
        id: String,
    },
    SessionInfoShown,
    SessionListed,
    /// Open the client's full-screen session browser (searchable list). TUI/WebUI map this
    /// to their own UI; CLI may print the list instead.
    OpenSessionBrowser,
    /// Open the unified session switcher (primary chat + goal sessions in one list). TUI maps this
    /// to a full-screen picker over `GET /api/goals`.
    OpenGoalSwitcher,
    /// Move input focus onto a goal session (by id or id prefix), subscribing to its event stream.
    JoinGoalSession {
        id: String,
    },
    /// Return input focus to the primary chat, leaving any joined goal session.
    BackToPrimary,
    ForkRequested {
        parent_id: String,
    },
    ShowOptions {
        title: String,
        options: Vec<(String, String)>,
    },
    None,
}
