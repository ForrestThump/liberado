//! The typed config model (Decision 14), split by section for maintainability.
//!
//! - [`config`]: top-level [`Config`]
//! - [`topology`]: vault, providers, pools, profiles, schedules, hooks, MCPs
//! - [`policy`]: zones and grants
//! - [`tuning`]: dispatch/context/concurrency and surface tunables
//! - [`builder`]: [`ConfigBuilder`] for tests

mod builder;
mod config;
mod policy;
mod topology;
mod tuning;

pub use builder::ConfigBuilder;
pub use config::{CodingAuthError, CodingWorkspaceAuth, Config, GrantParts, ResolvedProfile};
pub use policy::{Grant, Policy, ZonePolicy};
pub use topology::{
    AcpConfig, COMPACTION_TRIGGER_PCT_DEFAULT, COMPACTION_TRIGGER_TOKENS_FALLBACK,
    CompactionSettings, CompactionTriggerSource, CronSchedule, EnterKey, HookConfig,
    MainAgentConfig, McpConfig, McpGrant, McpTransport, ModelCompactionSettings, PoolConfig,
    PreflightProfileConfig, PreflightStepConfig, ProjectConfig, ProjectPreflightConfig,
    ProviderProfile, ReportSinkConfig, RoleOverride, SessionProfile, ShepherdConfig,
    ShepherdProjectConfig, ToolImpact, Topology, WebUiConfig, managed_binary_path,
    resolve_declared_zone,
};
pub use tuning::{
    CURRENT_SCHEMA_VERSION, CaptureTuning, ConcurrencyTuning, ContextTuning, CronDeliveryTuning,
    DispatchTuning, MaintenanceTuning, McpPoolingTuning, SubagentIsolation,
    TelegramApprovalsTuning, Tuning,
};
