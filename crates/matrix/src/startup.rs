//! Startup check, then the listener. A failed check leaves the daemon running.

use std::path::Path;
use std::sync::Arc;

use tokio::task::JoinHandle;

use crate::channel::{MatrixCardUpdater, MatrixChannel};
use crate::client::{MatrixClient, member_present};
use crate::config::MatrixSettings;
use crate::filter::{AcceptPolicy, in_user_namespace};
use crate::hub::RoomQueues;
use crate::listener::{ListenerState, bind_and_serve};
use crate::txn_log::TxnLog;

/// A running listener and the channel for the main room.
pub struct RunningMatrix {
    pub channel: Arc<MatrixChannel>,
    pub updater: MatrixCardUpdater,
    pub room_id: String,
    pub user_id: String,
    listener: JoinHandle<()>,
}

impl RunningMatrix {
    /// Stop the listener. Dropping the channel ends the room queue.
    pub fn stop(&self) {
        self.listener.abort();
    }
}

/// Check the homeserver, join the room when needed, and listen for transactions.
pub async fn connect(settings: &MatrixSettings, data_dir: &Path) -> Result<RunningMatrix, String> {
    let client = MatrixClient::new(
        &settings.homeserver,
        &settings.as_token,
        &settings.main_user,
    )?;
    let room_id = prepare_room(&client, settings).await?;
    let queues = Arc::new(RoomQueues::new());
    let channel = Arc::new(MatrixChannel::open(
        client.clone(),
        room_id.clone(),
        &queues,
    ));
    let state = listener_state(settings, &client, &queues, &room_id, data_dir);
    let listener = bind_and_serve(settings.listen, Arc::new(state)).await?;
    Ok(RunningMatrix {
        updater: MatrixCardUpdater::new(Arc::clone(&channel)),
        channel,
        room_id,
        user_id: settings.main_user.clone(),
        listener,
    })
}

/// Whoami, alias resolution, and join. The error text names the step and not the token.
pub async fn prepare_room(
    client: &MatrixClient,
    settings: &MatrixSettings,
) -> Result<String, String> {
    let who = client
        .whoami()
        .await
        .map_err(|error| format!("matrix whoami failed: {error}"))?;
    whoami_ok(&who, settings)?;
    let room_id = resolve_room(client, &settings.main_room).await?;
    ensure_member(client, &room_id, &settings.main_user).await?;
    Ok(room_id)
}

fn whoami_ok(returned: &str, settings: &MatrixSettings) -> Result<(), String> {
    if returned.is_empty()
        || returned.eq_ignore_ascii_case(&settings.main_user)
        || in_user_namespace(returned, &settings.server_name)
    {
        return Ok(());
    }
    Err(format!(
        "matrix whoami user {returned} is outside the liberado namespace"
    ))
}

async fn resolve_room(client: &MatrixClient, room: &str) -> Result<String, String> {
    if room.starts_with('!') {
        return Ok(room.to_string());
    }
    client
        .resolve_alias(room)
        .await
        .map_err(|error| format!("matrix alias resolve failed: {error}"))
}

async fn ensure_member(client: &MatrixClient, room_id: &str, user_id: &str) -> Result<(), String> {
    let members = client
        .joined_members(room_id)
        .await
        .map_err(|error| format!("matrix joined_members failed: {error}"))?;
    if member_present(&members, user_id) {
        return Ok(());
    }
    client
        .join_room(room_id)
        .await
        .map_err(|error| format!("matrix join failed: {error}"))?;
    Ok(())
}

fn listener_state(
    settings: &MatrixSettings,
    client: &MatrixClient,
    queues: &Arc<RoomQueues>,
    room_id: &str,
    data_dir: &Path,
) -> ListenerState {
    let mut known = vec![settings.main_room.clone(), room_id.to_string()];
    known.sort();
    known.dedup();
    ListenerState::new(
        settings.hs_token.clone(),
        Arc::new(TxnLog::load(data_dir.join("matrix-txns.json"))),
        Arc::clone(queues),
        AcceptPolicy {
            owner: settings.owner.clone(),
            server_name: settings.server_name.clone(),
            main_user: settings.main_user.clone(),
            sender_localpart: settings.sender_localpart.clone(),
        },
        client.clone(),
        settings.main_user.clone(),
        known,
    )
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
