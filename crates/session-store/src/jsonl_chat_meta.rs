//! Chat project and pending-model updates.
//!
//! Split from `jsonl.rs` so the header rewrite does not add another copy of the
//! lock-and-append body to that file's function count.

use liberado_conversation_store::{ChatMetaUpdate, StoreError, StoreResult, Ulid};

use super::{Record, SessionStore};

pub(super) async fn apply(
    store: &SessionStore,
    conversation: Ulid,
    update: ChatMetaUpdate,
) -> StoreResult<()> {
    if update.project.is_none() && update.pending_model.is_none() {
        return Ok(());
    }
    let mut map = store.inner.lock().await;
    let live = map
        .get_mut(&conversation)
        .ok_or_else(|| StoreError::NotFound(format!("session {conversation}")))?;
    if let Some(project) = update.project {
        live.header.project = project;
    }
    if let Some(model) = update.pending_model {
        live.header.pending_model = model;
    }
    let header = live.header.clone();
    drop(map);
    store.append_line(conversation, &Record::Header(Box::new(header)));
    Ok(())
}
