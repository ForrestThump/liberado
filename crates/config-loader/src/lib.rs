//! # liberado-config-loader
//!
//! A config-loading foundation: [`ConfigSource`] trait and [`ChainLoader`] that merges multiple
//! sources in precedence order (see `docs/specs/liberado-config-spec.md`), plus the typed
//! [`model`] the whole system configures itself with.
//!
//! The trait is generic — any source of TOML content (file, env var, compiled-in default)
//! implements it. [`ChainLoader`] loads each source in sequence and merges the results at
//! the TOML table/key level (later sources override earlier ones). The merged value
//! deserializes into the target config model.
//!
//! The typed [`Config`] model lives here rather than in `liberado-config` (the more
//! natural-sounding name) because this crate's own [`validate_merged_config`] needs it, and
//! `liberado-config` already depends on this crate — putting the model in `liberado-config`
//! instead would create a cycle. `liberado-config` re-exports everything in [`model`], so external
//! consumers still reach it as `liberado_config::Config` et al. (moved from `liberado-common`
//! 2026-07-04, `docs/roadmap/hygiene-audit-2026-07-04.md`).
//!
//! ## Usage
//!
//! ```rust
//! use liberado_config_loader::{ConfigSource, ChainLoader, FileSource};
//! use std::path::PathBuf;
//!
//! let loader = ChainLoader::new()
//!     .add_source(Box::new(FileSource::new(PathBuf::from("/etc/liberado/topology.toml"))))
//!     .add_source(Box::new(FileSource::new(PathBuf::from("./config/topology.toml"))));
//! ```

mod chain;
mod file_source;
pub mod model;
mod source;
mod validation;

pub use chain::ChainLoader;
pub use file_source::FileSource;
pub use liberado_common::DEFAULT_POOL;
pub use model::{
    AcpConfig, COMPACTION_TRIGGER_PCT_DEFAULT, COMPACTION_TRIGGER_TOKENS_FALLBACK,
    CURRENT_SCHEMA_VERSION, CaptureTuning, CodingAuthError, CodingWorkspaceAuth,
    CompactionSettings, CompactionTriggerSource, ConcurrencyTuning, Config, ConfigBuilder,
    ContextTuning, CronDeliveryTuning, CronSchedule, DispatchTuning, EnterKey, Grant, HookConfig,
    MainAgentConfig, MaintenanceTuning, McpConfig, McpPoolingTuning, McpTransport,
    ModelCompactionSettings, Policy, PoolConfig, ProjectConfig, ProviderProfile, RoleOverride,
    SubagentIsolation, TelegramApprovalsTuning, ToolImpact, Topology, Tuning, WebUiConfig,
    ZonePolicy, managed_binary_path, resolve_declared_zone,
};
pub use source::{ConfigLoadError, ConfigSource};
pub use validation::validate_merged_config;
