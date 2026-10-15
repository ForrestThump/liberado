//! One Matrix room as a [`liberado_messaging::MessagingChannel`].

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_markdown::markdown_to_html;
use liberado_messaging::{
    ActionButton, InboundEvent, MessagingChannel, MessagingError, ResolvedElsewhere,
};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::cards::{CardBook, action_suffix};
use crate::client::MatrixClient;
use crate::commands::command_line;
use crate::hub::RoomQueues;
use crate::messages::{CHUNK_BYTES, MsgKind, byte_chunks, edit_content, message_content};

/// Transport for one room, acting as one user.
pub struct MatrixChannel {
    client: MatrixClient,
    room_id: String,
    incoming: tokio::sync::Mutex<UnboundedReceiver<InboundEvent>>,
    cards: Arc<CardBook>,
    pending_prompt: std::sync::Mutex<Option<String>>,
}

impl MatrixChannel {
    /// Register `room_id` on `queues` and return the channel that drains it.
    pub fn open(client: MatrixClient, room_id: impl Into<String>, queues: &RoomQueues) -> Self {
        let room_id = room_id.into();
        let incoming = queues.register(&room_id);
        Self {
            client,
            room_id,
            incoming: tokio::sync::Mutex::new(incoming),
            cards: Arc::new(CardBook::new()),
            pending_prompt: Mutex::new(None),
        }
    }

    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    pub fn user_id(&self) -> &str {
        self.client.user_id()
    }

    /// Edit the card for `proposal_id` to `receipt`, when this process sent one.
    pub async fn mark_resolved(&self, proposal_id: &str, receipt: &str) {
        let Some(event_id) = self.cards.take(proposal_id) else {
            return;
        };
        if let Err(error) = self.edit_text(&event_id, receipt).await {
            tracing::warn!(%error, proposal_id, "matrix card update failed");
        }
    }

    fn classify(&self, text: String) -> InboundEvent {
        if let Some(action) = self.cards.take_keyword(&text) {
            return action;
        }
        if let Some(slash) = command_line(&text) {
            return chat_message(slash, None);
        }
        let prompt = self
            .pending_prompt
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        chat_message(text, prompt)
    }

    async fn send_plain(&self, text: &str) -> Result<(), MessagingError> {
        for chunk in byte_chunks(text, CHUNK_BYTES) {
            self.send_chunk(chunk).await?;
        }
        Ok(())
    }

    async fn send_chunk(&self, text: &str) -> Result<String, MessagingError> {
        let html = markdown_to_html(text);
        let content = message_content(MsgKind::Text, text, &html);
        self.client.send_message(&self.room_id, &content).await
    }

    async fn edit_text(&self, event_id: &str, text: &str) -> Result<(), MessagingError> {
        let html = markdown_to_html(text);
        let content = edit_content(event_id, text, &html);
        self.client.send_message(&self.room_id, &content).await?;
        Ok(())
    }
}

fn chat_message(text: String, reply_to_prompt: Option<String>) -> InboundEvent {
    InboundEvent::Message {
        text,
        reply_to_prompt,
        from_bot: false,
    }
}

/// Edits a Matrix card when another surface records the decision.
pub struct MatrixCardUpdater {
    channel: Arc<MatrixChannel>,
}

impl MatrixCardUpdater {
    pub fn new(channel: Arc<MatrixChannel>) -> Self {
        Self { channel }
    }
}

#[async_trait]
impl ResolvedElsewhere for MatrixCardUpdater {
    fn surface_id(&self) -> &'static str {
        "matrix"
    }

    async fn on_resolved_elsewhere(&self, proposal_id: &str, receipt: &str) {
        self.channel.mark_resolved(proposal_id, receipt).await;
    }
}

#[async_trait]
impl MessagingChannel for MatrixChannel {
    fn name(&self) -> &str {
        "Matrix"
    }

    fn defer_help_to_chat(&self) -> bool {
        true
    }

    async fn send_text(&self, text: &str) -> Result<(), MessagingError> {
        self.send_plain(text).await
    }

    async fn send_with_actions(
        &self,
        text: &str,
        rows: &[Vec<ActionButton>],
    ) -> Result<(), MessagingError> {
        let suffix = action_suffix(rows);
        let budget = CHUNK_BYTES.saturating_sub(suffix.len()).max(1);
        let chunks = byte_chunks(text, budget);
        let mut first = None;
        for (index, chunk) in chunks.iter().enumerate() {
            let body = if index == 0 {
                format!("{chunk}{suffix}")
            } else {
                (*chunk).to_string()
            };
            let event_id = self.send_chunk(&body).await?;
            if index == 0 {
                first = Some(event_id);
            }
        }
        if let Some(event_id) = first {
            self.cards.remember(rows, event_id);
        }
        Ok(())
    }

    async fn request_reply(&self, prompt: &str) -> Result<String, MessagingError> {
        self.send_plain(prompt).await?;
        let id = ulid::Ulid::new().to_string();
        if let Ok(mut slot) = self.pending_prompt.lock() {
            *slot = Some(id.clone());
        }
        Ok(id)
    }

    async fn acknowledge(&self, _event_id: &str, _text: &str) -> Result<(), MessagingError> {
        Ok(())
    }

    async fn edit_message(&self, message_ref: &str, text: &str) -> Result<(), MessagingError> {
        self.edit_text(message_ref, text).await
    }

    async fn set_typing(&self) -> Result<(), MessagingError> {
        self.client.set_typing(&self.room_id, true).await
    }

    async fn receive(&self, cursor: &mut String) -> Result<Vec<InboundEvent>, MessagingError> {
        cursor.clear();
        cursor.push('0');
        let event = {
            let mut incoming = self.incoming.lock().await;
            incoming.recv().await
        };
        match event {
            Some(InboundEvent::Message { text, .. }) => Ok(vec![self.classify(text)]),
            Some(other) => Ok(vec![other]),
            None => Err(MessagingError::new("matrix room queue closed")),
        }
    }
}

#[cfg(test)]
#[path = "channel_tests.rs"]
mod tests;
