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
    /// Open the client's searchable theme browser (`/theme` / `/theme list`). Emitted alongside
    /// [`CommandResult::ShowOptions`] so a text-only surface still prints the list while a surface
    /// with a picker shows one instead.
    OpenThemeBrowser,
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
    /// Start a new interactive goal session and focus it (`/spawn <domain> <goal>`). The surface
    /// POSTs `/api/goals` (with `origin` = the current conversation) and joins the returned session.
    SpawnGoalSession {
        domain: String,
        goal: String,
    },
    /// Start a **coding** goal session (`/goal <text>`). Distinct from `SpawnGoalSession`: that one
    /// takes a domain/profile token from the human, while this one is always the coding pack and
    /// carries a project instead. `project` is `None` when the surface should use its current
    /// project context.
    StartCodingGoal {
        project: Option<String>,
        text: String,
    },
    /// Open the goal view for the focused session (bare `/goal`).
    OpenGoalView,
    /// Snapshot the focused goal session inline (`/goal status`).
    GoalStatus,
    /// Park the focused goal session — graceful and resumable (`/goal pause`).
    ParkGoalSession,
    /// Resume a parked session, optionally answering its outstanding question (`/goal resume`).
    ResumeGoalSession {
        answer: String,
    },
    /// Cancel the focused goal session — terminal (`/goal clear`).
    CancelGoalSession,
    /// Branch `parent_id`, keeping the original. The surface POSTs `/api/sessions/{id}/fork`, then
    /// switches to the new conversation — you carry on in the branch, and the original is still
    /// there in the switcher, exactly as you left it.
    ForkRequested {
        parent_id: String,
        /// Keep through this turn of yours (1-based). `None` = the whole conversation.
        after_turn: Option<u32>,
    },
    ShowOptions {
        title: String,
        options: Vec<(String, String)>,
    },
    None,
}
