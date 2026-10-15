//! Tool policy model.
//!
//! Each tool declares what it touches and which effects it performs. A call
//! with no declaration is denied. A declaration with an empty effect set is
//! denied. The runtime does not call this crate yet. A later change connects
//! [`ToolPolicy::authorize`] at the tool boundary.
//!
//! The vocabulary follows the capability model in
//! `docs/decisions/ADR-0004-capability-zone-model.md` and
//! `docs/future-work/ideas/liberado-permissions-idea.md`: explicit grants, no
//! ambient authority, and a fail-closed answer when the zone or the effect is
//! unknown. Path hardening matches the vault rule already used for writes:
//! a relative `zone/rest` path, exact zone case, and a refusal for `..`,
//! absolute paths, drive letters, and control characters.
//!
//! This crate does not depend on `liberado-common` or `liberado-executor`.
//! Those files are free to move while this model lands.

mod compat;
mod decide;
mod descriptor;
mod effect;
mod load;
mod path;
mod segment;
mod url;

pub use compat::{CompatError, LegacyMcp, LegacyToolZone, policy_from_legacy};
pub use decide::{Allowed, Deny, ToolCall};
pub use descriptor::{Declaration, PolicyError, ToolDescriptor, ToolPolicy, VaultScope};
pub use effect::Effect;
pub use load::load_toml;
pub use path::{PathError, VaultPath, normalize_vault_path};
pub use url::{ToolUrl, UrlError, normalize_url};
