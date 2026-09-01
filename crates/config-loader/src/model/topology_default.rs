//! `Default` for [`super::Topology`]. Split so new turn-ceiling fields can live on the
//! struct without growing the parent file past its ploc baseline.

use std::collections::HashMap;
use std::path::PathBuf;

use super::topology::default_providers;
use super::{AcpConfig, MainAgentConfig, ShepherdConfig, Topology, WebUiConfig};
use liberado_common::DEFAULT_TIMEZONE;

impl Default for Topology {
    fn default() -> Self {
        Self {
            direct_max_turns: None,
            subagent_max_turns: None,
            research_max_turns: None,
            report_sink: None,
            vault_path: PathBuf::new(),
            timezone: DEFAULT_TIMEZONE.to_string(),
            daemon_socket: PathBuf::from("/run/liberado/daemon.sock"),
            provider: "deepseek".to_string(),
            providers: default_providers(),
            acp: AcpConfig::default(),
            models: Vec::new(),
            model_roles: HashMap::new(),
            mcps: Vec::new(),
            hooks: Vec::new(),
            schedules: Vec::new(),
            pools: Vec::new(),
            session_profiles: Vec::new(),
            projects: Vec::new(),
            shepherd: ShepherdConfig::default(),
            main_agent: MainAgentConfig::default(),
            webui: WebUiConfig::default(),
            roles: HashMap::new(),
        }
    }
}
