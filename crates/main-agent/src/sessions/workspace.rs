//! Per-agent private file workspace, attached outside the MCP risk gate.
//!
//! The session id is the agent id. The model cannot pass another agent's id.
//! When the directory cannot be opened, the tools are withheld. There is no
//! shared directory to fall back to.
//!
//! A profile with `workspace = false` skips the attach. A later call then
//! reaches the risk gate, which refuses the name: the tools are not a
//! capability on the grant.

use std::collections::HashMap;

use tracing::warn;

use liberado_agent_workspace::{AgentWorkspace, WorkspaceRuntime, WorkspaceSettings};
use liberado_executor::ToolRuntime;
use liberado_session::SessionGrant;

use super::ChatSessions;

impl ChatSessions {
    /// Remember where private agent directories live. Production calls this once at boot.
    pub fn with_agent_workspace(mut self, settings: WorkspaceSettings) -> Self {
        self.agent_workspace = Some(settings);
        self
    }

    /// Replace the profile → file-tools table. The host installs this at boot
    /// from the loaded `[[session_profiles]]` rows. An empty table leaves the
    /// stored grant in charge.
    pub fn with_profile_workspaces(self, flags: HashMap<String, bool>) -> Self {
        *self
            .profile_workspaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = flags;
        self
    }

    /// Whether this turn offers the private file tools.
    ///
    /// A named profile uses the live table when the host installed a row, so a
    /// restarted daemon applies `workspace = false` to a chat that already
    /// stored the old grant. Otherwise the stored grant wins. No profile stays
    /// on.
    pub(super) fn workspace_for_grant(&self, grant: &SessionGrant) -> bool {
        let Some(name) = grant.profile.as_deref() else {
            return true;
        };
        self.profile_workspaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
            .copied()
            .unwrap_or(grant.workspace)
    }

    /// Put this session's file tools in front of `inner`.
    ///
    /// `enabled` false returns `inner` and does not open a directory. Absent
    /// settings also leave `inner` unchanged, so unit tests do not create
    /// directories. An open failure withholds the tools and does not substitute
    /// a shared directory.
    pub(super) fn attach_workspace(
        &self,
        session: impl std::fmt::Display,
        inner: Box<dyn ToolRuntime>,
        enabled: bool,
    ) -> Box<dyn ToolRuntime> {
        if !enabled {
            return inner;
        }
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
