//! Build tool declarations from today's MCP zone config.
//!
//! The fields mirror `zone_from_arg`, `write_tools`, `read_tools`,
//! `vault_wide_writes`, and `admin_tools`. This crate does not import the
//! config types. A later wiring change can copy those fields in.
//!
//! A name in none of the lists is omitted. [`crate::ToolPolicy::authorize`]
//! denies that name. A name in two lists is refused here, so the shim does
//! not pick a winner.

use std::collections::{BTreeMap, BTreeSet};

use crate::descriptor::{Declaration, PolicyError, ToolPolicy, VaultScope};
use crate::effect::Effect;

/// One tool's fixed zone. `zone = None` means the tool is a read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyToolZone {
    /// Bare tool name.
    pub name: String,
    /// Fixed zone. `None` means this tool is not a zone write.
    pub zone: Option<String>,
}

/// The zone fields a current MCP declaration carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyMcp {
    /// MCP name. Becomes the descriptor server.
    pub server: String,
    /// Path argument whose leading segment is the zone.
    pub zone_from_arg: Option<String>,
    /// Tools that write. A delete-shaped name becomes [`Effect::Delete`].
    pub write_tools: Vec<String>,
    /// Tools that read.
    pub read_tools: Vec<String>,
    /// Tools that mutate vault-wide state.
    pub vault_wide_writes: Vec<String>,
    /// Tools reserved for the operator.
    pub admin_tools: Vec<String>,
    /// Zone used when `zone_from_arg` is absent.
    pub default_zone: Option<String>,
    /// Per-tool zone overrides for a fixed-zone MCP.
    pub tool_zones: Vec<LegacyToolZone>,
    /// `Some(false)` says the MCP writes, and the writes are not vault zones.
    pub writes_vault: Option<bool>,
}

/// Why the legacy fields could not become declarations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompatError {
    /// The same tool name appears in two lists.
    #[error("tool {name} is listed in more than one classification")]
    Overlap { name: String },
    /// Path-addressed fields and fixed-zone fields are both set.
    #[error("path-addressed and fixed-zone declarations are both set")]
    MixedStyles,
    /// A write has no zone and was not opted out of the vault.
    #[error("write tool {name} has no zone")]
    UnplacedWrite { name: String },
    /// The server name is empty.
    #[error("server name is empty")]
    EmptyServer,
    /// A declaration failed the policy checks.
    #[error(transparent)]
    Policy(#[from] PolicyError),
}

/// Compile a policy from one legacy MCP declaration.
pub fn policy_from_legacy(mcp: &LegacyMcp) -> Result<ToolPolicy, CompatError> {
    let decls = declarations_from_legacy(mcp)?;
    ToolPolicy::from_declarations(decls).map_err(CompatError::from)
}

fn declarations_from_legacy(mcp: &LegacyMcp) -> Result<Vec<Declaration>, CompatError> {
    if mcp.server.is_empty() {
        return Err(CompatError::EmptyServer);
    }
    if let Some(arg) = mcp.zone_from_arg.as_deref() {
        return from_path_addressed(mcp, arg);
    }
    from_fixed(mcp)
}

fn from_path_addressed(mcp: &LegacyMcp, arg: &str) -> Result<Vec<Declaration>, CompatError> {
    if mixed_with_path_addressing(mcp) {
        return Err(CompatError::MixedStyles);
    }
    refuse_overlap(&[
        &mcp.write_tools,
        &mcp.read_tools,
        &mcp.vault_wide_writes,
        &mcp.admin_tools,
    ])?;
    let mut out = Vec::new();
    push_writers(&mut out, mcp, &mcp.write_tools, VaultScope::from_arg(arg))?;
    push_named(
        &mut out,
        mcp,
        &mcp.read_tools,
        Effect::Read,
        VaultScope::from_arg(arg),
    )?;
    push_named(
        &mut out,
        mcp,
        &mcp.vault_wide_writes,
        Effect::Write,
        VaultScope::VaultWide,
    )?;
    push_named(
        &mut out,
        mcp,
        &mcp.admin_tools,
        Effect::Admin,
        VaultScope::None,
    )?;
    Ok(out)
}

fn mixed_with_path_addressing(mcp: &LegacyMcp) -> bool {
    !mcp.tool_zones.is_empty() || mcp.writes_vault == Some(false) || mcp.default_zone.is_some()
}

fn from_fixed(mcp: &LegacyMcp) -> Result<Vec<Declaration>, CompatError> {
    if writes_vault_disagrees(mcp) {
        return Err(CompatError::MixedStyles);
    }
    let zone_names = tool_zone_names(mcp);
    refuse_overlap(&[
        &mcp.write_tools,
        &mcp.read_tools,
        &mcp.vault_wide_writes,
        &mcp.admin_tools,
        &zone_names,
    ])?;
    let mut out = Vec::new();
    push_fixed_writes(&mut out, mcp)?;
    push_tool_zones(&mut out, mcp)?;
    push_named(
        &mut out,
        mcp,
        &mcp.read_tools,
        Effect::Read,
        VaultScope::None,
    )?;
    push_named(
        &mut out,
        mcp,
        &mcp.vault_wide_writes,
        Effect::Write,
        VaultScope::VaultWide,
    )?;
    push_named(
        &mut out,
        mcp,
        &mcp.admin_tools,
        Effect::Admin,
        VaultScope::None,
    )?;
    Ok(out)
}

fn writes_vault_disagrees(mcp: &LegacyMcp) -> bool {
    mcp.writes_vault == Some(false) && has_zone_source(mcp)
}

fn has_zone_source(mcp: &LegacyMcp) -> bool {
    mcp.default_zone.is_some()
        || !mcp.vault_wide_writes.is_empty()
        || mcp.tool_zones.iter().any(|tool| tool.zone.is_some())
}

fn tool_zone_names(mcp: &LegacyMcp) -> Vec<String> {
    mcp.tool_zones
        .iter()
        .map(|tool| tool.name.clone())
        .collect()
}

fn refuse_overlap(lists: &[&[String]]) -> Result<(), CompatError> {
    let mut seen = BTreeMap::new();
    for list in lists {
        for name in *list {
            if seen.insert(name.clone(), ()).is_some() {
                return Err(CompatError::Overlap { name: name.clone() });
            }
        }
    }
    Ok(())
}

fn push_writers(
    out: &mut Vec<Declaration>,
    mcp: &LegacyMcp,
    names: &[String],
    vault: VaultScope,
) -> Result<(), CompatError> {
    for name in names {
        out.push(declaration(mcp, name, writer_effects(name), vault.clone())?);
    }
    Ok(())
}

fn push_fixed_writes(out: &mut Vec<Declaration>, mcp: &LegacyMcp) -> Result<(), CompatError> {
    for name in &mcp.write_tools {
        let vault = fixed_write_scope(mcp, name)?;
        out.push(declaration(mcp, name, writer_effects(name), vault)?);
    }
    Ok(())
}

fn fixed_write_scope(mcp: &LegacyMcp, name: &str) -> Result<VaultScope, CompatError> {
    if let Some(zone) = &mcp.default_zone {
        return Ok(VaultScope::zones(vec![zone.clone()]));
    }
    if mcp.writes_vault == Some(false) {
        return Ok(VaultScope::None);
    }
    Err(CompatError::UnplacedWrite {
        name: name.to_owned(),
    })
}

fn push_tool_zones(out: &mut Vec<Declaration>, mcp: &LegacyMcp) -> Result<(), CompatError> {
    for tool in &mcp.tool_zones {
        let (effects, vault) = tool_zone_effects(tool);
        out.push(declaration(mcp, &tool.name, effects, vault)?);
    }
    Ok(())
}

fn tool_zone_effects(tool: &LegacyToolZone) -> (BTreeSet<Effect>, VaultScope) {
    match &tool.zone {
        Some(zone) => (
            writer_effects(&tool.name),
            VaultScope::zones(vec![zone.clone()]),
        ),
        None => (one(Effect::Read), VaultScope::None),
    }
}

fn push_named(
    out: &mut Vec<Declaration>,
    mcp: &LegacyMcp,
    names: &[String],
    effect: Effect,
    vault: VaultScope,
) -> Result<(), CompatError> {
    for name in names {
        out.push(declaration(mcp, name, one(effect), vault.clone())?);
    }
    Ok(())
}

fn declaration(
    mcp: &LegacyMcp,
    name: &str,
    effects: BTreeSet<Effect>,
    vault: VaultScope,
) -> Result<Declaration, CompatError> {
    if name.is_empty() {
        return Err(CompatError::Policy(PolicyError::EmptyName));
    }
    Ok(Declaration {
        server: mcp.server.clone(),
        name: name.to_owned(),
        effects,
        vault,
        urls: Vec::new(),
        external_services: BTreeSet::new(),
    })
}

fn writer_effects(name: &str) -> BTreeSet<Effect> {
    if is_delete_name(name) {
        one(Effect::Delete)
    } else {
        one(Effect::Write)
    }
}

fn is_delete_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "delete"
        || lower.starts_with("delete_")
        || lower.ends_with("_delete")
        || lower.contains("_delete_")
}

fn one(effect: Effect) -> BTreeSet<Effect> {
    BTreeSet::from([effect])
}

#[cfg(test)]
#[path = "compat_tests.rs"]
mod compat_tests;
