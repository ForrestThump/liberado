//! Per-room inbound queues. The listener pushes. Each room's channel drains its queue.

use std::collections::HashMap;

use liberado_messaging::InboundEvent;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Fan-out from one transaction listener to one queue per room.
#[derive(Default)]
pub struct RoomQueues {
    rooms: std::sync::Mutex<HashMap<String, UnboundedSender<InboundEvent>>>,
}

impl RoomQueues {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue for `room_id`. A second call replaces the previous sender.
    pub fn register(&self, room_id: &str) -> UnboundedReceiver<InboundEvent> {
        let (sender, receiver) = unbounded_channel();
        if let Ok(mut rooms) = self.rooms.lock() {
            rooms.insert(room_id.to_string(), sender);
        }
        receiver
    }

    /// Drop the event when no channel is listening on `room_id`.
    pub fn push(&self, room_id: &str, event: InboundEvent) {
        let Ok(rooms) = self.rooms.lock() else {
            return;
        };
        let Some(sender) = rooms.get(room_id) else {
            tracing::debug!(room = room_id, "matrix: no channel for room");
            return;
        };
        if sender.send(event).is_err() {
            tracing::debug!(room = room_id, "matrix: room channel closed");
        }
    }
}
