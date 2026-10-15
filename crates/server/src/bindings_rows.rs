//! Inactive agent rows live beside the routing map.
//!
//! Routing reads `bindings` only. `reserved` keeps a deleted agent's slug and
//! room without a second session id being minted for that room.

use std::future::Future;

use liberado_common::ChannelKind;
use liberado_conversation_store::Ulid;

use super::{BindingKey, BindingState, ChannelBindings, StoredBinding};

pub(super) fn active_by_default() -> bool {
    true
}

impl ChannelBindings {
    /// Whether this peer is stored, and whether it still routes.
    ///
    /// `None` means there is no row. `Some(false)` is an archived agent DM.
    /// Tests read this. Routing uses [`ChannelBindings::get`], which only sees active rows.
    #[allow(dead_code)]
    pub async fn is_active(&self, key: &BindingKey) -> Option<bool> {
        let state = self.inner.lock().await;
        if state.bindings.iter().any(|binding| binding.matches(key)) {
            return Some(true);
        }
        state
            .reserved
            .iter()
            .any(|binding| binding.matches(key))
            .then_some(false)
    }

    /// Mark a stored peer active or inactive. A missing key is left alone.
    pub async fn set_active(&self, key: &BindingKey, active: bool) {
        let snapshot = {
            let mut state = self.inner.lock().await;
            if !state.set_flag(key, active) {
                return;
            }
            let snapshot = state.snapshot();
            self.persist(&state).await;
            snapshot
        };
        self.publish(&snapshot);
    }
}

impl BindingState {
    pub(super) fn all_rows(&self) -> Vec<StoredBinding> {
        let mut rows = self.bindings.clone();
        rows.extend(self.reserved.iter().cloned());
        rows
    }

    pub(super) fn stored_session(&self, key: &BindingKey) -> Option<Ulid> {
        self.bindings
            .iter()
            .chain(self.reserved.iter())
            .find(|binding| binding.matches(key))
            .and_then(StoredBinding::session)
    }

    pub(super) fn key_for(&self, session: Ulid) -> Option<BindingKey> {
        let label = session.to_string();
        self.bindings
            .iter()
            .chain(self.reserved.iter())
            .find(|binding| binding.session_id == label)
            .map(StoredBinding::key)
    }

    /// Returns whether the map changed. An inactive row stays inactive.
    pub(super) fn assign(&mut self, key: &BindingKey, id: Option<Ulid>) -> bool {
        match id {
            Some(id) => self.store_session(key, id),
            None => self.clear_key(key),
        }
    }

    fn set_flag(&mut self, key: &BindingKey, active: bool) -> bool {
        if active {
            self.activate_key(key)
        } else {
            self.deactivate_key(key)
        }
    }

    fn activate_key(&mut self, key: &BindingKey) -> bool {
        let Some(index) = self
            .reserved
            .iter()
            .position(|binding| binding.matches(key))
        else {
            return false;
        };
        let mut row = self.reserved.remove(index);
        row.active = true;
        self.bindings.push(row);
        true
    }

    fn deactivate_key(&mut self, key: &BindingKey) -> bool {
        let Some(index) = self
            .bindings
            .iter()
            .position(|binding| binding.matches(key))
        else {
            return false;
        };
        let mut row = self.bindings.remove(index);
        row.active = false;
        self.reserved.push(row);
        true
    }

    fn store_session(&mut self, key: &BindingKey, id: Ulid) -> bool {
        let label = id.to_string();
        match write_session(&mut self.bindings, key, &label) {
            Slot::Updated => true,
            Slot::Same => false,
            Slot::Missing => self.store_reserved_or_insert(key, &label),
        }
    }

    fn store_reserved_or_insert(&mut self, key: &BindingKey, label: &str) -> bool {
        match write_session(&mut self.reserved, key, label) {
            Slot::Missing => {
                self.bindings.push(new_row(key, label));
                true
            }
            Slot::Same => false,
            Slot::Updated => true,
        }
    }

    fn clear_key(&mut self, key: &BindingKey) -> bool {
        let before = self.bindings.len() + self.reserved.len();
        self.bindings.retain(|binding| !binding.matches(key));
        self.reserved.retain(|binding| !binding.matches(key));
        before != self.bindings.len() + self.reserved.len()
    }
}

enum Slot {
    Missing,
    Same,
    Updated,
}

fn write_session(rows: &mut [StoredBinding], key: &BindingKey, label: &str) -> Slot {
    let Some(row) = rows.iter_mut().find(|binding| binding.matches(key)) else {
        return Slot::Missing;
    };
    if row.session_id == label {
        return Slot::Same;
    }
    row.session_id = label.to_string();
    Slot::Updated
}

fn new_row(key: &BindingKey, label: &str) -> StoredBinding {
    StoredBinding {
        channel: key.channel,
        peer: key.peer.clone(),
        bot: key.bot.clone(),
        session_id: label.to_string(),
        active: true,
    }
}

pub(super) struct Retained {
    pub(super) bindings: Vec<StoredBinding>,
    pub(super) reserved: Vec<StoredBinding>,
    pub(super) changed: bool,
}

pub(super) async fn retain_valid<F, Fut>(bindings: Vec<StoredBinding>, is_valid: &F) -> Retained
where
    F: Fn(Ulid) -> Fut,
    Fut: Future<Output = bool>,
{
    let mut retained = Retained {
        bindings: Vec::new(),
        reserved: Vec::new(),
        changed: false,
    };
    for binding in bindings {
        retained.keep(binding, is_valid).await;
    }
    retained
}

impl Retained {
    async fn keep<F, Fut>(&mut self, binding: StoredBinding, is_valid: &F)
    where
        F: Fn(Ulid) -> Fut,
        Fut: Future<Output = bool>,
    {
        if !binding.active {
            self.reserved.push(binding);
            return;
        }
        if session_still_valid(&binding, is_valid).await {
            self.bindings.push(binding);
            return;
        }
        if reserves_puppet(&binding) {
            self.park(binding);
            return;
        }
        self.changed = true;
        tracing::info!(
            session = %binding.session_id,
            channel = binding.channel.as_str(),
            "dropping channel binding whose session is gone"
        );
    }

    fn park(&mut self, mut binding: StoredBinding) {
        tracing::info!(
            session = %binding.session_id,
            bot = %binding.bot,
            "matrix: agent binding kept inactive; the session is gone"
        );
        binding.active = false;
        self.reserved.push(binding);
        self.changed = true;
    }
}

async fn session_still_valid<F, Fut>(binding: &StoredBinding, is_valid: &F) -> bool
where
    F: Fn(Ulid) -> Fut,
    Fut: Future<Output = bool>,
{
    match binding.session() {
        Some(id) => is_valid(id).await,
        None => false,
    }
}

fn reserves_puppet(binding: &StoredBinding) -> bool {
    if binding.channel != ChannelKind::Matrix {
        return false;
    }
    let Some((_, host)) = binding.bot.rsplit_once(':') else {
        return false;
    };
    liberado_matrix::slug_of_mxid(&binding.bot, host).is_some()
}
