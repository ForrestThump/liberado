//! Start helpers split from `hub.rs` for its function and module-health boundaries.

use std::collections::HashMap;

use tokio::sync::mpsc;
use tracing::debug;

use crate::goal::GoalSpec;
use crate::hub::GoalSessionHub;
use crate::runner::HumanInput;

impl GoalSessionHub {
    pub(super) async fn existing_session_id(&self, goal: &GoalSpec) -> Option<String> {
        let id = goal.id.as_deref()?;
        self.store().get(id).await.map(|_| id.to_string())
    }

    pub(super) async fn register_input(
        inputs: &tokio::sync::Mutex<HashMap<String, mpsc::Sender<HumanInput>>>,
        session_id: &str,
        interactive: bool,
        input_tx: mpsc::Sender<HumanInput>,
    ) {
        if interactive {
            inputs.lock().await.insert(session_id.to_string(), input_tx);
        } else {
            drop(input_tx);
            debug!(
                session = %session_id,
                "session grant omits AskHuman — running non-interactively (input channel closed)"
            );
        }
    }
}
