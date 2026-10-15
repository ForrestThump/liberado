//! One Matrix user and one direct room per Agents-shelf session.
//!
//! Reconcile creates what is missing, reuses a stored slug, and leaves the room
//! when the session is gone. The binding stays, marked inactive, so the slug
//! and the room stay reserved. A failure is logged and retried on the next
//! reconcile. It does not stop the daemon.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use liberado_common::{ChannelKind, ProposalSigner};
use liberado_config::TelegramApprovalsTuning;
use liberado_conversation_store::Ulid;
use liberado_matrix::{
    AgentDirectory, AgentRecord, AgentSeat, MatrixCardUpdater, MatrixChannel, MatrixClient,
    RoomQueues, reconcile_agents,
};
use liberado_provider::Provider;
use liberado_telegram_approvals::{ApprovalBot, PermissionResolver};
use liberado_vault::Vault;
use tokio::task::JoinHandle;

use crate::bindings::{BindingKey, ChannelBindings};
use crate::matrix_agent_chat::{PinnedAgentChat, dm_command_menu};
use crate::matrix_cards::MatrixCards;
use crate::state::AppState;
use crate::telegram::TextChatBridge;

struct LiveBot {
    room_id: String,
    handle: JoinHandle<()>,
}

/// What [`MatrixAgents`] needs from process startup. One value, so the
/// constructor stays inside the argument limit.
pub struct AgentLinks {
    pub state: Weak<AppState>,
    pub client: MatrixClient,
    pub queues: Arc<RoomQueues>,
    pub known_users: Arc<Mutex<std::collections::HashSet<String>>>,
    pub server_name: String,
    pub owner: String,
    pub directory_path: PathBuf,
    pub bindings: ChannelBindings,
    pub cards: Arc<MatrixCards>,
    pub resolver: Arc<PermissionResolver>,
    pub vault: Vault,
    pub signer: ProposalSigner,
    pub provider: Arc<dyn Provider>,
    pub tuning: TelegramApprovalsTuning,
}

/// Homeserver client, directory, and the bots for active agent rooms.
pub struct MatrixAgents {
    state: Weak<AppState>,
    client: MatrixClient,
    queues: Arc<RoomQueues>,
    known_users: Arc<Mutex<std::collections::HashSet<String>>>,
    server_name: String,
    owner: String,
    directory_path: PathBuf,
    bindings: ChannelBindings,
    cards: Arc<MatrixCards>,
    resolver: Arc<PermissionResolver>,
    vault: Vault,
    signer: ProposalSigner,
    provider: Arc<dyn Provider>,
    tuning: TelegramApprovalsTuning,
    reconcile_lock: tokio::sync::Mutex<()>,
    bots: Mutex<HashMap<String, LiveBot>>,
}

impl MatrixAgents {
    pub fn new(links: AgentLinks) -> Self {
        Self {
            state: links.state,
            client: links.client,
            queues: links.queues,
            known_users: links.known_users,
            server_name: links.server_name,
            owner: links.owner,
            directory_path: links.directory_path,
            bindings: links.bindings,
            cards: links.cards,
            resolver: links.resolver,
            vault: links.vault,
            signer: links.signer,
            provider: links.provider,
            tuning: links.tuning,
            reconcile_lock: tokio::sync::Mutex::new(()),
            bots: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn reconcile and return. Safe to call from an HTTP handler.
    pub fn schedule(self: &Arc<Self>) {
        let agents = Arc::clone(self);
        tokio::spawn(async move {
            agents.reconcile().await;
        });
    }

    async fn reconcile(&self) {
        let _guard = self.reconcile_lock.lock().await;
        let Some(chat) = self.chat() else {
            return;
        };
        match agent_seats(&chat).await {
            Ok(seats) => self.reconcile_seats(&seats).await,
            Err(error) => tracing::warn!(%error, "matrix: agent list failed"),
        }
    }

    fn chat(&self) -> Option<Arc<liberado_main_agent::ChatSessions>> {
        self.state.upgrade().and_then(|state| state.chat.clone())
    }

    async fn reconcile_seats(&self, seats: &[AgentSeat]) {
        let mut directory = AgentDirectory::load(&self.directory_path).await;
        let report = reconcile_agents(
            &self.client,
            &self.server_name,
            &self.owner,
            &mut directory,
            seats,
        )
        .await;
        note_errors(&report.errors);
        self.apply_ensured(&report.ensured).await;
        self.apply_deactivated(&report.deactivated).await;
    }

    async fn apply_ensured(&self, records: &[AgentRecord]) {
        for record in records {
            self.activate(record).await;
        }
    }

    async fn apply_deactivated(&self, records: &[AgentRecord]) {
        for record in records {
            self.deactivate(record).await;
        }
    }

    async fn activate(&self, record: &AgentRecord) {
        if let Err(error) = self.sync_binding(record, true).await {
            tracing::warn!(%error, session = %record.session_id, "matrix: agent binding was not stored");
            return;
        }
        self.remember_user(&record.mxid);
        if let Err(error) = self.ensure_bot(record) {
            tracing::warn!(%error, session = %record.session_id, "matrix: agent room is not listening");
        }
    }

    async fn deactivate(&self, record: &AgentRecord) {
        if let Err(error) = self.sync_binding(record, false).await {
            tracing::warn!(%error, session = %record.session_id, "matrix: agent binding was not archived");
        }
        self.stop_bot(&record.session_id);
    }

    async fn sync_binding(&self, record: &AgentRecord, active: bool) -> Result<(), String> {
        let room = record
            .room_id
            .clone()
            .ok_or_else(|| format!("agent {} has no room", record.session_id))?;
        let session: Ulid = record
            .session_id
            .parse()
            .map_err(|_| format!("agent session id {} is not a ulid", record.session_id))?;
        let key = BindingKey {
            channel: ChannelKind::Matrix,
            peer: room,
            bot: record.mxid.clone(),
        };
        self.bindings.set(&key, Some(session)).await;
        self.bindings.set_active(&key, active).await;
        Ok(())
    }

    fn remember_user(&self, mxid: &str) {
        let mut known = self
            .known_users
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        known.insert(mxid.to_string());
    }

    fn ensure_bot(&self, record: &AgentRecord) -> Result<(), String> {
        let Some(room) = record.room_id.as_deref() else {
            return Err(format!("agent {} has no room", record.session_id));
        };
        if self.bot_is_live(&record.session_id, room) {
            return Ok(());
        }
        self.stop_bot(&record.session_id);
        self.spawn_bot(record, room)
    }

    fn bot_is_live(&self, session_id: &str, room: &str) -> bool {
        let bots = self
            .bots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        bots.get(session_id)
            .is_some_and(|bot| bot.room_id == room && !bot.handle.is_finished())
    }

    fn stop_bot(&self, session_id: &str) {
        self.cards.remove(session_id);
        let mut bots = self
            .bots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(bot) = bots.remove(session_id) {
            bot.handle.abort();
        }
    }

    fn spawn_bot(&self, record: &AgentRecord, room: &str) -> Result<(), String> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| "app state dropped".to_string())?;
        let channel = Arc::new(MatrixChannel::open(
            self.client.for_user(&record.mxid),
            room.to_string(),
            &self.queues,
        ));
        self.cards.insert(&record.session_id, Arc::clone(&channel));
        self.resolver
            .add_resolved_elsewhere(Arc::new(MatrixCardUpdater::new(Arc::clone(&channel))));
        let chat = Arc::new(PinnedAgentChat::new(TextChatBridge {
            state,
            bindings: self.bindings.clone(),
            key: BindingKey {
                channel: ChannelKind::Matrix,
                peer: room.to_string(),
                bot: record.mxid.clone(),
            },
        }));
        let bot = ApprovalBot::new(
            channel,
            self.vault.clone(),
            self.signer.clone(),
            Arc::clone(&self.provider),
            self.tuning.clone(),
        )
        .with_resolver(Arc::clone(&self.resolver))
        .with_decision_channel(ChannelKind::Matrix)
        .with_chat(chat)
        .with_command_menu(dm_command_menu());
        let handle = tokio::spawn(async move { bot.run().await });
        self.store_bot(&record.session_id, room, handle);
        Ok(())
    }

    fn store_bot(&self, session_id: &str, room: &str, handle: JoinHandle<()>) {
        let mut bots = self
            .bots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        bots.insert(
            session_id.to_string(),
            LiveBot {
                room_id: room.to_string(),
                handle,
            },
        );
    }
}

fn note_errors(errors: &[String]) {
    for error in errors {
        tracing::warn!(%error, "matrix: agent reconcile failed");
    }
}

async fn agent_seats(chat: &liberado_main_agent::ChatSessions) -> Result<Vec<AgentSeat>, String> {
    let headers = chat.list().await.map_err(|error| error.to_string())?;
    Ok(headers
        .into_iter()
        .filter(liberado_main_agent::is_agent_dm_session)
        .map(seat_from_header)
        .collect())
}

fn seat_from_header(header: liberado_conversation_store::ConversationHeader) -> AgentSeat {
    AgentSeat {
        session_id: header.id.to_string(),
        title: header.title.unwrap_or_default(),
    }
}
