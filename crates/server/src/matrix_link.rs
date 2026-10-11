//! Start the Matrix room when its environment is complete.
//!
//! Telegram stays the background channel. This room gets its own binding, its own
//! permission sink, and its own decision label. A missing or failed setup logs one
//! warning and leaves the daemon running.

use std::sync::Arc;

use liberado_common::{ChannelKind, ProposalSigner};
use liberado_config::TelegramApprovalsTuning;
use liberado_daemon::Daemon;
use liberado_main_agent::ChatSessions;
use liberado_matrix::{
    Activation, EnvSnapshot, MatrixSettings, RunningMatrix, activation, connect,
};
use liberado_messaging::MessagingChannel;
use liberado_notify::ChannelNotifier;
use liberado_provider::Provider;
use liberado_telegram_approvals::{ApprovalBot, PermissionResolver};
use liberado_vault::Vault;

use crate::approvals::ChannelPermissionSink;
use crate::bindings::{BindingKey, ChannelBindings};
use crate::channels::ChannelRuntime;
use crate::state::AppState;
use crate::telegram::TextChatBridge;

/// Read the environment and start Matrix on a background task when it is complete.
pub fn spawn_matrix(
    state: &Arc<AppState>,
    daemon: &Daemon,
    provider: Option<&Arc<dyn Provider>>,
    config: &liberado_bootstrap::Config,
    channels: &ChannelRuntime,
    resolver: Arc<PermissionResolver>,
) {
    let Some(settings) = accept(activation(&EnvSnapshot::from_env())) else {
        return;
    };
    let boot = MatrixBoot {
        state: Arc::clone(state),
        vault: daemon.vault().clone(),
        signer: daemon.signer().clone(),
        provider: provider.cloned(),
        tuning: config.tuning.telegram_approvals.clone(),
        bindings: channels.bindings.clone(),
        resolver,
    };
    tokio::spawn(async move {
        match connect(&settings, &boot.state.data_dir).await {
            Ok(running) => run_matrix(running, boot).await,
            Err(error) => {
                tracing::warn!(%error, "matrix: startup check failed; matrix stays off");
            }
        }
    });
}

fn accept(decision: Activation) -> Option<MatrixSettings> {
    match decision {
        Activation::Off => {
            tracing::debug!("matrix: not configured");
            None
        }
        Activation::Partial { missing } => {
            tracing::warn!(
                missing = %missing.join(", "),
                "matrix: incomplete environment; matrix stays off"
            );
            None
        }
        Activation::Invalid { reason } => {
            tracing::warn!(%reason, "matrix: invalid environment; matrix stays off");
            None
        }
        Activation::On(settings) => Some(*settings),
    }
}

struct MatrixBoot {
    state: Arc<AppState>,
    vault: Vault,
    signer: ProposalSigner,
    provider: Option<Arc<dyn Provider>>,
    tuning: TelegramApprovalsTuning,
    bindings: ChannelBindings,
    resolver: Arc<PermissionResolver>,
}

async fn run_matrix(running: RunningMatrix, boot: MatrixBoot) {
    let MatrixBoot {
        state,
        vault,
        signer,
        provider,
        tuning,
        bindings,
        resolver,
    } = boot;
    let Some(provider) = provider else {
        tracing::warn!("matrix: no provider; matrix stays off");
        running.stop();
        return;
    };
    let room_id = running.room_id.clone();
    let user_id = running.user_id.clone();
    let transport: Arc<dyn MessagingChannel> = running.channel.clone();
    resolver.add_resolved_elsewhere(Arc::new(running.updater));
    let key = matrix_key(&room_id, &user_id);
    let mut bot = ApprovalBot::new(Arc::clone(&transport), vault, signer, provider, tuning)
        .with_resolver(resolver)
        .with_decision_channel(ChannelKind::Matrix);
    if let Some(chat) = state.chat.clone() {
        bot = attach_chat(bot, &state, &bindings, &chat, &key, transport).await;
    } else {
        tracing::warn!("matrix: chat is off; the room is not bound");
    }
    tracing::info!(room = %room_id, user = %user_id, "matrix: room is listening");
    tokio::spawn(bot.run());
}

async fn attach_chat(
    bot: ApprovalBot,
    state: &Arc<AppState>,
    bindings: &ChannelBindings,
    chat: &Arc<ChatSessions>,
    key: &BindingKey,
    transport: Arc<dyn MessagingChannel>,
) -> ApprovalBot {
    let notifier = Arc::new(ChannelNotifier::new(transport));
    chat.set_permission_sink(
        ChannelKind::Matrix,
        Arc::new(ChannelPermissionSink::new(notifier)),
    );
    if let Err(error) = open_session(bindings, key, chat).await {
        tracing::warn!(%error, "matrix: session was not created");
    }
    bot.with_chat(Arc::new(TextChatBridge {
        state: Arc::clone(state),
        bindings: bindings.clone(),
        key: key.clone(),
    }))
    .with_command_menu(command_menu())
}

async fn open_session(
    bindings: &ChannelBindings,
    key: &BindingKey,
    chat: &ChatSessions,
) -> Result<(), String> {
    bindings
        .get_or_create(key, || async {
            chat.create(Some("Matrix".into()))
                .await
                .map_err(|error| error.to_string())
        })
        .await?;
    Ok(())
}

fn matrix_key(room_id: &str, user_id: &str) -> BindingKey {
    BindingKey {
        channel: ChannelKind::Matrix,
        peer: room_id.to_string(),
        bot: user_id.to_string(),
    }
}

fn command_menu() -> Vec<(String, String)> {
    liberado_commands::telegram_commands()
        .into_iter()
        .map(|(command, description)| (command.to_string(), description.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use liberado_common::ChannelKind;
    use liberado_conversation_store::Ulid;

    use crate::bindings::ChannelBindings;

    #[tokio::test]
    async fn the_main_room_gets_its_own_binding() {
        let key = super::matrix_key(
            "!room:matrix.homelab.local",
            "@liberado:matrix.homelab.local",
        );
        assert_eq!(key.channel, ChannelKind::Matrix);
        assert_eq!(key.peer, "!room:matrix.homelab.local");
        assert_eq!(key.bot, "@liberado:matrix.homelab.local");
        let bindings = ChannelBindings::ephemeral();
        let created = bindings
            .get_or_create(&key, || async { Ok::<_, &str>(Ulid::new()) })
            .await
            .expect("create");
        assert_eq!(bindings.get(&key).await, Some(created));
        assert!(bindings.background_session().await.is_none());
        let again = bindings
            .get_or_create(&key, || async { Err("reused") })
            .await
            .expect("reuse");
        assert_eq!(again, created);
    }
}
