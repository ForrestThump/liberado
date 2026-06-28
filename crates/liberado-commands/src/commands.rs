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
    Session(SessionCmd),
    Fork,
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
            SlashCommand::Session(cmd) => match cmd {
                SessionCmd::Info => write!(f, "/session info"),
                SessionCmd::List => write!(f, "/session list"),
                SessionCmd::Switch(id) => write!(f, "/session switch {id}"),
                SessionCmd::Close => write!(f, "/session close"),
                SessionCmd::Unknown(sub) => write!(f, "/session {sub}"),
            },
            SlashCommand::Fork => write!(f, "/fork"),
        }
    }
}
