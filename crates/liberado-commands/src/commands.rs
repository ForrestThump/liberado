#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Quit,
    Exit,
    New,
    Clear,
    Help,
    Status,
    Theme(ThemeCmd),
    Model,
    /// Open the session-profile picker: which tools (and dispatch mode) *this chat* runs under.
    ///
    /// Per-conversation and switchable. A human-only act by construction — the agent has no tool
    /// that reaches it.
    ///
    /// This used to read "unlike `/model`'s process-wide hot-swap". That contrast is gone: WebUI,
    /// TUI and Telegram all scope `/model` to the open conversation now, and only a `/model` with
    /// no conversation to bind to still moves the daemon-wide default.
    Profile,
    Session(SessionCmd),
    /// Open the unified session switcher (primary chat + goal sessions in one list).
    Sessions,
    /// Join a goal session by id (or id prefix), moving input focus onto it.
    Join(String),
    /// Start a new interactive goal session and focus it: `/spawn <domain> <goal>`.
    Spawn {
        domain: String,
        goal: String,
    },
    /// Return input focus to the primary chat.
    Back,
    /// Branch this conversation, keeping the original (`/fork`, or `/fork <turn>`).
    ///
    /// `after_turn` = keep through your Nth turn and its reply, dropping everything after it — "go
    /// back to just after turn N and take a different path". `None` forks the whole conversation as
    /// it stands, which is a *snapshot*: continuing the original afterwards does not move the fork.
    Fork {
        after_turn: Option<u32>,
    },
    /// Coding-goal surface (S2/G2): `/goal <text>`, `/goal in <project> <text>`, and the
    /// lifecycle subcommands.
    Goal(GoalCmd),
    /// Plan mode coding goal (`/plan <text>`, `/plan in <project> <text>`).
    ///
    /// Same coding pack as `/goal`, but the session payload sets `plan_mode` so the pack applies
    /// restricted path/command policy (exclusive plan-file writes). Not a second engine.
    Plan {
        project: Option<String>,
        text: String,
    },
}

/// `/goal` subcommands.
///
/// `Start` is the default reading of a bare argument, so `/goal add a --version flag` works
/// without ceremony. The lifecycle words (`status`, `pause`, `resume`, `clear`) are reserved
/// first-words; an unrecognized first word is goal *text*, not an error — refusing to start a goal
/// because it happened to begin with an unknown verb would be worse than occasionally
/// misinterpreting one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalCmd {
    /// Start a coding goal. `project` comes from `/goal in <project> …`; `None` uses the surface's
    /// current project context.
    Start {
        project: Option<String>,
        text: String,
    },
    /// Bare `/goal` — open the goal view for the focused session.
    View,
    /// Snapshot of the focused goal session.
    Status,
    /// Ask the focused session to park (graceful; resumable).
    Pause,
    /// Resume a parked session, optionally answering the question it holds.
    Resume(String),
    /// Cancel the focused session (terminal, not resumable).
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeCmd {
    List,
    Set(String),
    Reload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCmd {
    Info,
    List,
    Switch(String),
    Close,
    Unknown(String),
}

#[cfg(test)]
impl std::fmt::Display for SlashCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlashCommand::Quit => write!(f, "/quit"),
            SlashCommand::Exit => write!(f, "/exit"),
            SlashCommand::New => write!(f, "/new"),
            SlashCommand::Clear => write!(f, "/clear"),
            SlashCommand::Help => write!(f, "/help"),
            SlashCommand::Status => write!(f, "/status"),
            SlashCommand::Theme(cmd) => match cmd {
                ThemeCmd::List => write!(f, "/theme list"),
                ThemeCmd::Set(name) => write!(f, "/theme set {name}"),
                ThemeCmd::Reload => write!(f, "/theme reload"),
            },
            SlashCommand::Model => write!(f, "/model"),
            SlashCommand::Profile => write!(f, "/profile"),
            SlashCommand::Session(cmd) => match cmd {
                SessionCmd::Info => write!(f, "/session info"),
                SessionCmd::List => write!(f, "/session list"),
                SessionCmd::Switch(id) => write!(f, "/session switch {id}"),
                SessionCmd::Close => write!(f, "/session close"),
                SessionCmd::Unknown(sub) => write!(f, "/session {sub}"),
            },
            SlashCommand::Sessions => write!(f, "/sessions"),
            SlashCommand::Join(id) => write!(f, "/join {id}"),
            SlashCommand::Spawn { domain, goal } => write!(f, "/spawn {domain} {goal}"),
            SlashCommand::Goal(g) => match g {
                GoalCmd::Start {
                    project: Some(p),
                    text,
                } => write!(f, "/goal in {p} {text}"),
                GoalCmd::Start {
                    project: None,
                    text,
                } => write!(f, "/goal {text}"),
                GoalCmd::View => write!(f, "/goal"),
                GoalCmd::Status => write!(f, "/goal status"),
                GoalCmd::Pause => write!(f, "/goal pause"),
                GoalCmd::Resume(a) if a.is_empty() => write!(f, "/goal resume"),
                GoalCmd::Resume(a) => write!(f, "/goal resume {a}"),
                GoalCmd::Clear => write!(f, "/goal clear"),
            },
            SlashCommand::Plan {
                project: Some(p),
                text,
            } => write!(f, "/plan in {p} {text}"),
            SlashCommand::Plan {
                project: None,
                text,
            } => write!(f, "/plan {text}"),
            SlashCommand::Back => write!(f, "/back"),
            SlashCommand::Fork { after_turn: None } => write!(f, "/fork"),
            SlashCommand::Fork {
                after_turn: Some(n),
            } => write!(f, "/fork {n}"),
        }
    }
}
