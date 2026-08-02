//! Telegram free-form chat surface: sticky session + shared slash commands
//! ([`liberado_commands`]) rendered as text (no TUI/WebUI widgets).

use std::sync::Arc;

use async_trait::async_trait;
use liberado_commands::{CommandContext, CommandResult, SlashCommand, StatusInfo, dispatch, parse};
use liberado_conversation_store::{Author, ConversationHeader, ConversationStore, Ulid};
use liberado_main_agent::ChatSessions;
use liberado_session::{DomainHint, GoalSpec, SessionGrant, SessionOrigin};

use crate::state::AppState;
use crate::sticky::StickySession;

/// Sticky Telegram conversation + slash-command adapter over the same face agent as HTTP chat.
///
/// `session_id` is shared (`Arc`) with the chat-delivering notifier so a cron brief appends into the
/// *same* conversation a reply continues — that shared sticky id is the whole mechanism behind
/// "replying to a brief has the brief in context" (see `docs/ideas/cron-delivery-timing-idea.md`).
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
        // Telegram reaches `ChatSessions::turn` directly, so the HTTP middleware that refuses new
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
        sessions
            .turn(id, user_text)
            .await
            .map_err(|e| e.to_string())
    }

    fn chat_sessions(&self) -> Result<Arc<ChatSessions>, String> {
        self.state
            .chat
            .clone()
            .ok_or_else(|| "chat is disabled".into())
    }

    async fn handle_slash(&self, text: &str) -> Result<String, String> {
        // Telegram-friendly: `/model <id>` hot-swaps (shared parse only knows bare `/model`).
        if let Some(rest) = text.strip_prefix("/model") {
            let rest = rest.trim();
            if !rest.is_empty() {
                let model = rest.split_whitespace().next().unwrap_or(rest);
                return self.select_model(model).await;
            }
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

    async fn select_model(&self, model: &str) -> Result<String, String> {
        let Some(provider) = self.state.provider.as_ref() else {
            return Err("no inference provider configured".into());
        };
        let previous = provider.model();
        provider.set_model(model.to_string());
        crate::state::resync_compaction_trigger_for_face_model(
            &self.state,
            provider.model().as_str(),
        );
        Ok(format!(
            "Model switched: {previous} → {}\n(hot-swap; no restart)",
            provider.model()
        ))
    }

    async fn apply_result(
        &self,
        result: CommandResult,
        ctx: &mut TelegramCommandContext,
    ) -> Option<String> {
        match result {
            CommandResult::HelpShown
            | CommandResult::StatusShown
            | CommandResult::ModelInfoShown
            | CommandResult::SessionInfoShown
            | CommandResult::ProfileInfoShown
            | CommandResult::None => None,

            // Telegram has no picker widget, and a sticky Telegram chat is not the place to be
            // re-authorising a session anyway — the switch is a deliberate, per-conversation act.
            CommandResult::OpenProfileBrowser => Some(
                "Session profiles are switched from the web UI or TUI with /profile.".into(),
            ),

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

            CommandResult::OpenModelBrowser => {
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

            CommandResult::SessionListed
            | CommandResult::OpenSessionBrowser
            | CommandResult::OpenGoalSwitcher => {
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

            CommandResult::SessionSwitched { id } => {
                if let Ok(ulid) = id.parse::<Ulid>() {
                    ctx.session_id = Some(ulid.to_string());
                    return Some(format!("Switched to session {ulid}"));
                }
                let sessions = self.chat_sessions().ok()?;
                let headers = sessions.list().await.ok()?;
                if let Some(h) = headers.iter().find(|h| h.id.to_string().starts_with(&id)) {
                    ctx.session_id = Some(h.id.to_string());
                    Some(format!("Switched to session {}", h.id))
                } else {
                    Some(format!("No session matching '{id}'"))
                }
            }

            CommandResult::SessionClosed { id } => {
                ctx.session_id = None;
                Some(match id {
                    Some(id) => format!("Closed session {id}. Use /sessions to resume later."),
                    None => "No active session.".into(),
                })
            }

            CommandResult::JoinGoalSession { id } => {
                if let Some(snap) = self.state.goals.snapshot(&id).await {
                    let desc = &snap.session.goal.description;
                    Some(format!(
                        "Goal session {}\nstatus: {:?}\n{}\n\n\
                         (Live event stream isn't on Telegram yet — use the API/WebUI. \
                         /spawn still works from here.)",
                        snap.session.id, snap.session.status, desc
                    ))
                } else {
                    let goals = self.state.goals.list().await;
                    if let Some(g) = goals.iter().find(|g| g.id.starts_with(&id)) {
                        Some(format!(
                            "Goal session {}\nstatus: {:?}\n{}",
                            g.id, g.status, g.goal.description
                        ))
                    } else {
                        Some(format!("No goal session matching '{id}'"))
                    }
                }
            }

            CommandResult::BackToPrimary => {
                Some("Back on primary chat (Telegram only has one input focus).".into())
            }

            CommandResult::SpawnGoalSession { domain, goal } => {
                let origin = self
                    .session_id
                    .get()
                    .await
                    .map(|id| SessionOrigin::from_conversation(id.to_string()));

                // Profile-first when the token isn't a well-known pack name.
                let profile = if matches!(domain.as_str(), "life" | "coding" | "dispatch") {
                    None
                } else {
                    Some(domain.clone())
                };
                let domain_fallback = profile.as_deref().unwrap_or(domain.as_str());
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
                            profile.as_deref().unwrap_or(domain.as_str())
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
                    description: goal.clone(),
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
                    overrides: serde_json::to_value(&resolved.overrides)
                        .unwrap_or(serde_json::Value::Null),
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

            // The coding-goal surface is a TUI view (role timeline, gate ballots, diffs). Telegram
            // has no place to render it, and a half-rendered gate is worse than an honest pointer:
            // the whole value of watching a quorum vote is seeing all of it.
            CommandResult::StartCodingGoal { .. }
            | CommandResult::OpenGoalView
            | CommandResult::GoalStatus
            | CommandResult::ParkGoalSession
            | CommandResult::ResumeGoalSession { .. }
            | CommandResult::CancelGoalSession => Some(
                "Coding goals run in the TUI, which can show the role timeline, the completion                  gate's ballots, and diffs. Use /spawn here to start a non-coding session."
                    .into(),
            ),

            CommandResult::ForkRequested {
                parent_id,
                after_turn,
            } => match self.fork_session(&parent_id, after_turn).await {
                Ok(msg) => Some(msg),
                Err(e) => Some(format!("Fork failed: {e}")),
            },

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
