//! One puppet and one direct room per agent session.
//!
//! The directory is `<data_dir>/matrix-agents.json`. A slug is written before
//! the homeserver calls, and it does not change when the title changes. A
//! missing session is marked inactive. The row stays, so the slug and the room
//! stay reserved and a later seat with the same session id can rejoin.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client::{MatrixClient, member_present};
use crate::puppet::{
    already_invited, already_left, invite_user, leave_room, register_or_existing, set_direct_room,
    set_display_name, set_room_name,
};
use crate::slug::{allocate_slug, puppet_localpart, puppet_mxid, slug_source};

const FILE_VERSION: u32 = 1;

/// An agent that should have a direct room right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSeat {
    pub session_id: String,
    pub title: String,
}

/// Persisted puppet. `active` is false after the agent leaves the shelf.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub session_id: String,
    pub slug: String,
    pub mxid: String,
    pub room_id: Option<String>,
    pub display_name: String,
    #[serde(default = "active_by_default")]
    pub active: bool,
    /// Leave succeeded, or the puppet was already out of the room.
    #[serde(default)]
    pub departed: bool,
    /// `m.direct` on the puppet includes this room.
    #[serde(default)]
    pub direct_set: bool,
}

fn active_by_default() -> bool {
    true
}

/// What one reconcile changed. Errors are logged by the caller and retried next time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub ensured: Vec<AgentRecord>,
    pub deactivated: Vec<AgentRecord>,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AgentFile {
    version: u32,
    agents: Vec<AgentRecord>,
}

/// Slug and room reservations. `path` is `None` for an in-memory directory.
pub struct AgentDirectory {
    path: Option<PathBuf>,
    agents: Vec<AgentRecord>,
}

impl AgentDirectory {
    pub fn memory() -> Self {
        Self {
            path: None,
            agents: Vec::new(),
        }
    }

    /// Missing or unreadable files start empty. A bad file is not deleted.
    pub async fn load(path: &Path) -> Self {
        let agents = match tokio::fs::read_to_string(path).await {
            Ok(text) => parse_agents(&text).unwrap_or_else(|error| {
                tracing::warn!(%error, path = %path.display(), "matrix: agent directory unreadable");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Self {
            path: Some(path.to_path_buf()),
            agents,
        }
    }

    pub fn get(&self, session_id: &str) -> Option<&AgentRecord> {
        self.agents
            .iter()
            .find(|agent| agent.session_id == session_id)
    }

    pub fn records(&self) -> &[AgentRecord] {
        &self.agents
    }

    pub async fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let body = serde_json::to_string_pretty(&AgentFile {
            version: FILE_VERSION,
            agents: self.agents.clone(),
        })
        .map_err(|error| format!("encode agent directory: {error}"))?;
        write_file(path, &body).await
    }

    fn upsert(&mut self, record: AgentRecord) {
        if let Some(slot) = self
            .agents
            .iter_mut()
            .find(|agent| agent.session_id == record.session_id)
        {
            *slot = record;
        } else {
            self.agents.push(record);
        }
    }

    fn taken_slugs(&self) -> HashSet<String> {
        self.agents.iter().map(|agent| agent.slug.clone()).collect()
    }
}

fn parse_agents(text: &str) -> Result<Vec<AgentRecord>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let file: AgentFile = serde_json::from_str(text).map_err(|error| error.to_string())?;
    Ok(file.agents)
}

async fn write_file(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let tmp = tmp_path(path);
    tokio::fs::write(&tmp, body)
        .await
        .map_err(|error| format!("write {}: {error}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|error| format!("rename {}: {error}", path.display()))
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Display name stored on the puppet and the room. A blank title stays `Agent`.
pub fn display_name(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        "Agent".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Create what is missing and deactivate seats that are gone.
///
/// One agent's failure is recorded and the next agent still runs. A second call
/// with the same seats does not create another user or room.
pub async fn reconcile_agents(
    client: &MatrixClient,
    server_name: &str,
    owner: &str,
    directory: &mut AgentDirectory,
    seats: &[AgentSeat],
) -> ReconcileReport {
    let mut report = ReconcileReport::default();
    let mut present = HashSet::new();
    for seat in seats {
        present.insert(seat.session_id.clone());
        match ensure_seat(client, server_name, owner, directory, seat).await {
            Ok(record) => report.ensured.push(record),
            Err(error) => report.errors.push(error),
        }
    }
    deactivate_absent(client, directory, &present, &mut report).await;
    report
}

async fn ensure_seat(
    client: &MatrixClient,
    server_name: &str,
    owner: &str,
    directory: &mut AgentDirectory,
    seat: &AgentSeat,
) -> Result<AgentRecord, String> {
    let mut record = assigned(directory, server_name, seat).await?;
    let puppet = client.for_user(&record.mxid);
    register_or_existing(client, &puppet_localpart(&record.slug))
        .await
        .map_err(|error| format!("register {}: {error}", record.mxid))?;
    let name = display_name(&seat.title);
    let renamed = record.display_name != name;
    if renamed {
        let updated = set_display_name(&puppet, &name).await;
        if let Err(error) = updated {
            return Err(format!("display name for {}: {error}", record.mxid));
        }
    }
    let created = open_room(&puppet, owner, &name, &mut record, directory).await?;
    if !record.active {
        rejoin(&puppet, owner, &record).await?;
        record.active = true;
        record.departed = false;
        record.direct_set = false;
    }
    refresh_names(&puppet, &name, created, renamed, &mut record).await?;
    ensure_direct(&puppet, owner, &mut record).await?;
    directory.upsert(record.clone());
    directory.save().await?;
    Ok(record)
}

async fn assigned(
    directory: &mut AgentDirectory,
    server_name: &str,
    seat: &AgentSeat,
) -> Result<AgentRecord, String> {
    if let Some(existing) = directory.get(&seat.session_id) {
        return Ok(existing.clone());
    }
    let source = slug_source(&seat.title, &seat.session_id);
    let slug = allocate_slug(&source, &directory.taken_slugs());
    let record = AgentRecord {
        session_id: seat.session_id.clone(),
        mxid: puppet_mxid(&slug, server_name),
        slug,
        room_id: None,
        display_name: String::new(),
        active: true,
        departed: false,
        direct_set: false,
    };
    directory.upsert(record.clone());
    directory.save().await?;
    Ok(record)
}

async fn open_room(
    puppet: &MatrixClient,
    owner: &str,
    name: &str,
    record: &mut AgentRecord,
    directory: &mut AgentDirectory,
) -> Result<bool, String> {
    if record.room_id.is_some() {
        return Ok(false);
    }
    let room = puppet
        .create_direct_room(name, owner, owner)
        .await
        .map_err(|error| format!("create room for {}: {error}", record.mxid))?;
    record.room_id = Some(room);
    directory.upsert(record.clone());
    directory.save().await?;
    Ok(true)
}

async fn refresh_names(
    puppet: &MatrixClient,
    name: &str,
    created: bool,
    renamed: bool,
    record: &mut AgentRecord,
) -> Result<(), String> {
    if !renamed {
        return Ok(());
    }
    record.display_name = name.to_string();
    if created {
        return Ok(());
    }
    let Some(room) = &record.room_id else {
        return Ok(());
    };
    let named = set_room_name(puppet, room, name).await;
    if let Err(error) = named {
        return Err(format!("room name for {}: {error}", record.mxid));
    }
    Ok(())
}

async fn ensure_direct(
    puppet: &MatrixClient,
    owner: &str,
    record: &mut AgentRecord,
) -> Result<(), String> {
    if record.direct_set {
        return Ok(());
    }
    let Some(room) = &record.room_id else {
        return Ok(());
    };
    set_direct_room(puppet, owner, room)
        .await
        .map_err(|error| format!("m.direct for {}: {error}", record.mxid))?;
    record.direct_set = true;
    Ok(())
}

async fn rejoin(puppet: &MatrixClient, owner: &str, record: &AgentRecord) -> Result<(), String> {
    let Some(room) = &record.room_id else {
        return Ok(());
    };
    let members = puppet
        .joined_members(room)
        .await
        .map_err(|error| format!("members for {}: {error}", record.mxid))?;
    if !member_present(&members, puppet.user_id()) {
        puppet
            .join_room(room)
            .await
            .map_err(|error| format!("rejoin {}: {error}", record.mxid))?;
    }
    if member_present(&members, owner) {
        return Ok(());
    }
    if let Err(error) = invite_user(puppet, room, owner).await
        && !already_invited(&error)
    {
        tracing::warn!(%error, mxid = %record.mxid, "matrix: re-invite failed");
    }
    Ok(())
}

async fn deactivate_absent(
    client: &MatrixClient,
    directory: &mut AgentDirectory,
    present: &HashSet<String>,
    report: &mut ReconcileReport,
) {
    let stale: Vec<AgentRecord> = directory
        .records()
        .iter()
        .filter(|agent| !present.contains(&agent.session_id))
        .filter(|agent| agent.active || !agent.departed)
        .cloned()
        .collect();
    for mut record in stale {
        match depart(client, &record).await {
            Ok(()) => record.departed = true,
            Err(error) => report.errors.push(error),
        }
        record.active = false;
        directory.upsert(record.clone());
        if let Err(error) = directory.save().await {
            report.errors.push(error);
        }
        report.deactivated.push(record);
    }
}

async fn depart(client: &MatrixClient, record: &AgentRecord) -> Result<(), String> {
    let Some(room) = &record.room_id else {
        return Ok(());
    };
    let puppet = client.for_user(&record.mxid);
    match leave_room(&puppet, room).await {
        Ok(()) => Ok(()),
        Err(error) if already_left(&error) => Ok(()),
        Err(error) => Err(format!("leave {}: {error}", record.mxid)),
    }
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
