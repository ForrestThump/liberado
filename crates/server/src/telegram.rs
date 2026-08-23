//! Telegram free-form chat surface: sticky session + shared slash commands
//! ([`liberado_commands`]) rendered as text (no TUI/WebUI widgets).

use std::sync::Arc;

use async_trait::async_trait;
use liberado_commands::{CommandContext, CommandResult, SlashCommand, StatusInfo, dispatch, parse};
use liberado_conversation_store::{Author, ConversationHeader, ConversationStore, Ulid};
use liberado_executor::AgentEvent;
use liberado_main_agent::ChatSessions;
use liberado_session::{DomainHint, GoalSpec, SessionGrant, SessionOrigin};
use tokio::sync::broadcast::error::RecvError;

use crate::state::AppState;
use crate::sticky::StickySession;

/// Sticky Telegram conversation + slash-command adapter over the same face agent as HTTP chat.
///
/// `session_id` is shared (`Arc`) with the chat-delivering notifier so a cron brief appends into the
/// *same* conversation a reply continues — that shared sticky id is the whole mechanism behind
/// "replying to a brief has the brief in context" (see `docs/future-work/ideas/cron-delivery-timing-idea.md`).
pub struct TelegramChatBridge {
    pub state: Arc<AppState>,
    pub session_id: StickySession,
}

#[async_trait]
impl liberado_messaging::ChatSurface for TelegramChatBridge {
    async fn reply(&self, user_text: &str) -> Result<String, String> {
        let text = user_text.trim();
        if text.starts_with('/') {
            return self.handle_slash(text).await;
        }
        self.chat_turn(text).await
    }
}

impl TelegramChatBridge {
    async fn chat_turn(&self, user_text: &str) -> Result<String, String> {
        // Telegram reaches the chat capability directly, so the HTTP middleware that refuses new
        // turns during shutdown never sees it. Without this check a message arriving mid-drain
        // starts a turn that the grace timeout aborts moments later — and it would also count
        // toward `in_flight_count`, so the drain waits on work it is about to throw away.
        if !self.state.drain.is_accepting() {
            return Err(crate::shutdown::SHUTTING_DOWN_MESSAGE.to_string());
        }
        let sessions = self.chat_sessions()?;
        let creator = sessions.clone();
        let id = self
            .session_id
            .get_or_create(move || async move {
                creator
                    .create(Some("Telegram".into()))
                    .await
                    .map_err(|e| e.to_string())
            })
            .await?;

        // Lifecycle: say so rather than attaching silently or queueing behind the session lock.
        if sessions.turn_running(id) {
            return Ok(
                "A turn is already running for this chat. Send /stop to cancel it — a cancelled \
                 turn keeps no partial answer."
                    .into(),
            );
        }
        // Prefer a clear note when the last turn left an unanswered user message (e.g. after /stop).
        let unanswered_prefix = if sessions.last_turn_unanswered(id).await {
            "Note: the previous turn ended without a reply.\n\n"
        } else {
            ""
        };

        // Durable turn path (same as WebUI/TUI): registers in the running map so /stop can cancel
        // and so drain/in_flight_count see Telegram work. Blocking `turn` never registered.
        let (_replay, rx) = sessions.start_or_attach(id, user_text);
        let reply = collect_turn_reply(rx).await?;
        if unanswered_prefix.is_empty() {
            Ok(reply)
        } else {
            Ok(format!("{unanswered_prefix}{reply}"))
        }
    }

    fn chat_sessions(&self) -> Result<Arc<ChatSessions>, String> {
        self.state
            .chat
            .clone()
            .ok_or_else(|| "chat is disabled".into())
    }

    async fn handle_slash(&self, text: &str) -> Result<String, String> {
        // Telegram-friendly: `/model <id>` (shared parse only knows bare `/model`).
        if let Some(rest) = text.strip_prefix("/model") {
            let rest = rest.trim();
            if !rest.is_empty() {
                let model = rest.split_whitespace().next().unwrap_or(rest);
                return self.select_model(model).await;
            }
        }

        // Cancel the sticky conversation's in-flight turn (same path as HTTP cancel).
        let head = text.split_whitespace().next().unwrap_or(text);
        if matches!(head, "/stop" | "/cancel") {
            return self.stop_turn().await;
        }

        if matches!(head, "/help") {
            let shared = liberado_commands::telegram_commands();
            let mut lines: Vec<String> = shared
                .iter()
                .map(|(name, desc)| format!("  /{name} — {desc}"))
                .collect();
            lines.push("  /stop — cancel the current turn (same as /cancel)".into());
            lines.sort();
            return Ok(format!("Available commands:\n{}", lines.join("\n")));
        }

        let cmd = parse(text).ok_or_else(|| {
            format!("Unknown command: {text}\nType /help for available commands.")
        })?;

        let sessions = self.chat_sessions()?;
        let active = self.session_id.get().await.map(|id| id.to_string());

        let conversations = match &cmd {
            SlashCommand::Session(_) | SlashCommand::Sessions | SlashCommand::Fork { .. } => {
                sessions.list().await.unwrap_or_default()
            }
            _ => Vec::new(),
        };

        let goals = match &cmd {
            SlashCommand::Sessions | SlashCommand::Join(_) => self.state.goals.list().await,
            _ => Vec::new(),
        };

        let reactions_seen = self.state.reactions.lock().await.len() as u64;
        let status = Some(StatusInfo {
            running: true,
            vault_path: self.state.vault_path.clone(),
            uptime_seconds: self.state.start_time.elapsed().as_secs(),
            model_name: self
                .state
                .provider
                .as_ref()
                .map(|p| p.model())
                .or_else(|| self.state.model_name.clone()),
            token_usage_total: None,
            context_window: None,
            dispatcher_attached: self.state.dispatcher_attached,
            orchestrator_attached: self.state.orchestrator_attached,
            reactions_seen,
        });

        let mut ctx = TelegramCommandContext {
            session_id: active,
            messages: Vec::new(),
            conversations,
            goals_summary: goals
                .iter()
                .map(|g| {
                    let title: String = g.goal.description.chars().take(40).collect();
                    (g.id.clone(), format!("{:?} — {title}", g.status))
                })
                .collect(),
            status,
            message_count: 0,
        };

        let results = dispatch(&cmd, &mut ctx);
        let mut out = std::mem::take(&mut ctx.messages);

        for result in results {
            if let Some(line) = self.apply_result(result, &mut ctx).await {
                out.push(line);
            }
        }

        let next = match ctx.session_id.as_deref() {
            None => None,
            Some(s) => s.parse::<Ulid>().ok(),
        };
        self.session_id.set(next).await;

        if out.is_empty() {
            Ok("(done)".into())
        } else {
            Ok(out.join("\n\n"))
        }
    }

    /// Scope a model pick to the sticky Telegram conversation — the same
    /// [`ChatSessions::select_model`] path WebUI/TUI use. Does **not** call
    /// `provider.set_model` when a sticky chat exists (that was the bug: reply claimed a switch
    /// while history kept resolving via `model_last_used`).
    ///
    /// Sticky id creation/storage is unchanged: we only read/create through [`StickySession`].
    async fn select_model(&self, model: &str) -> Result<String, String> {
        let model = model.trim();
        if model.is_empty() {
            return Err("usage: /model <id>".into());
        }
        let sessions = self.chat_sessions()?;

        if let Some(id) = self.session_id.get().await {
            sessions.select_model(id, model.to_string());
            return Ok(format!(
                "Model set for this Telegram chat ({id}): {model}\n\
                 Next turn of this conversation only — not the daemon-wide default."
            ));
        }

        // No sticky yet: create one (same get_or_create path free-form messages use) and scope
        // there. Stated in the reply — not the old silent process-wide set_model.
        let creator = sessions.clone();
        let id = self
            .session_id
            .get_or_create(move || async move {
                creator
                    .create(Some("Telegram".into()))
                    .await
                    .map_err(|e| e.to_string())
            })
            .await?;
        sessions.select_model(id, model.to_string());
        Ok(format!(
            "No chat was open yet — started Telegram session {id}.\n\
             Model set for that conversation: {model}\n\
             (Not daemon-wide; the next message uses this model.)"
        ))
    }

    /// Cancel the sticky conversation's running turn, if any.
    ///
    /// Reply deliberately does **not** claim a partial answer was kept — cancelled turns persist
    /// no assistant reply (same honesty the TUI help text was forced to adopt).
    async fn stop_turn(&self) -> Result<String, String> {
        let sessions = self.chat_sessions()?;
        let Some(id) = self.session_id.get().await else {
            return Ok(
                "No active Telegram conversation — nothing to stop. Send a message first.".into(),
            );
        };
        if sessions.cancel_turn(id) {
            Ok(
                "Turn cancelled. Nothing from that turn was kept — send a new message to try again."
                    .into(),
            )
        } else {
            Ok("No turn is running right now.".into())
        }
    }

    /// Resolve a command result to a Telegram reply. The browser/spawn/fork results need the
    /// live session state and are awaited; the rest is a deterministic local mapping.
    async fn apply_result(
        &self,
        result: CommandResult,
        ctx: &mut TelegramCommandContext,
    ) -> Option<String> {
        match result {
            CommandResult::OpenModelBrowser
            | CommandResult::SessionListed
            | CommandResult::OpenSessionBrowser
            | CommandResult::OpenGoalSwitcher
            | CommandResult::SessionSwitched { .. }
            | CommandResult::JoinGoalSession { .. }
            | CommandResult::SpawnGoalSession { .. }
            | CommandResult::ForkRequested { .. } => self.async_reply(result, ctx).await,
            other => static_reply(other, ctx),
        }
    }

    /// The awaited replies: model/session browsers, session switching, goal join/spawn, fork.
    async fn async_reply(
        &self,
        result: CommandResult,
        ctx: &mut TelegramCommandContext,
    ) -> Option<String> {
        match result {
            CommandResult::OpenModelBrowser => self.on_model_browser().await,

            CommandResult::SessionListed
            | CommandResult::OpenSessionBrowser
            | CommandResult::OpenGoalSwitcher => self.on_session_browser().await,

            CommandResult::SessionSwitched { id } => self.on_session_switched(ctx, &id).await,

            CommandResult::JoinGoalSession { id } => self.on_join_goal(&id).await,

            CommandResult::SpawnGoalSession { domain, goal } => {
                self.on_spawn_goal_session(&domain, &goal).await
            }

            CommandResult::ForkRequested {
                parent_id,
                after_turn,
            } => match self.fork_session(&parent_id, after_turn).await {
                Ok(msg) => {
                    // `fork_session` moved the sticky onto the fork; carry that into `ctx` so
                    // `handle_slash`'s trailing `session_id.set` keeps the fork instead of
                    // clobbering it back to the parent (the reply promises "You are now on the
                    // fork" — the sticky must land there too).
                    if let Some(fork_id) = self.session_id.get().await {
                        ctx.session_id = Some(fork_id.to_string());
                    }
                    Some(msg)
                }
                Err(e) => Some(format!("Fork failed: {e}")),
            },

            _ => unreachable!("async_reply only receives awaited results"),
        }
    }

    /// Telegram has no model picker; render the current model and the first 40 of the live list.
    async fn on_model_browser(&self) -> Option<String> {
        let Some(provider) = self.state.provider.as_ref() else {
            return Some("No provider configured.".into());
        };
        let current = provider.model();
        match provider.list_models().await {
            Ok(models) => {
                let mut lines = vec![
                    format!("Current model: {current}"),
                    "Switch: /model <id>".into(),
                    String::new(),
                ];
                for m in models.iter().take(40) {
                    let mark = if m == &current { " *" } else { "" };
                    lines.push(format!("  {m}{mark}"));
                }
                if models.len() > 40 {
                    lines.push(format!("  … and {} more", models.len() - 40));
                }
                Some(lines.join("\n"))
            }
            Err(e) => Some(format!("Could not list models: {e}")),
        }
    }

    /// Render the chat-session and goal-session listings as a flat text menu.
    async fn on_session_browser(&self) -> Option<String> {
        let sessions = self.chat_sessions().ok()?;
        let mut lines = vec!["Sessions (chat):".into()];
        match sessions.list().await {
            Ok(headers) => {
                if headers.is_empty() {
                    lines.push("  (none yet)".into());
                }
                for h in headers.iter().take(25) {
                    let title = h
                        .title
                        .clone()
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| "(untitled)".into());
                    lines.push(format!("  {}  {title}", h.id));
                }
            }
            Err(e) => lines.push(format!("  error: {e}")),
        }
        lines.push(String::new());
        lines.push("Goal sessions:".into());
        let goals = self.state.goals.list().await;
        if goals.is_empty() {
            lines.push("  (none)".into());
        }
        for g in goals.iter().take(25) {
            let desc: String = g.goal.description.chars().take(50).collect();
            lines.push(format!("  {}  [{:?}] {desc}", g.id, g.status));
        }
        lines.push(String::new());
        lines.push("Switch chat: /session switch <id>".into());
        lines.push("Join goal:   /join <id>".into());
        lines.push("Spawn:       /spawn <domain|profile> <goal text>".into());
        Some(lines.join("\n"))
    }

    /// Switch the active chat session, resolving by exact ulid first, then by prefix.
    async fn on_session_switched(
        &self,
        ctx: &mut TelegramCommandContext,
        id: &str,
    ) -> Option<String> {
        if let Ok(ulid) = id.parse::<Ulid>() {
            ctx.session_id = Some(ulid.to_string());
            return Some(format!("Switched to session {ulid}"));
        }
        let sessions = self.chat_sessions().ok()?;
        let headers = sessions.list().await.ok()?;
        if let Some(h) = headers.iter().find(|h| h.id.to_string().starts_with(id)) {
            ctx.session_id = Some(h.id.to_string());
            Some(format!("Switched to session {}", h.id))
        } else {
            Some(format!("No session matching '{id}'"))
        }
    }

    /// Snapshot a goal session by id or prefix; the live event stream isn't on Telegram yet.
    async fn on_join_goal(&self, id: &str) -> Option<String> {
        if let Some(snap) = self.state.goals.snapshot(id).await {
            let desc = &snap.session.goal.description;
            Some(format!(
                "Goal session {}\nstatus: {:?}\n{}\n\n\
                 (Live event stream isn't on Telegram yet — use the API/WebUI. \
                 /spawn still works from here.)",
                snap.session.id, snap.session.status, desc
            ))
        } else {
            let goals = self.state.goals.list().await;
            if let Some(g) = goals.iter().find(|g| g.id.starts_with(id)) {
                Some(format!(
                    "Goal session {}\nstatus: {:?}\n{}",
                    g.id, g.status, g.goal.description
                ))
            } else {
                Some(format!("No goal session matching '{id}'"))
            }
        }
    }

    /// `/spawn` a pack session: resolve the profile/domain grant, refuse silently-dead configs,
    /// and start the goal session. The same refusals as `POST /api/goals`.
    async fn on_spawn_goal_session(&self, domain: &str, goal: &str) -> Option<String> {
        let origin = self
            .session_id
            .get()
            .await
            .map(|id| SessionOrigin::from_conversation(id.to_string()));

        // Profile-first when the token isn't a well-known pack name.
        let profile = if matches!(domain, "life" | "coding" | "dispatch") {
            None
        } else {
            Some(domain.to_string())
        };
        let domain_fallback = profile.as_deref().unwrap_or(domain);
        // An unrecognized token was read as a profile name above. If it names no enabled
        // profile, say so instead of starting a session under a grant the human never
        // chose — a typo'd `/spawn` should be a correction, not a silent mis-scoped run.
        let resolved = self
            .state
            .config
            .resolve_session_profile(profile.as_deref(), domain_fallback);
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(e) => {
                tracing::warn!(
                    profile = ?profile,
                    error = %e,
                    "/spawn named an unknown session profile — not starting a session"
                );
                return Some(format!(
                    "Unknown session profile '{}'. Use a configured profile, or one of: \
                     life, coding, dispatch.",
                    profile.as_deref().unwrap_or(domain)
                ));
            }
        };
        // `/spawn` starts a *pack* session, so a chat-only profile is the wrong tool. Say so
        // rather than falling back to a domain the human did not pick.
        let Some(resolved_domain) = resolved.domain.clone() else {
            return Some(format!(
                "'{}' is a chat profile — it has no domain pack to run a session. Use it \
                 with /profile in a conversation instead.",
                profile.as_deref().unwrap_or("?")
            ));
        };

        // Same refusal as `POST /api/goals`: a domain with no grant resolves to zero
        // authority, and a session that may do nothing is safe but never useful. Saying so
        // beats a run that fails every action with a capability gap naming the wrong thing.
        if profile.is_none() && resolved.capabilities.capabilities.is_empty() {
            return Some(format!(
                "'{resolved_domain}' has no capability grant, so that session could do \
                 nothing. Add a policy.toml [[grants]] entry with component = \
                 \"{resolved_domain}\", or /spawn a configured profile."
            ));
        }

        let mut spec = GoalSpec {
            id: None,
            description: goal.to_string(),
            success_criteria: Vec::new(),
            domain: DomainHint::from(resolved_domain.as_str()),
            max_turns: 0,
            max_idle_secs: resolved.max_idle_secs,
            origin,
            profile,
            payload: serde_json::Value::Null,
        };
        if spec.domain.as_str() != resolved_domain.as_str() {
            spec.domain = DomainHint::from(resolved_domain.as_str());
        }

        let parts = resolved.grant_parts();
        let grant = SessionGrant {
            capabilities: parts.capabilities,
            profile: spec.profile.clone(),
            overrides: serde_json::to_value(&resolved.overrides).unwrap_or(serde_json::Value::Null),
            delegation: parts.delegation,
            model: parts.model.map(str::to_string),
            prompt_append: parts.prompt_append.map(str::to_string),
        };
        match self.state.goals.start_with_grant(spec, grant).await {
            Ok(id) => Some(format!(
                "Spawned goal session {id}\n{domain}: {goal}\n\
                 Snapshot: /join {id}"
            )),
            Err(e) => Some(format!("Spawn failed: {e}")),
        }
    }

    async fn fork_session(
        &self,
        parent_id: &str,
        after_turn: Option<u32>,
    ) -> Result<String, String> {
        let source: Ulid = parent_id
            .parse()
            .map_err(|_| format!("invalid session id: {parent_id}"))?;
        let path = self
            .state
            .sessions
            .leaf_path(source, None)
            .await
            .map_err(|e| e.to_string())?;

        let user_turns: Vec<usize> = path
            .iter()
            .enumerate()
            .filter(|(_, n)| n.author == Author::User)
            .map(|(i, _)| i)
            .collect();
        let total_turns = user_turns.len() as u32;

        let (at, kept_turns) = match after_turn {
            None => (None, total_turns),
            Some(0) => return Err("after_turn is 1-based; there is no turn 0".into()),
            Some(n) if n as usize >= user_turns.len() => (None, total_turns),
            Some(n) => {
                let next_turn_start = user_turns[n as usize];
                (Some(path[next_turn_start - 1].id), n)
            }
        };

        let header = self
            .state
            .sessions
            .fork_session(source, at, None)
            .await
            .map_err(|e| e.to_string())?;

        self.session_id.set(Some(header.id)).await;

        Ok(format!(
            "Forked → {}\nkept_turns={kept_turns}/{total_turns} from {parent_id}\n\
             You are now on the fork.",
            header.id
        ))
    }
}

/// The deterministic result→reply mapping that needs no session state.
fn static_reply(result: CommandResult, ctx: &mut TelegramCommandContext) -> Option<String> {
    match result {
        CommandResult::HelpShown
        | CommandResult::StatusShown
        | CommandResult::ModelInfoShown
        | CommandResult::SessionInfoShown
        | CommandResult::ProfileInfoShown
        | CommandResult::None => None,

        // Telegram has no picker widget, and a sticky Telegram chat is not the place to be
        // re-authorising a session anyway — the switch is a deliberate, per-conversation act.
        CommandResult::OpenProfileBrowser => {
            Some("Session profiles are switched from the web UI or TUI with /profile.".into())
        }

        CommandResult::Quit => {
            Some("I'm a long-running bot — I can't quit. Use /new for a fresh chat.".into())
        }

        CommandResult::NewConversation { .. } => {
            ctx.session_id = None;
            Some("Started a new conversation. Next message begins a fresh session.".into())
        }

        CommandResult::ChatCleared => Some(
            "Telegram has no local transcript buffer to clear. Use /new for a fresh session."
                .into(),
        ),

        CommandResult::SessionClosed { id } => {
            ctx.session_id = None;
            Some(match id {
                Some(id) => format!("Closed session {id}. Use /sessions to resume later."),
                None => "No active session.".into(),
            })
        }

        CommandResult::BackToPrimary => {
            Some("Back on primary chat (Telegram only has one input focus).".into())
        }

        // The coding-goal surface is a TUI view (role timeline, gate ballots, diffs). Telegram
        // has no place to render it, and a half-rendered gate is worse than an honest pointer:
        // the whole value of watching a quorum vote is seeing all of it.
        CommandResult::StartCodingGoal { .. }
        | CommandResult::OpenGoalView
        | CommandResult::GoalStatus
        | CommandResult::ParkGoalSession
        | CommandResult::ResumeGoalSession { .. }
        | CommandResult::CancelGoalSession => Some(
            "Coding goals run in the TUI, which can show the role timeline, the completion \
             gate's ballots, and diffs. Use /spawn here to start a non-coding session."
                .into(),
        ),

        CommandResult::ShowOptions { title, options } => {
            let mut lines = vec![format!("{title}:")];
            for (label, id) in options {
                if id.is_empty() {
                    lines.push(format!("  {label}"));
                } else {
                    lines.push(format!("  {label}  ({id})"));
                }
            }
            Some(lines.join("\n"))
        }

        CommandResult::ThemeChanged { name } => Some(format!(
            "Theme '{name}' is UI-only — Telegram has no theme surface."
        )),
        CommandResult::ThemesReloaded { .. } | CommandResult::ThemeListed { .. } => {
            Some("Themes are UI-only on Telegram.".into())
        }
        // Telegram has no picker to open, and the `ShowOptions` emitted alongside this already
        // renders the list here — so saying anything would just duplicate it.
        CommandResult::OpenThemeBrowser => None,
        _ => unreachable!("static_reply only receives local results"),
    }
}

/// Drain a durable-turn event stream until Done/Error. Used by free-form Telegram turns after
/// [`ChatSessions::start_or_attach`].
async fn collect_turn_reply(
    mut rx: tokio::sync::broadcast::Receiver<AgentEvent>,
) -> Result<String, String> {
    let mut reply = String::new();
    loop {
        match rx.recv().await {
            Ok(AgentEvent::Token(t)) => reply.push_str(&t),
            Ok(AgentEvent::Done) => return Ok(reply),
            // Cancels and hard failures alike: surface the error and discard whatever text had
            // streamed. Returning the partial would be the "we kept some of it" promise that /stop's
            // wording explicitly refuses to make.
            Ok(AgentEvent::Error(e)) => return Err(e),
            Ok(_) => {}
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => {
                if reply.is_empty() {
                    return Err("turn ended without a reply".into());
                }
                return Ok(reply);
            }
        }
    }
}

struct TelegramCommandContext {
    session_id: Option<String>,
    messages: Vec<String>,
    conversations: Vec<ConversationHeader>,
    goals_summary: Vec<(String, String)>,
    status: Option<StatusInfo>,
    message_count: usize,
}

impl CommandContext for TelegramCommandContext {
    fn active_session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    fn is_streaming(&self) -> bool {
        false
    }
    fn conversation_count(&self) -> usize {
        self.conversations.len()
    }
    fn find_conversation_id_by_prefix(&self, prefix: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|c| c.id.to_string().starts_with(prefix))
            .map(|c| c.id.to_string())
    }
    fn status_info(&self) -> Option<StatusInfo> {
        self.status.clone()
    }
    fn theme_names(&self) -> Vec<String> {
        Vec::new()
    }
    fn current_theme_name(&self) -> &str {
        "n/a"
    }
    fn conversation_title_for(&self, id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|c| c.id.to_string() == id)
            .and_then(|c| c.title.clone())
            .filter(|t| !t.is_empty())
    }
    fn conversation_parent_for(&self, id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|c| c.id.to_string() == id)
            .and_then(|c| c.parent_conversation.map(|p| p.to_string()))
    }
    fn message_count(&self) -> usize {
        self.message_count
    }
    fn conversation_list(&self) -> Vec<(String, String)> {
        let mut list: Vec<_> = self
            .conversations
            .iter()
            .map(|c| {
                let title = c
                    .title
                    .clone()
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "(untitled)".into());
                (title, c.id.to_string())
            })
            .collect();
        for (id, label) in &self.goals_summary {
            list.push((format!("[goal] {label}"), id.clone()));
        }
        list
    }

    fn set_active_session(&mut self, id: Option<String>) {
        self.session_id = id;
    }
    fn clear_chat(&mut self) {
        self.messages.clear();
    }
    fn reset_for_new_conversation(&mut self) {
        self.session_id = None;
        self.messages.clear();
    }
    fn push_system_message(&mut self, msg: String) {
        self.messages.push(msg);
    }
    fn clear_input(&mut self) {}
    fn stop_streaming(&mut self) {}
    fn set_theme(&mut self, _name: &str) -> bool {
        false
    }
    fn reload_themes(&mut self) -> Result<usize, Vec<String>> {
        Err(vec!["Themes are UI-only on Telegram.".into()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use liberado_executor::{Budget, Executor, ToolRuntime};

    /// The deterministic result→reply mapping (no session state) must cover the local arms, and
    /// the awaited browser/spawn/fork results must be routed past it (unreachable here).
    #[test]
    fn static_reply_maps_local_results() {
        let mut ctx = TelegramCommandContext {
            session_id: None,
            messages: Vec::new(),
            conversations: Vec::new(),
            goals_summary: Vec::new(),
            status: None,
            message_count: 0,
        };
        assert_eq!(
            static_reply(CommandResult::Quit, &mut ctx).unwrap(),
            "I'm a long-running bot — I can't quit. Use /new for a fresh chat."
        );
        assert!(
            static_reply(CommandResult::OpenThemeBrowser, &mut ctx).is_none(),
            "OpenThemeBrowser must stay silent (ShowOptions already rendered the list)"
        );
        assert!(static_reply(CommandResult::None, &mut ctx).is_none());

        let shown = static_reply(
            CommandResult::ShowOptions {
                title: "Pick".into(),
                options: vec![
                    ("Fork".into(), "f1".into()),
                    ("Fresh".into(), String::new()),
                ],
            },
            &mut ctx,
        )
        .unwrap();
        assert!(shown.contains("Pick"), "{shown}");
        assert!(shown.contains("Fork  (f1)"), "{shown}");
        assert!(shown.contains("Fresh"), "{shown}");
    }

    use liberado_common::Capability;
    use liberado_config::Grant;
    use liberado_executor::AgentEvent;
    use liberado_messaging::ChatSurface;
    use liberado_provider::ProviderError;
    use liberado_provider::{
        CompletionRequest, CompletionResponse, MockProvider, Provider, ProviderResult, ToolDef,
        ToolInvocation,
    };
    use liberado_session::{DomainHint, GoalSessionHub, GoalSpec, LifeOpsDemoRunner, SessionGrant};
    use liberado_session_store::SessionStore;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    struct NoTools;
    #[async_trait]
    impl ToolRuntime for NoTools {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _: &ToolInvocation) -> Result<String, String> {
            Err("no tools".into())
        }
    }

    /// Provider that never completes — keeps a turn in the running map for cancel/lifecycle tests.
    struct PendingProvider {
        model: std::sync::Mutex<String>,
        entered: AtomicBool,
    }
    impl PendingProvider {
        fn new(model: &str) -> Self {
            Self {
                model: std::sync::Mutex::new(model.into()),
                entered: AtomicBool::new(false),
            }
        }
    }
    #[async_trait]
    impl Provider for PendingProvider {
        fn model(&self) -> String {
            self.model.lock().unwrap().clone()
        }
        fn set_model(&self, model: String) {
            *self.model.lock().unwrap() = model;
        }
        async fn complete(&self, _: CompletionRequest) -> ProviderResult<CompletionResponse> {
            self.entered.store(true, Ordering::SeqCst);
            std::future::pending().await
        }
    }

    /// Hangs on the first completion, answers every one after. Lets a test cancel a turn and then
    /// take a *successful* next turn — the sequence the unanswered-turn note exists for.
    struct HangOnceProvider {
        model: std::sync::Mutex<String>,
        hung: AtomicBool,
    }
    #[async_trait]
    impl Provider for HangOnceProvider {
        fn model(&self) -> String {
            self.model.lock().unwrap().clone()
        }
        fn set_model(&self, model: String) {
            *self.model.lock().unwrap() = model;
        }
        async fn complete(&self, _: CompletionRequest) -> ProviderResult<CompletionResponse> {
            if !self.hung.swap(true, Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(CompletionResponse::text("recovered"))
        }
    }

    /// Spin until the sticky conversation has a turn registered, or give up.
    async fn wait_for_running(bridge: &TelegramChatBridge, chat: &Arc<ChatSessions>) -> Ulid {
        for _ in 0..200 {
            if let Some(s) = bridge.session_id.get().await
                && chat.turn_running(s)
            {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no turn registered as running");
    }

    async fn bridge_with_provider(
        root: &std::path::Path,
        provider: Arc<dyn Provider>,
    ) -> (TelegramChatBridge, Arc<ChatSessions>, Arc<dyn Provider>) {
        let store = Arc::new(SessionStore::open(root).await);
        let executor = Executor::new(Arc::clone(&provider), Budget::default());
        let chat = Arc::new(ChatSessions::new(
            store.clone(),
            executor,
            Arc::new(NoTools),
        ));
        let mut state =
            crate::state::AppState::for_test(store, Some(Arc::clone(&chat)), root.into());
        state.provider = Some(Arc::clone(&provider));
        let bridge = TelegramChatBridge {
            state: Arc::new(state),
            session_id: StickySession::ephemeral(),
        };
        (bridge, chat, provider)
    }

    /// Same as [`bridge_with_provider`] but with an explicit config — the `/spawn` refusal branches
    /// depend on which profiles and grants are configured.
    async fn bridge_with_config(
        root: &std::path::Path,
        provider: Arc<dyn Provider>,
        config: Arc<liberado_bootstrap::Config>,
    ) -> (TelegramChatBridge, Arc<ChatSessions>, Arc<dyn Provider>) {
        let store = Arc::new(SessionStore::open(root).await);
        let executor = Executor::new(Arc::clone(&provider), Budget::default());
        let chat = Arc::new(ChatSessions::new(
            store.clone(),
            executor,
            Arc::new(NoTools),
        ));
        let mut state =
            crate::state::AppState::for_test(store, Some(Arc::clone(&chat)), root.into());
        state.provider = Some(Arc::clone(&provider));
        state.config = config;
        let bridge = TelegramChatBridge {
            state: Arc::new(state),
            session_id: StickySession::ephemeral(),
        };
        (bridge, chat, provider)
    }

    /// Like [`bridge_with_provider`] but with a goal-session hub that has the life demo pack
    /// registered, so a test can start a real goal session and exercise `/join`.
    async fn bridge_with_goal_pack(
        root: &std::path::Path,
        provider: Arc<dyn Provider>,
    ) -> (TelegramChatBridge, Arc<ChatSessions>, Arc<GoalSessionHub>) {
        let store = Arc::new(SessionStore::open(root).await);
        let executor = Executor::new(Arc::clone(&provider), Budget::default());
        let chat = Arc::new(ChatSessions::new(
            store.clone(),
            executor,
            Arc::new(NoTools),
        ));
        let mut hub = GoalSessionHub::new(SessionStore::clone(&store));
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let goals = Arc::new(hub);
        let mut state =
            crate::state::AppState::for_test(store, Some(Arc::clone(&chat)), root.into());
        state.provider = Some(provider);
        state.goals = goals.clone();
        let bridge = TelegramChatBridge {
            state: Arc::new(state),
            session_id: StickySession::ephemeral(),
        };
        (bridge, chat, goals)
    }

    /// R3: `/model <id>` with a sticky chat sets the **next turn's user-node model stamp**, not
    /// merely a reply string. Also asserts the process-wide provider default is unchanged.
    ///
    /// R1: if `select_model` is reverted to only `provider.set_model`, the stamp assertion fails
    /// (pending pick never lands) and/or the global-default assertion fails.
    #[tokio::test]
    async fn model_command_scopes_to_sticky_and_stamps_the_next_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::with_script(
            "daemon-default",
            [
                CompletionResponse::text("ok"),
                CompletionResponse::text("second"),
            ],
        ));
        let (bridge, chat, provider) = bridge_with_provider(dir.path(), mock).await;

        // Establish sticky conversation (same get_or_create path free-form uses).
        let first = bridge.reply("hello").await.unwrap();
        assert_eq!(first, "ok");
        let sticky = bridge
            .session_id
            .get()
            .await
            .expect("sticky after first turn");
        let global_before = provider.model();
        assert_eq!(global_before, "daemon-default");

        let reply = bridge.reply("/model picked/for-telegram").await.unwrap();
        assert!(
            reply.contains("this Telegram chat") || reply.contains(&sticky.to_string()),
            "reply must state conversation scope: {reply}"
        );
        assert_eq!(
            provider.model(),
            global_before,
            "/model must not change the process-wide default while sticky exists"
        );

        // Next turn must run on the pick — assert the stamp on the user node (R3), not reply text.
        bridge.reply("next turn").await.unwrap();
        let nodes = chat.history_nodes(sticky).await.unwrap();
        let user = nodes
            .iter()
            .rev()
            .find(|n| matches!(n.author, Author::User))
            .expect("user node for the second turn");
        assert_eq!(
            user.model.as_deref(),
            Some("picked/for-telegram"),
            "next turn must stamp the per-conversation pick, not the daemon default"
        );
        assert_eq!(
            provider.model(),
            "daemon-default",
            "process-wide default must still be untouched after the turn"
        );
    }

    /// No sticky yet: /model creates a conversation and scopes there — stated, not silent global.
    #[tokio::test]
    async fn model_without_sticky_creates_conversation_and_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::with_script(
            "daemon-default",
            [CompletionResponse::text("after-model")],
        ));
        let (bridge, chat, provider) = bridge_with_provider(dir.path(), mock).await;
        assert!(bridge.session_id.get().await.is_none());

        let reply = bridge.reply("/model fresh/pick").await.unwrap();
        assert!(
            reply.contains("No chat was open") || reply.contains("started Telegram session"),
            "must state the no-sticky policy: {reply}"
        );
        assert_eq!(
            provider.model(),
            "daemon-default",
            "must not fall back to silent process-wide set_model"
        );
        let sticky = bridge
            .session_id
            .get()
            .await
            .expect("sticky created by /model");
        bridge.reply("go").await.unwrap();
        let nodes = chat.history_nodes(sticky).await.unwrap();
        let user = nodes
            .iter()
            .rev()
            .find(|n| matches!(n.author, Author::User))
            .unwrap();
        assert_eq!(user.model.as_deref(), Some("fresh/pick"));
    }

    /// Free-form while a sticky turn is running gets a distinguishable response (not the model).
    #[tokio::test]
    async fn freeform_while_turn_running_is_refused_with_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Arc::new(PendingProvider::new("pending"));
        let (bridge, chat, _) = bridge_with_provider(dir.path(), pending.clone()).await;

        // Start a hang turn in the background via the real bridge path.
        let b = TelegramChatBridge {
            state: Arc::clone(&bridge.state),
            session_id: bridge.session_id.clone(),
        };
        let hang = tokio::spawn(async move { b.reply("long running").await });

        // Wait until the turn is registered (provider entered or turn_running).
        let sticky = {
            let mut id = None;
            for _ in 0..100 {
                if let Some(s) = bridge.session_id.get().await
                    && chat.turn_running(s)
                {
                    id = Some(s);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            id.expect("sticky turn should register as running")
        };
        assert!(chat.turn_running(sticky));

        let feedback = bridge.reply("another message").await.unwrap();
        assert!(
            feedback.contains("already running") && feedback.contains("/stop"),
            "must be a lifecycle reply, not a model completion: {feedback}"
        );
        // Not a silent attach that waits on the hang.
        hang.abort();
        let _ = hang.await;
        let _ = chat.cancel_turn(sticky);
    }

    /// /stop cancels through the real cancel path; reply does not promise a kept partial.
    #[tokio::test]
    async fn stop_cancels_inflight_turn_without_promising_partial() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Arc::new(PendingProvider::new("pending"));
        let (bridge, chat, _) = bridge_with_provider(dir.path(), pending).await;

        let b = TelegramChatBridge {
            state: Arc::clone(&bridge.state),
            session_id: bridge.session_id.clone(),
        };
        let hang = tokio::spawn(async move { b.reply("hang").await });

        let sticky = {
            let mut id = None;
            for _ in 0..100 {
                if let Some(s) = bridge.session_id.get().await
                    && chat.turn_running(s)
                {
                    id = Some(s);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            id.expect("running turn")
        };

        let stop_reply = bridge.reply("/stop").await.unwrap();
        assert!(
            stop_reply.to_lowercase().contains("cancel"),
            "stop reply should acknowledge cancel: {stop_reply}"
        );
        assert!(
            !stop_reply.to_lowercase().contains("kept")
                || stop_reply.to_lowercase().contains("nothing")
                || stop_reply.to_lowercase().contains("no partial"),
            "must not promise a partial was kept: {stop_reply}"
        );
        // Stronger: our shipped text says nothing was kept.
        assert!(
            stop_reply.contains("Nothing from that turn was kept")
                || stop_reply.contains("keeps no partial"),
            "honest cancel wording required: {stop_reply}"
        );
        assert!(
            !chat.turn_running(sticky),
            "cancel_turn must clear the running map"
        );

        let hang_result = hang.await.expect("join");
        // The waiting free-form future should fail with cancelled, not return a model answer.
        assert!(
            hang_result.is_err()
                || hang_result
                    .as_ref()
                    .is_ok_and(|s| s.to_lowercase().contains("cancel")),
            "hung turn after /stop: {hang_result:?}"
        );
    }

    /// After a cancelled turn, the **next** message says the previous turn ended without a reply.
    ///
    /// This is the second half of the lifecycle acceptance item ("if the last turn ended
    /// unanswered, say that instead of silence"), and it was the one path with no test: deleting
    /// the whole `unanswered_prefix` block left every other test in this module green.
    ///
    /// R3: crosses from the running-turn map into the persisted log — `last_turn_unanswered` reads
    /// message nodes, so this only passes if the cancelled turn genuinely left a user node with no
    /// reply after it, not merely if a flag was set in memory.
    #[tokio::test]
    async fn message_after_a_cancelled_turn_reports_the_unanswered_turn() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(HangOnceProvider {
            model: std::sync::Mutex::new("m".into()),
            hung: AtomicBool::new(false),
        });
        let (bridge, chat, _) = bridge_with_provider(dir.path(), provider).await;

        let b = TelegramChatBridge {
            state: Arc::clone(&bridge.state),
            session_id: bridge.session_id.clone(),
        };
        let hang = tokio::spawn(async move { b.reply("first").await });
        let sticky = wait_for_running(&bridge, &chat).await;

        assert!(bridge.reply("/stop").await.is_ok());
        let _ = hang.await;
        assert!(!chat.turn_running(sticky));
        assert!(
            chat.last_turn_unanswered(sticky).await,
            "precondition: the cancelled turn must leave an unanswered user node"
        );

        let next = bridge.reply("second").await.unwrap();
        assert!(
            next.contains("previous turn ended without a reply"),
            "must state the unanswered turn rather than answering in silence: {next}"
        );
        assert!(
            next.contains("recovered"),
            "the note prefixes the reply, it does not replace it: {next}"
        );
    }

    /// The note is *conditional* — an ordinary turn following an answered one carries no prefix.
    /// Without this, always emitting the note would pass the test above.
    #[tokio::test]
    async fn ordinary_turn_carries_no_unanswered_note() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::with_script(
            "m",
            [
                CompletionResponse::text("one"),
                CompletionResponse::text("two"),
            ],
        ));
        let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
        assert_eq!(bridge.reply("hello").await.unwrap(), "one");
        let second = bridge.reply("again").await.unwrap();
        assert_eq!(
            second, "two",
            "an answered turn must not be decorated: {second}"
        );
    }

    #[tokio::test]
    async fn help_command_lists_available_commands() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let (bridge, _chat, _provider) = bridge_with_provider(dir.path(), mock).await;
        let reply = bridge.reply("/help").await.unwrap();
        // Commands from the shared telegram_commands() catalog.
        for cmd in [
            "/help",
            "/new",
            "/status",
            "/sessions",
            "/spawn",
            "/goal",
            "/join",
            "/model",
            "/fork",
        ] {
            assert!(reply.contains(cmd), "/help must list {cmd}: got {reply}");
        }
        // Telegram-specific commands not in the shared catalog.
        assert!(reply.contains("/stop"), "must include /stop");
        // Shared catalog descriptions come through (not the hardcoded old text).
        assert!(
            !reply.contains("switch to model"),
            "descriptions come from COMMAND_CATALOG, not hardcoded"
        );
    }

    // --- Mutation-hardening tests: the branches the 2026-07-30 report left uncovered. ---

    #[tokio::test]
    async fn spawn_naming_an_unknown_profile_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
        let reply = bridge
            .reply("/spawn nosuchprofile write a report")
            .await
            .unwrap();
        assert!(reply.contains("Unknown session profile"), "{reply}");
    }

    #[tokio::test]
    async fn spawn_a_chat_only_profile_is_refused_not_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        // `chatpack` is an enabled profile with no domain — a chat-only hat `/spawn` must refuse.
        // Built through TOML so `Config::validate` runs (the profile's component needs a grant).
        let config = liberado_bootstrap::Config::from_str(
            r#"
[topology]
vault_path = "/tmp/vault"

[[topology.session_profiles]]
name = "chatpack"

[[policy.grants]]
component = "chatpack"
capabilities = []
"#,
        )
        .unwrap();
        let (bridge, _chat, _) = bridge_with_config(dir.path(), mock, Arc::new(config)).await;
        let reply = bridge
            .reply("/spawn chatpack write a report")
            .await
            .unwrap();
        assert!(reply.contains("is a chat profile"), "{reply}");
    }

    #[tokio::test]
    async fn spawn_a_domain_with_no_grant_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        // Config::default() has no grants, so "coding" resolves to zero authority.
        let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
        let reply = bridge.reply("/spawn coding write a report").await.unwrap();
        assert!(reply.contains("no capability grant"), "{reply}");
    }

    #[tokio::test]
    async fn spawn_passes_refusals_then_surfaces_a_start_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let mut config = liberado_bootstrap::Config::default();
        config.policy.grants.push(Grant {
            component: "coding".into(),
            capabilities: vec![Capability::AskHuman],
        });
        let (bridge, _chat, _) = bridge_with_config(dir.path(), mock, Arc::new(config)).await;
        let reply = bridge.reply("/spawn coding write a report").await.unwrap();
        // No pack is registered for "coding" in the test hub, so the start must fail loudly.
        assert!(reply.contains("Spawn failed"), "{reply}");
    }

    #[tokio::test]
    async fn fork_without_an_active_session_explains() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
        let reply = bridge.reply("/fork").await.unwrap();
        assert!(reply.contains("No conversation to fork"), "{reply}");
    }

    #[tokio::test]
    async fn fork_after_turn_zero_is_rejected_as_1_based() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::with_script(
            "m",
            [CompletionResponse::text("ok")],
        ));
        let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
        assert_eq!(bridge.reply("hello").await.unwrap(), "ok");
        let reply = bridge.reply("/fork 0").await.unwrap();
        assert!(reply.contains("Turns are numbered from 1"), "{reply}");
    }

    #[tokio::test]
    async fn fork_branches_the_sticky_conversation_and_switches_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::with_script(
            "m",
            [CompletionResponse::text("ok")],
        ));
        let (bridge, chat, _) = bridge_with_provider(dir.path(), mock).await;
        assert_eq!(bridge.reply("hello").await.unwrap(), "ok");
        let sticky = bridge.session_id.get().await.expect("sticky after turn");

        let reply = bridge.reply("/fork").await.unwrap();
        assert!(reply.contains("Forked"), "{reply}");
        let fork_id = bridge.session_id.get().await.expect("switched to the fork");
        assert_ne!(fork_id, sticky, "the fork must be a new conversation");

        // The fork copied the transcript — one user turn, its reply, and the original untouched.
        let nodes = chat.history_nodes(fork_id).await.unwrap();
        assert_eq!(
            nodes
                .iter()
                .filter(|n| matches!(n.author, Author::User))
                .count(),
            1,
            "fork must carry the user turn"
        );
        assert!(
            !chat.turn_running(sticky),
            "forking must not disturb the original conversation"
        );

        // /fork 1 keeps through the first turn — same single-turn transcript, fresh id.
        let before = bridge.session_id.get().await.unwrap();
        let reply = bridge.reply("/fork 1").await.unwrap();
        assert!(reply.contains("kept_turns=1/1"), "{reply}");
        assert_ne!(bridge.session_id.get().await.unwrap(), before);
    }

    #[tokio::test]
    async fn join_snapshot_renders_a_live_goal_session() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let (bridge, _chat, goals) = bridge_with_goal_pack(dir.path(), mock).await;
        let id = goals
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "review the week".into(),
                    success_criteria: vec![],
                    domain: DomainHint::from("life"),
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({}),
                },
                SessionGrant::default(),
            )
            .await
            .unwrap();

        let reply = bridge.reply(&format!("/join {id}")).await.unwrap();
        assert!(reply.contains(&format!("Goal session {id}")), "{reply}");
    }

    #[tokio::test]
    async fn join_by_id_prefix_falls_back_to_the_listing() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let (bridge, _chat, goals) = bridge_with_goal_pack(dir.path(), mock).await;
        let id = goals
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "plan the quarter".into(),
                    success_criteria: vec![],
                    domain: DomainHint::from("life"),
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({}),
                },
                SessionGrant::default(),
            )
            .await
            .unwrap();
        let prefix: String = id.to_string().chars().take(8).collect();

        let reply = bridge.reply(&format!("/join {prefix}")).await.unwrap();
        assert!(reply.contains(&format!("Goal session {id}")), "{reply}");
    }

    #[tokio::test]
    async fn join_an_unknown_goal_session_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let (bridge, _chat, _goals) = bridge_with_goal_pack(dir.path(), mock).await;
        let reply = bridge.reply("/join zzzzz").await.unwrap();
        assert!(
            reply.contains("No goal session matching 'zzzzz'"),
            "{reply}"
        );
    }

    #[tokio::test]
    async fn session_switch_by_exact_ulid_moves_the_sticky_chat() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let (bridge, chat, _) = bridge_with_provider(dir.path(), mock).await;
        let conv = chat.create(None).await.unwrap();

        let reply = bridge
            .reply(&format!("/session switch {conv}"))
            .await
            .unwrap();
        assert!(
            reply.contains(&format!("Switched to session {conv}")),
            "{reply}"
        );
        assert_eq!(bridge.session_id.get().await.unwrap(), conv);
    }

    #[tokio::test]
    async fn session_switch_resolves_by_prefix_and_reports_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let (bridge, chat, _) = bridge_with_provider(dir.path(), mock).await;
        let conv = chat.create(None).await.unwrap();
        let prefix: String = conv.to_string().chars().take(8).collect();

        let reply = bridge
            .reply(&format!("/session switch {prefix}"))
            .await
            .unwrap();
        assert!(reply.contains("Switched to session"), "{reply}");
        assert_eq!(bridge.session_id.get().await.unwrap(), conv);

        let reply = bridge.reply("/session switch zzzzz").await.unwrap();
        assert!(reply.contains("No session matching 'zzzzz'"), "{reply}");
    }

    #[tokio::test]
    async fn sessions_browser_lists_chats_and_goal_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let (bridge, chat, _) = bridge_with_provider(dir.path(), mock).await;
        let conv = chat.create(None).await.unwrap();

        let reply = bridge.reply("/sessions").await.unwrap();
        assert!(reply.contains("Sessions (chat):"), "{reply}");
        assert!(reply.contains(&conv.to_string()), "{reply}");
        assert!(reply.contains("Goal sessions:"), "{reply}");
        assert!(reply.contains("(none)"), "{reply}");
    }

    #[tokio::test]
    async fn unknown_slash_command_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;
        let reply = bridge.reply("/definitely-not-a-command").await;
        assert!(
            matches!(reply, Err(ref e) if e.contains("Unknown command")),
            "{reply:?}"
        );
    }

    #[tokio::test]
    async fn chat_turn_refuses_when_chat_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path()).await);
        let state = crate::state::AppState::for_test(store, None, dir.path().into());
        let bridge = TelegramChatBridge {
            state: Arc::new(state),
            session_id: StickySession::ephemeral(),
        };
        let reply = bridge.reply("hello").await;
        assert!(
            matches!(reply, Err(ref e) if e == "chat is disabled"),
            "{reply:?}"
        );
    }

    #[tokio::test]
    async fn model_browser_renders_current_and_listed_models() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockProvider::new("m"));
        mock.set_models(["alpha", "beta"]);
        let (bridge, _chat, _) = bridge_with_provider(dir.path(), mock).await;

        let reply = bridge.reply("/model").await.unwrap();
        assert!(reply.contains("Current model: m"), "{reply}");
        assert!(reply.contains("alpha"), "{reply}");
        assert!(reply.contains("beta"), "{reply}");
    }

    #[tokio::test]
    async fn model_browser_with_no_provider_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path()).await);
        let executor = Executor::new(Arc::new(MockProvider::new("m")), Budget::default());
        let chat = Arc::new(ChatSessions::new(
            store.clone(),
            executor,
            Arc::new(NoTools),
        ));
        let mut state = crate::state::AppState::for_test(store, Some(chat), dir.path().into());
        state.provider = None;
        let bridge = TelegramChatBridge {
            state: Arc::new(state),
            session_id: StickySession::ephemeral(),
        };
        let reply = bridge.reply("/model").await.unwrap();
        assert!(reply.contains("No provider configured"), "{reply}");
    }

    struct ModelListErrorProvider;
    #[async_trait]
    impl Provider for ModelListErrorProvider {
        fn model(&self) -> String {
            "m".into()
        }
        fn set_model(&self, _: String) {}
        async fn complete(&self, _: CompletionRequest) -> ProviderResult<CompletionResponse> {
            Ok(CompletionResponse::text("ok"))
        }
        async fn list_models(&self) -> ProviderResult<Vec<String>> {
            Err(ProviderError::Transport("list failed".into()))
        }
    }

    #[tokio::test]
    async fn model_browser_surfaces_a_list_models_failure() {
        let dir = tempfile::tempdir().unwrap();
        let (bridge, _chat, _) =
            bridge_with_provider(dir.path(), Arc::new(ModelListErrorProvider)).await;
        let reply = bridge.reply("/model").await.unwrap();
        assert!(reply.contains("Could not list models"), "{reply}");
        assert!(reply.contains("list failed"), "{reply}");
    }

    // collect_turn_reply is a free function fed a broadcast stream; unit-driving it directly
    // covers the event variants the end-to-end turns never reach (Lagged, Closed-with-empty).
    #[tokio::test]
    async fn collect_turn_reply_surfaces_error_and_discards_stream() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        tx.send(AgentEvent::Token("partial".into())).ok();
        tx.send(AgentEvent::Error("boom".into())).ok();
        assert_eq!(collect_turn_reply(rx).await, Err("boom".into()));
    }

    #[tokio::test]
    async fn collect_turn_reply_skips_lagged_and_returns_latest() {
        let (tx, rx) = tokio::sync::broadcast::channel(2);
        tx.send(AgentEvent::Token("a".into())).ok();
        tx.send(AgentEvent::Token("b".into())).ok();
        tx.send(AgentEvent::Token("c".into())).ok();
        tx.send(AgentEvent::Done).ok();
        let reply = collect_turn_reply(rx).await;
        assert_eq!(reply, Ok("c".into()));
    }

    #[tokio::test]
    async fn collect_turn_reply_closed_with_empty_stream_errors() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        drop(tx);
        assert_eq!(
            collect_turn_reply(rx).await,
            Err("turn ended without a reply".into())
        );
    }

    #[tokio::test]
    async fn collect_turn_reply_closed_after_partial_returns_partial() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        tx.send(AgentEvent::Token("partial".into())).ok();
        drop(tx);
        assert_eq!(collect_turn_reply(rx).await, Ok("partial".into()));
    }

    #[tokio::test]
    async fn collect_turn_reply_done_returns_stream() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        tx.send(AgentEvent::Token("hi".into())).ok();
        tx.send(AgentEvent::Done).ok();
        assert_eq!(collect_turn_reply(rx).await, Ok("hi".into()));
    }

    #[tokio::test]
    async fn collect_turn_reply_ignores_non_token_events() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        tx.send(AgentEvent::ToolStarted {
            name: "x".into(),
            args: "y".into(),
        })
        .ok();
        tx.send(AgentEvent::Done).ok();
        assert_eq!(collect_turn_reply(rx).await, Ok(String::new()));
    }

    /// The deterministic result→reply mapping must cover every surface-local arm, including the
    /// lifecycle verbs that reset the sticky session. (Browser/fork/spawn results are routed past
    /// `static_reply` and asserted via the end-to-end tests above.)
    #[test]
    fn static_reply_covers_surface_local_arms() {
        let ctx = || TelegramCommandContext {
            session_id: Some("sess-1".into()),
            messages: Vec::new(),
            conversations: Vec::new(),
            goals_summary: Vec::new(),
            status: None,
            message_count: 0,
        };

        let mut c = ctx();
        let r = static_reply(
            CommandResult::NewConversation {
                was_streaming: false,
            },
            &mut c,
        )
        .unwrap();
        assert!(r.contains("Started a new conversation"), "{r}");
        assert!(c.session_id.is_none(), "/new must clear the sticky session");

        let r = static_reply(CommandResult::ChatCleared, &mut ctx()).unwrap();
        assert!(r.contains("no local transcript buffer"), "{r}");

        let mut c = ctx();
        let r = static_reply(
            CommandResult::SessionClosed {
                id: Some("abc".into()),
            },
            &mut c,
        )
        .unwrap();
        assert!(r.contains("Closed session abc"), "{r}");
        assert!(
            c.session_id.is_none(),
            "/close must clear the sticky session"
        );

        let r = static_reply(CommandResult::SessionClosed { id: None }, &mut ctx()).unwrap();
        assert!(r.contains("No active session"), "{r}");

        let r = static_reply(CommandResult::BackToPrimary, &mut ctx()).unwrap();
        assert!(r.contains("Back on primary chat"), "{r}");

        let r = static_reply(CommandResult::OpenProfileBrowser, &mut ctx()).unwrap();
        assert!(
            r.contains("Session profiles are switched from the web UI"),
            "{r}"
        );

        for coding in [
            CommandResult::StartCodingGoal {
                project: None,
                text: "x".into(),
                mode: None,
            },
            CommandResult::OpenGoalView,
            CommandResult::GoalStatus,
            CommandResult::ParkGoalSession,
            CommandResult::ResumeGoalSession { answer: "y".into() },
            CommandResult::CancelGoalSession,
        ] {
            let r = static_reply(coding, &mut ctx()).unwrap();
            assert!(r.contains("Coding goals run in the TUI"), "{r}");
        }

        let r = static_reply(
            CommandResult::ThemeChanged {
                name: "dark".into(),
            },
            &mut ctx(),
        )
        .unwrap();
        assert!(r.contains("Theme 'dark' is UI-only"), "{r}");

        let r = static_reply(
            CommandResult::ThemesReloaded {
                count: 1,
                errors: vec![],
            },
            &mut ctx(),
        )
        .unwrap();
        assert!(r.contains("Themes are UI-only on Telegram"), "{r}");

        let r = static_reply(
            CommandResult::ThemeListed {
                names: vec!["a".into()],
                active: "a".into(),
            },
            &mut ctx(),
        )
        .unwrap();
        assert!(r.contains("Themes are UI-only on Telegram"), "{r}");

        for silent in [
            CommandResult::HelpShown,
            CommandResult::StatusShown,
            CommandResult::ModelInfoShown,
            CommandResult::SessionInfoShown,
            CommandResult::ProfileInfoShown,
            CommandResult::OpenThemeBrowser,
        ] {
            assert!(
                static_reply(silent.clone(), &mut ctx()).is_none(),
                "{silent:?}"
            );
        }
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    fn header(
        id: Ulid,
        title: &str,
        parent: Option<Ulid>,
    ) -> liberado_conversation_store::ConversationHeader {
        use liberado_conversation_store as lcs;
        lcs::ConversationHeader {
            id,
            title: if title.is_empty() {
                None
            } else {
                Some(title.into())
            },
            parent_conversation: parent,
            spawned_by: None,
            created_at: chrono::Utc::now(),
            grant: Default::default(),
        }
    }

    fn ctx_with(conversations: Vec<ConversationHeader>) -> TelegramCommandContext {
        TelegramCommandContext {
            session_id: Some("active-1".into()),
            messages: vec!["m1".into(), "m2".into()],
            conversations,
            goals_summary: vec![("goal-1".into(), "build the thing".into())],
            status: None,
            message_count: 7,
        }
    }

    /// Every CommandContext accessor answers from the real snapshot fields — stubs here would
    /// feed the Telegram slash-command surface invented sessions, counts and titles.
    #[tokio::test]
    async fn command_context_accessors_answer_from_their_fields() {
        let a = Ulid::new();
        let b = Ulid::new();
        let mut ctx = ctx_with(vec![header(a, "First", None), header(b, "", Some(a))]);
        ctx.status = Some(StatusInfo {
            running: true,
            vault_path: "/v".into(),
            uptime_seconds: 9,
            model_name: Some("live-model".into()),
            token_usage_total: None,
            context_window: None,
            dispatcher_attached: false,
            orchestrator_attached: false,
            reactions_seen: 0,
        });

        assert!(!ctx.is_streaming());
        assert_eq!(ctx.conversation_count(), 2);
        assert_eq!(ctx.message_count, 7);
        assert_eq!(ctx.message_count(), 7, "the accessor reads the same field");
        assert_eq!(ctx.active_session_id(), Some("active-1"));

        // Prefix lookup: full id works, a short prefix resolves, a miss is None.
        let full = a.to_string();
        assert_eq!(
            ctx.find_conversation_id_by_prefix(&full).as_deref(),
            Some(full.as_str())
        );
        let short = &full[..8];
        assert_eq!(
            ctx.find_conversation_id_by_prefix(short).as_deref(),
            Some(full.as_str())
        );
        assert_eq!(ctx.find_conversation_id_by_prefix("zzzz"), None);

        // Status passes through.
        let status = ctx.status_info().expect("status present");
        assert_eq!(status.model_name.as_deref(), Some("live-model"));

        // Telegram has no theme engine: empty list, fixed label, reload refuses.
        assert!(ctx.theme_names().is_empty());
        assert_eq!(ctx.current_theme_name(), "n/a");
        assert!(!ctx.set_theme("dark"));
        assert!(ctx.reload_themes().is_err());

        // Titles resolve for known ids; blank titles are not invented.
        assert_eq!(ctx.conversation_title_for(&full).as_deref(), Some("First"));
        assert_eq!(ctx.conversation_title_for(&b.to_string()), None);
        assert_eq!(ctx.conversation_title_for("unknown"), None);

        // Parents resolve as strings; root conversations have none.
        assert_eq!(
            ctx.conversation_parent_for(&b.to_string()).as_deref(),
            Some(full.as_str())
        );
        assert_eq!(ctx.conversation_parent_for(&full), None);

        // The listing pairs titles with ids, marks untitled ones, appends goals last.
        let list = ctx.conversation_list();
        assert_eq!(
            list,
            vec![
                ("First".to_string(), full.clone()),
                ("(untitled)".to_string(), b.to_string()),
                ("[goal] build the thing".to_string(), "goal-1".to_string()),
            ]
        );
    }

    /// Mutators actually mutate: set/clear/reset are observable through the accessors.
    #[tokio::test]
    async fn command_context_mutators_take_effect() {
        let mut ctx = ctx_with(Vec::new());

        ctx.set_active_session(Some("sess-9".into()));
        assert_eq!(ctx.active_session_id(), Some("sess-9"));
        ctx.set_active_session(None);
        assert_eq!(ctx.active_session_id(), None);

        // Reset clears the active session (the messages vector has no trait-level reader;
        // its clearing is covered by reset's session half plus push_system_message users).
        ctx.set_active_session(Some("again".into()));
        ctx.reset_for_new_conversation();
        assert_eq!(ctx.active_session_id(), None);
        ctx.push_system_message("hello".into()); // must compile-and-run without state visible
    }
}
