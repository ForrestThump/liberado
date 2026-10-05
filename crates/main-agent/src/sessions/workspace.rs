//! Per-agent private file workspace, attached outside the MCP risk gate.
//!
//! The session id is the agent id. The model cannot pass another agent's id.
//! When the directory cannot be opened, the tools are withheld. There is no
//! shared directory to fall back to.

use tracing::warn;

use liberado_agent_workspace::{AgentWorkspace, WorkspaceRuntime, WorkspaceSettings};
use liberado_executor::ToolRuntime;

use super::ChatSessions;

impl ChatSessions {
    /// Remember where private agent directories live. Production calls this once at boot.
    pub fn with_agent_workspace(mut self, settings: WorkspaceSettings) -> Self {
        self.agent_workspace = Some(settings);
        self
    }

    /// Put this session's file tools in front of `inner`.
    ///
    /// Absent settings leave `inner` unchanged, so unit tests do not create directories.
    /// An open failure withholds the tools and does not substitute a shared directory.
    pub(super) fn attach_workspace(
        &self,
        session: impl std::fmt::Display,
        inner: Box<dyn ToolRuntime>,
    ) -> Box<dyn ToolRuntime> {
        let Some(settings) = &self.agent_workspace else {
            return inner;
        };
        let session = session.to_string();
        match AgentWorkspace::open(&settings.root, &session, settings.max_bytes) {
            Ok(workspace) => Box::new(WorkspaceRuntime::new(workspace, inner)),
            Err(err) => {
                warn!(
                    session,
                    error = %err,
                    "agent workspace unavailable; file tools withheld"
                );
                inner
            }
        }
    }
}
