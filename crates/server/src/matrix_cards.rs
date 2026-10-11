//! Permission cards for Matrix. An agent session uses that agent's room.
//! Every other Matrix session uses the main room.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_executor::PermissionSink;
use liberado_matrix::MatrixChannel;
use liberado_messaging::{MessagingChannel, permission_action_rows};

/// Main-room fallback plus one channel per agent session.
pub struct MatrixCards {
    main: Arc<MatrixChannel>,
    agents: Mutex<HashMap<String, Arc<MatrixChannel>>>,
}

impl MatrixCards {
    pub fn new(main: Arc<MatrixChannel>) -> Self {
        Self {
            main,
            agents: Mutex::new(HashMap::new()),
        }
    }

    pub fn insert(&self, session_id: &str, channel: Arc<MatrixChannel>) {
        let mut agents = self
            .agents
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        agents.insert(session_id.to_string(), channel);
    }

    pub fn remove(&self, session_id: &str) {
        let mut agents = self
            .agents
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        agents.remove(session_id);
    }

    fn channel_for(&self, session_id: Option<&str>) -> Arc<MatrixChannel> {
        let Some(session_id) = session_id else {
            return Arc::clone(&self.main);
        };
        let agents = self
            .agents
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        agents
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| Arc::clone(&self.main))
    }
}

#[async_trait]
impl PermissionSink for MatrixCards {
    async fn notify_permission(&self, proposal_id: &str, message: &str) -> Result<(), String> {
        self.notify_permission_in(proposal_id, message, None).await
    }

    async fn notify_permission_in(
        &self,
        proposal_id: &str,
        message: &str,
        session_id: Option<&str>,
    ) -> Result<(), String> {
        self.channel_for(session_id)
            .send_with_actions(message, &permission_action_rows(proposal_id))
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use liberado_matrix::{MatrixChannel, MatrixClient, RoomQueues};

    use super::MatrixCards;

    #[test]
    fn an_agent_session_uses_its_room_and_everyone_else_uses_the_main_room() {
        let queues = RoomQueues::new();
        let client =
            MatrixClient::new("http://127.0.0.1:9", "token", "@liberado:s").expect("client");
        let main = Arc::new(MatrixChannel::open(client.clone(), "!main:s", &queues));
        let cards = MatrixCards::new(Arc::clone(&main));
        assert_eq!(cards.channel_for(None).room_id(), "!main:s");
        assert_eq!(cards.channel_for(Some("missing")).room_id(), "!main:s");
        let agent = Arc::new(MatrixChannel::open(
            client.for_user("@liberado_coder:s"),
            "!dm:s",
            &queues,
        ));
        cards.insert("sess", agent);
        assert_eq!(cards.channel_for(Some("sess")).room_id(), "!dm:s");
        cards.remove("sess");
        assert_eq!(cards.channel_for(Some("sess")).room_id(), "!main:s");
    }
}
