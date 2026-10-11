//! Compose the chat channels that are configured.
//!
//! Telegram starts only when `LIBERADO_TELEGRAM_BOT_TOKEN` and `LIBERADO_TELEGRAM_CHAT_ID`
//! are both set. A later channel starts only when its own env is set. Cron delivery, the
//! text bridge, and approval routing share one [`ChannelBindings`]
//! and, for Telegram, one [`BindingKey`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use liberado_conversation_store::Ulid;
use liberado_daemon::Daemon;
use liberado_main_agent::ChatSessions;
use liberado_provider::Provider;
use tokio::sync::Mutex;

use crate::bindings::{BindingKey, BindingLoad, ChannelBindings};
use crate::cron_delivery::ChatDeliveringNotifier;
use crate::state::AppState;
use crate::telegram::TextChatBridge;

/// Bindings plus the Telegram peer, when that env is set.
pub struct ChannelRuntime {
    pub bindings: ChannelBindings,
    activity: Arc<Mutex<Option<Instant>>>,
    telegram: Option<TelegramLink>,
}

struct TelegramLink {
    key: BindingKey,
}

/// Load `<data_dir>/channel-bindings.json`. A legacy `telegram-sticky-session` file is adopted
/// for the configured Telegram peer, or published for routing when that env is unset.
pub async fn resolve_channels(chat: Option<&Arc<ChatSessions>>) -> ChannelRuntime {
    let telegram = telegram_link_from_env();
    let adopt_key = telegram.as_ref().map(|link| link.key.clone());
    ChannelRuntime {
        bindings: load_bindings(chat, adopt_key).await,
        activity: Arc::new(Mutex::new(None)),
        telegram,
    }
}

fn telegram_link_from_env() -> Option<TelegramLink> {
    let token = std::env::var("LIBERADO_TELEGRAM_BOT_TOKEN").ok()?;
    let chat_id = std::env::var("LIBERADO_TELEGRAM_CHAT_ID").ok()?;
    Some(TelegramLink {
        key: BindingKey::telegram(chat_id, telegram_bot_id(&token)),
    })
}

/// Numeric bot user id from a Telegram token (`12345:secret`). A token without that prefix
/// records the stable label `telegram`. The secret is never stored.
pub fn telegram_bot_id(token: &str) -> String {
    match token.split(':').next() {
        Some(prefix) if !prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_digit()) => {
            prefix.to_string()
        }
        _ => "telegram".to_string(),
    }
}

async fn load_bindings(
    chat: Option<&Arc<ChatSessions>>,
    adopt_key: Option<BindingKey>,
) -> ChannelBindings {
    let Some(chat_sessions) = chat else {
        return ChannelBindings::ephemeral();
    };
    let data = liberado_bootstrap::data_dir();
    let sessions = Arc::clone(chat_sessions);
    ChannelBindings::load(
        BindingLoad {
            path: data.join("channel-bindings.json"),
            legacy_path: data.join("telegram-sticky-session"),
            adopt_key,
        },
        move |id| {
            let sessions = Arc::clone(&sessions);
            async move { session_exists(&sessions, id).await }
        },
    )
    .await
}

async fn session_exists(sessions: &ChatSessions, id: Ulid) -> bool {
    sessions
        .list()
        .await
        .map(|headers| headers.iter().any(|header| header.id == id))
        .unwrap_or(false)
}

/// When Telegram and a chat surface exist, fold each cron brief into the bound conversation.
/// Without either, the daemon keeps the notifier `configure_daemon` set.
pub fn wrap_cron_notifier(
    daemon: Daemon,
    chat: Option<&Arc<ChatSessions>>,
    config: &liberado_bootstrap::Config,
    channels: &ChannelRuntime,
) -> Daemon {
    let (Some(chat_sessions), Some(link)) = (chat, channels.telegram.as_ref()) else {
        return daemon;
    };
    let Some(inner) = liberado_notify::TelegramNotifier::from_env() else {
        return daemon;
    };
    let notifier = ChatDeliveringNotifier::new(
        Arc::new(inner),
        Arc::clone(chat_sessions),
        channels.bindings.clone(),
        link.key.clone(),
        Arc::clone(&channels.activity),
        Duration::from_secs(config.tuning.cron_delivery.quiet_delay_secs),
        Duration::from_secs(config.tuning.cron_delivery.deliver_by_secs),
    );
    tracing::info!(
        "cron delivery: folding briefs into the sticky Telegram chat (quiet-delay defer)"
    );
    daemon.with_notifier(Arc::new(notifier))
}

/// Start every configured channel. Telegram starts only when its env is set.
pub fn spawn_configured_channels(
    state: &Arc<AppState>,
    daemon: &Daemon,
    provider: Option<&Arc<dyn Provider>>,
    config: &liberado_bootstrap::Config,
    channels: &ChannelRuntime,
    resolver: Arc<liberado_telegram_approvals::PermissionResolver>,
) {
    if let Some(link) = channels.telegram.as_ref() {
        // Register even when the bot itself does not start. Background cards are sent by the
        // daemon notifier, which shares this card book.
        register_telegram_resolution(&resolver);
        spawn_telegram(state, daemon, provider, config, channels, link, resolver);
    }
}

fn spawn_telegram(
    state: &Arc<AppState>,
    daemon: &Daemon,
    provider: Option<&Arc<dyn Provider>>,
    config: &liberado_bootstrap::Config,
    channels: &ChannelRuntime,
    link: &TelegramLink,
    resolver: Arc<liberado_telegram_approvals::PermissionResolver>,
) {
    let Some(provider) = provider else {
        return;
    };
    let Some(bot) = liberado_telegram_approvals::ApprovalBot::from_env(
        daemon.vault().clone(),
        daemon.signer().clone(),
        provider.clone(),
        config.tuning.telegram_approvals.clone(),
    ) else {
        return;
    };
    let bot = attach_telegram_chat(bot.with_resolver(resolver), state, channels, link);
    tokio::spawn(bot.run());
}

fn register_telegram_resolution(resolver: &liberado_telegram_approvals::PermissionResolver) {
    resolver.set_push_surface("telegram");
    if let Some(updater) = liberado_notify::TelegramCardUpdater::from_env() {
        resolver.add_resolved_elsewhere(Arc::new(updater));
    }
}

fn attach_telegram_chat(
    bot: liberado_telegram_approvals::ApprovalBot,
    state: &Arc<AppState>,
    channels: &ChannelRuntime,
    link: &TelegramLink,
) -> liberado_telegram_approvals::ApprovalBot {
    if state.chat.is_none() {
        return bot;
    }
    let command_menu = liberado_commands::telegram_commands()
        .into_iter()
        .map(|(command, description)| (command.to_string(), description.to_string()))
        .collect();
    tracing::info!(
        "Telegram free-form chat surface attached (slash commands enabled + menu registered)"
    );
    bot.with_chat(Arc::new(TextChatBridge {
        state: state.clone(),
        bindings: channels.bindings.clone(),
        key: link.key.clone(),
    }))
    .with_activity_tracker(Arc::clone(&channels.activity))
    .with_command_menu(command_menu)
}

/// Ping Telegram when a session awaits input and nobody has the stream open.
/// The message text is the one the hub has always sent.
pub fn attach_session_alert(
    goals: liberado_session::GoalSessionHub,
) -> liberado_session::GoalSessionHub {
    let Some(notifier) = liberado_notify::TelegramNotifier::from_env() else {
        return goals;
    };
    tracing::info!("session alerts: telegram notifier attached");
    goals.with_alert(Arc::new(NotifySessionAlert(Arc::new(notifier))))
}

struct NotifySessionAlert(Arc<dyn liberado_notify::Notifier>);

#[async_trait::async_trait]
impl liberado_session::SessionAlert for NotifySessionAlert {
    async fn session_needs_you(&self, session_id: &str, prompt: &str) {
        let message = format!(
            "Liberado: a session needs your input.\n\
             session: {session_id}\n\
             {prompt}\n\
             Answer in the TUI or via POST /api/goals/{session_id}/message"
        );
        match self.0.notify(&message).await {
            Ok(()) => tracing::info!(%session_id, "session alert sent — nobody was watching"),
            Err(error) => {
                tracing::warn!(%error, %session_id, "session alert notification failed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::telegram_bot_id;

    #[test]
    fn bot_id_is_the_numeric_token_prefix() {
        assert_eq!(telegram_bot_id("12345:secret"), "12345");
        assert_eq!(telegram_bot_id("not-a-number:secret"), "telegram");
        assert_eq!(telegram_bot_id("telegram"), "telegram");
        assert_eq!(telegram_bot_id(":secret"), "telegram");
    }
}
