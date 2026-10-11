//! Text chat surface: one channel binding + shared slash commands
//! ([`liberado_commands`]) rendered as text (no TUI/WebUI widgets).
//!
//! Telegram is one binding. A later channel uses the same bridge with its own
//! [`BindingKey`](crate::bindings::BindingKey). Command text is shared; the channel label
//! (`Telegram`) is the only word that changes.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_commands::{CommandContext, CommandResult, SlashCommand, StatusInfo, dispatch, parse};
use liberado_conversation_store::{Author, ConversationHeader, ConversationStore, Ulid};
use liberado_executor::AgentEvent;
use liberado_main_agent::ChatSessions;
use liberado_session::{DomainHint, GoalSpec, SessionGrant, SessionOrigin};
use tokio::sync::broadcast::error::RecvError;

use crate::bindings::{BindingKey, ChannelBindings};
use crate::state::AppState;

/// Bound conversation + slash-command adapter over the same face agent as HTTP chat.
///
/// `bindings` is shared with the chat-delivering notifier so a cron brief appends into the
/// *same* conversation a reply continues (see `docs/future-work/ideas/cron-delivery-timing-idea.md`).
#[derive(Clone)]
pub struct TextChatBridge {
    pub state: Arc<AppState>,
    pub bindings: ChannelBindings,
    pub key: BindingKey,
}

#[async_trait]
impl liberado_messaging::ChatSurface for TextChatBridge {
    async fn reply(&self, user_text: &str) -> Result<String, String> {
        let text = user_text.trim();
        if text.starts_with('/') {
            return self.handle_slash(text).await;
        }
        self.chat_turn(text).await
    }
}

impl TextChatBridge {
    /// In-memory binding tests use. The label is still `Telegram`, so replies match production.
    #[cfg(test)]
    pub fn for_test(state: Arc<AppState>) -> Self {
        Self {
            state,
            bindings: ChannelBindings::ephemeral(),
            key: BindingKey::telegram("test-chat", "test-bot"),
        }
    }

    fn label(&self) -> &'static str {
        self.key.channel.label()
    }

    async fn bound_session(&self) -> Option<Ulid> {
        self.bindings.get(&self.key).await
    }

    async fn store_session(&self, id: Option<Ulid>) {
        self.bindings.set(&self.key, id).await;
    }

    async fn session_or_create<F, Fut>(&self, create: F) -> Result<Ulid, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Ulid, String>>,
    {
        self.bindings.get_or_create(&self.key, create).await
    }
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
        let label = self.label();
        let id = self
            .session_or_create(move || async move {
                creator
                    .create(Some(label.into()))
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
        let active = self.bound_session().await.map(|id| id.to_string());

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
            label: self.label(),
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
        self.store_session(next).await;

        if out.is_empty() {
            Ok("(done)".into())
        } else {
            Ok(out.join("\n\n"))
        }
    }

    /// Scope a model pick to the bound conversation — the same
    /// [`ChatSessions::select_model`] path WebUI/TUI use. Does **not** call
    /// `provider.set_model` when a bound chat exists (that was the bug: reply claimed a switch
    /// while history kept resolving via `model_last_used`).
    ///
    /// The session id is read and stored only through the binding registry.
    async fn select_model(&self, model: &str) -> Result<String, String> {
        let model = model.trim();
        if model.is_empty() {
            return Err("usage: /model <id>".into());
        }
        let sessions = self.chat_sessions()?;

        let label = self.label();
        if let Some(id) = self.bound_session().await {
            sessions.select_model(id, model.to_string());
            return Ok(format!(
                "Model set for this {label} chat ({id}): {model}\n\
                 Next turn of this conversation only — not the daemon-wide default."
            ));
        }

        // No binding yet: create one (same path free-form messages use) and scope there.
        // Stated in the reply — not the old silent process-wide set_model.
        let creator = sessions.clone();
        let id = self
            .session_or_create(move || async move {
                creator
                    .create(Some(label.into()))
                    .await
                    .map_err(|e| e.to_string())
            })
            .await?;
        sessions.select_model(id, model.to_string());
        Ok(format!(
            "No chat was open yet — started {label} session {id}.\n\
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
        let Some(id) = self.bound_session().await else {
            let label = self.label();
            return Ok(format!(
                "No active {label} conversation — nothing to stop. Send a message first."
            ));
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
                    // `handle_slash`'s trailing `store_session` keeps the fork instead of
                    // clobbering it back to the parent (the reply promises "You are now on the
                    // fork" — the sticky must land there too).
                    if let Some(fork_id) = self.bound_session().await {
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
            let label = self.label();
            Some(format!(
                "Goal session {}\nstatus: {:?}\n{}\n\n\
                 (Live event stream isn't on {label} yet — use the API/WebUI. \
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
            .bound_session()
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
            direct: false,
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

        self.store_session(Some(header.id)).await;

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

        CommandResult::ChatCleared => Some(format!(
            "{label} has no local transcript buffer to clear. Use /new for a fresh session.",
            label = ctx.label
        )),

        CommandResult::SessionClosed { id } => {
            ctx.session_id = None;
            Some(match id {
                Some(id) => format!("Closed session {id}. Use /sessions to resume later."),
                None => "No active session.".into(),
            })
        }

        CommandResult::BackToPrimary => Some(format!(
            "Back on primary chat ({label} only has one input focus).",
            label = ctx.label
        )),

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
            "Theme '{name}' is UI-only — {label} has no theme surface.",
            label = ctx.label
        )),
        CommandResult::ThemesReloaded { .. } | CommandResult::ThemeListed { .. } => {
            Some(format!("Themes are UI-only on {}.", ctx.label))
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
    label: &'static str,
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
        Err(vec![format!("Themes are UI-only on {}.", self.label)])
    }
}

#[cfg(test)]
#[path = "telegram_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "telegram_context_tests.rs"]
mod context_tests;
