//! TOML loader for tool descriptors.
//!
//! A file whose tool lists no effects is refused. The process does not start
//! with that file. Unknown fields are refused, so a typo cannot widen a tool.

use serde::Deserialize;

use crate::descriptor::{Declaration, PolicyError, ToolPolicy, VaultScope};
use crate::effect::Effect;

/// Load a policy from TOML text.
///
/// A tool with an empty `effects` list refuses the file.
pub fn load_toml(text: &str) -> Result<ToolPolicy, PolicyError> {
    let file: ToolFile =
        toml::from_str(text).map_err(|error| PolicyError::Toml(error.to_string()))?;
    let decls = rows_to_declarations(file)?;
    refuse_empty_effects(&decls)?;
    ToolPolicy::from_declarations(decls)
}

fn refuse_empty_effects(decls: &[Declaration]) -> Result<(), PolicyError> {
    if let Some(decl) = decls.iter().find(|decl| decl.effects.is_empty()) {
        return Err(PolicyError::UndeclaredEffects {
            server: decl.server.clone(),
            name: decl.name.clone(),
        });
    }
    Ok(())
}

fn rows_to_declarations(file: ToolFile) -> Result<Vec<Declaration>, PolicyError> {
    let mut out = Vec::with_capacity(file.tools.len());
    for row in file.tools {
        out.push(row_to_declaration(row)?);
    }
    Ok(out)
}

fn row_to_declaration(row: ToolRow) -> Result<Declaration, PolicyError> {
    let vault = scope_from_row(&row)?;
    Ok(Declaration {
        server: row.server,
        name: row.name,
        effects: row.effects.into_iter().collect(),
        vault,
        urls: row.urls,
        external_services: row.external_services.into_iter().collect(),
    })
}

fn scope_from_row(row: &ToolRow) -> Result<VaultScope, PolicyError> {
    match row.vault {
        VaultKind::None => no_zone_fields(row).map(|()| VaultScope::None),
        VaultKind::VaultWide => no_zone_fields(row).map(|()| VaultScope::VaultWide),
        VaultKind::Zones => fixed_zones(row),
        VaultKind::FromArg => from_arg_scope(row),
    }
}

fn no_zone_fields(row: &ToolRow) -> Result<(), PolicyError> {
    if row.zones.is_empty() && row.from_arg.is_none() {
        Ok(())
    } else {
        Err(PolicyError::ScopeMismatch)
    }
}

fn fixed_zones(row: &ToolRow) -> Result<VaultScope, PolicyError> {
    if row.from_arg.is_some() || row.zones.is_empty() {
        return Err(PolicyError::ScopeMismatch);
    }
    Ok(VaultScope::zones(row.zones.clone()))
}

fn from_arg_scope(row: &ToolRow) -> Result<VaultScope, PolicyError> {
    let Some(arg) = row.from_arg.clone() else {
        return Err(PolicyError::ScopeMismatch);
    };
    Ok(VaultScope::Zones {
        zones: row.zones.clone(),
        from_arg: Some(arg),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolFile {
    tools: Vec<ToolRow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolRow {
    server: String,
    name: String,
    effects: Vec<Effect>,
    vault: VaultKind,
    #[serde(default)]
    zones: Vec<String>,
    #[serde(default)]
    from_arg: Option<String>,
    #[serde(default)]
    urls: Vec<String>,
    #[serde(default)]
    external_services: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum VaultKind {
    None,
    Zones,
    VaultWide,
    FromArg,
}

#[cfg(test)]
#[path = "load_tests.rs"]
mod load_tests;
