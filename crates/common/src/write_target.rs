//! What a tool call writes, resolved from the MCP declaration and the call arguments.
//!
//! Path-addressed MCPs (TurboVault) classify every tool. A tool in none of the lists
//! is [`WriteTarget::Undeterminable`] and the runtime refuses it. Path arguments are
//! hardened here: absolute paths, drive letters, and any `..` segment are refused,
//! and the zone name is the leading segment compared case-sensitively by the grant.

use crate::WriteClass;
use crate::catalog::McpDescriptor;

/// Sentinel zone on a vault-wide permission request's `requested_grant`.
///
/// The grant applier writes it verbatim. [`crate::VaultWideGrant::holds_vault_wide_grant`]
/// is the matching predicate. The literal appears in the proposal note, so an operator
/// reading it sees what was approved. The `vault-wide-` prefix keeps a real vault folder
/// from matching by accident.
pub const VAULT_WIDE_ZONE_MARKER: &str = "vault-wide-any";

/// What a tool call writes to.
///
/// "This is a write whose zone I cannot determine" must not collapse into "not a write".
/// That collapse is the class of bug where a guard says nothing when it does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteTarget {
    /// Confirmed read, or an MCP that writes nothing.
    NotAWrite,
    /// Writes to this one zone. The caller must hold `Write(Vault(zone))`.
    Zone(String),
    /// Writes to every named zone. Multi-path tools (`move_note(from, to)`) return this.
    /// The runtime requires `Write` for each zone. Order follows the argument order.
    Zones(Vec<String>),
    /// Vault-wide mutation with no single zone (fanout, index rebuild).
    ///
    /// Authorised by `Write(Vault(VAULT_WIDE_ZONE_MARKER))`, not by an arbitrary zone write.
    /// `Write(Vault("Tasks"))` rearranges Tasks. It does not rebuild the index.
    VaultWide,
    /// A write, or an unknown tool, whose target could not be classified. Fail closed.
    Undeterminable(String),
}

/// Resolve what `bare_tool_name` on `descriptor` writes, given `arguments`.
///
/// Fixed-zone MCPs resolve through [`resolve_zone`]. Path-addressed MCPs (those with
/// `zone_from_arg`) must list every tool in `admin_tools`, `vault_wide_writes`,
/// `write_tools`, or `read_tools`. Writers resolve one zone per declared path argument.
/// An argument may be a string or an array of strings; every element is checked.
///
/// Overlap resolves strictest-first: admin, then vault-wide, then a zone write, then a
/// read. A name listed as a write is never treated as a read.
pub fn write_target(
    descriptor: &McpDescriptor,
    bare_tool_name: &str,
    arguments: &serde_json::Value,
) -> WriteTarget {
    let Some(default_arg) = descriptor.zone_from_arg.as_deref() else {
        return fixed_zone_target(descriptor, bare_tool_name);
    };
    if listed(&descriptor.admin_tools, bare_tool_name) {
        return admin_target(descriptor, bare_tool_name);
    }
    if listed(&descriptor.vault_wide_writes, bare_tool_name) {
        return WriteTarget::VaultWide;
    }
    if listed(&descriptor.write_tools, bare_tool_name) {
        return zones_for_writer(descriptor, bare_tool_name, default_arg, arguments);
    }
    if listed(&descriptor.read_tools, bare_tool_name) {
        return WriteTarget::NotAWrite;
    }
    undeclared_target(descriptor, bare_tool_name)
}

/// True only for a path-addressed writer whose arguments name exactly one zone.
///
/// Fixed-zone tools return false on purpose. There the zone comes from the tool name,
/// so a resolved zone says which folder is touched and nothing about how much of it.
pub fn names_single_write_target(
    descriptor: &McpDescriptor,
    bare_tool_name: &str,
    arguments: &serde_json::Value,
) -> bool {
    descriptor.zone_from_arg.is_some()
        && matches!(
            write_target(descriptor, bare_tool_name, arguments),
            WriteTarget::Zone(_)
        )
}

/// Target zone for a fixed-zone tool. `None` means the tool is not a zone write.
pub fn resolve_zone(descriptor: &McpDescriptor, bare_tool_name: &str) -> Option<String> {
    match descriptor
        .tool_zones
        .iter()
        .find(|(name, _)| name == bare_tool_name)
    {
        Some((_, zone)) => zone.clone(),
        None => descriptor.default_zone.clone(),
    }
}

/// Zone whose write class blocks a direct agent write, if any.
///
/// Shared by the dispatcher's pre-flight guard and the runtime gate. An MCP missing
/// from `zone_catalog` is not restricted here. A resolved zone absent from
/// `zone_write_classes` fails safe to [`WriteClass::default`] (`ProposalOnly`).
pub fn zone_write_restriction(
    mcp_name: &str,
    bare_tool_name: &str,
    zone_catalog: &[McpDescriptor],
    zone_write_classes: &[(String, WriteClass)],
) -> Option<String> {
    let descriptor = zone_catalog.iter().find(|d| d.name == mcp_name)?;
    let zone = resolve_zone(descriptor, bare_tool_name)?;
    let write_class = zone_write_classes
        .iter()
        .find(|(name, _)| name == &zone)
        .map(|(_, class)| *class)
        .unwrap_or_default();
    (!write_class.allows_direct_agent_write()).then_some(zone)
}

fn fixed_zone_target(descriptor: &McpDescriptor, bare_tool_name: &str) -> WriteTarget {
    match resolve_zone(descriptor, bare_tool_name) {
        Some(zone) => WriteTarget::Zone(zone),
        None => WriteTarget::NotAWrite,
    }
}

fn admin_target(descriptor: &McpDescriptor, bare_tool_name: &str) -> WriteTarget {
    WriteTarget::Undeterminable(format!(
        "'{bare_tool_name}' on '{}' is an admin-only operation. The runtime does not route \
         admin tools through chat or agent sessions.",
        descriptor.name
    ))
}

fn undeclared_target(descriptor: &McpDescriptor, bare_tool_name: &str) -> WriteTarget {
    WriteTarget::Undeterminable(format!(
        "'{bare_tool_name}' on '{}' is undeclared — name it in one of `write_tools`, \
         `read_tools`, or `vault_wide_writes` (in topology.toml) before this MCP can be \
         called; silently treating it as a read is the F1 bug",
        descriptor.name
    ))
}

fn listed(tools: &[String], name: &str) -> bool {
    tools.iter().any(|tool| tool == name)
}

fn zones_for_writer(
    descriptor: &McpDescriptor,
    bare_tool_name: &str,
    default_arg: &str,
    arguments: &serde_json::Value,
) -> WriteTarget {
    let declared = descriptor.write_path_args.get(bare_tool_name);
    let args: Vec<&str> = match declared {
        Some(names) if !names.is_empty() => names.iter().map(String::as_str).collect(),
        _ => vec![default_arg],
    };
    let mut zones = Vec::new();
    for arg in args {
        if let Err(why) = push_arg_zones(&mut zones, arguments, arg, bare_tool_name, true) {
            return WriteTarget::Undeterminable(why);
        }
    }
    if let Err(why) = push_confirm_zones(&mut zones, arguments, bare_tool_name) {
        return WriteTarget::Undeterminable(why);
    }
    zones_to_target(zones)
}

fn push_confirm_zones(
    zones: &mut Vec<String>,
    arguments: &serde_json::Value,
    bare_tool_name: &str,
) -> Result<(), String> {
    for arg in ["confirm_path", "confirm_from", "confirm_to"] {
        if arguments.get(arg).is_none() {
            continue;
        }
        push_arg_zones(zones, arguments, arg, bare_tool_name, false)?;
    }
    Ok(())
}

fn push_arg_zones(
    zones: &mut Vec<String>,
    arguments: &serde_json::Value,
    arg: &str,
    bare_tool_name: &str,
    required: bool,
) -> Result<(), String> {
    let Some(value) = arguments.get(arg) else {
        if required {
            return Err(missing_arg(bare_tool_name, arg));
        }
        return Ok(());
    };
    for path in path_strings(value, bare_tool_name, arg)? {
        match hard_path_zone(&path, arg, bare_tool_name) {
            Ok(zone) => push_unique(zones, zone),
            Err(why) => return Err(why),
        }
    }
    Ok(())
}

fn path_strings(
    value: &serde_json::Value,
    bare_tool_name: &str,
    arg: &str,
) -> Result<Vec<String>, String> {
    if let Some(path) = value.as_str() {
        return Ok(vec![path.to_string()]);
    }
    let Some(items) = value.as_array() else {
        return Err(format!(
            "'{bare_tool_name}' writes through arg '{arg}', which is not a string or an array of strings"
        ));
    };
    if items.is_empty() {
        return Err(format!(
            "'{bare_tool_name}' writes through arg '{arg}', which is an empty path list"
        ));
    }
    let mut paths = Vec::with_capacity(items.len());
    for item in items {
        match item.as_str() {
            Some(path) => paths.push(path.to_string()),
            None => {
                return Err(format!(
                    "'{bare_tool_name}' writes through arg '{arg}', and one entry is not a string"
                ));
            }
        }
    }
    Ok(paths)
}

fn missing_arg(bare_tool_name: &str, arg: &str) -> String {
    format!(
        "'{bare_tool_name}' writes, and its zone comes from the '{arg}' argument, \
         which is missing or not a string"
    )
}

fn push_unique(zones: &mut Vec<String>, zone: String) {
    if !zones.iter().any(|existing| existing == &zone) {
        zones.push(zone);
    }
}

fn zones_to_target(mut zones: Vec<String>) -> WriteTarget {
    if zones.len() == 1 {
        return WriteTarget::Zone(zones.remove(0));
    }
    WriteTarget::Zones(zones)
}

/// Leading vault-folder segment, or why the path is refused.
///
/// Absolute paths, drive letters, and any `..` segment are refused. `.` and empty
/// segments are skipped. The zone is the first remaining segment, unchanged in case.
fn hard_path_zone(path: &str, arg_name: &str, bare_tool_name: &str) -> Result<String, String> {
    if path.is_empty() {
        return Err(format!(
            "'{bare_tool_name}' writes through arg '{arg_name}' which is an empty string"
        ));
    }
    if let Some(why) = absolute_reason(path, arg_name, bare_tool_name) {
        return Err(why);
    }
    if let Some(why) = control_reason(path, arg_name, bare_tool_name) {
        return Err(why);
    }
    if let Some(why) = parent_reason(path, arg_name, bare_tool_name) {
        return Err(why);
    }
    let normalised = normalise_relative(path);
    zone_of_normalised(&normalised, path, arg_name, bare_tool_name)
}

fn absolute_reason(path: &str, arg_name: &str, bare_tool_name: &str) -> Option<String> {
    let drive = path.split(['/', '\\']).any(|segment| segment.contains(':'));
    if path.starts_with('/') || path.starts_with('\\') || drive {
        return Some(format!(
            "'{bare_tool_name}' writes through arg '{arg_name}' with absolute path \
             '{path}' — refuse; vault writes must be relative paths under the vault"
        ));
    }
    None
}

fn control_reason(path: &str, arg_name: &str, bare_tool_name: &str) -> Option<String> {
    if path.chars().any(|c| c.is_ascii_control()) {
        return Some(format!(
            "'{bare_tool_name}' writes through arg '{arg_name}' with control characters in \
             path '{path}' — refuse"
        ));
    }
    None
}

fn parent_reason(path: &str, arg_name: &str, bare_tool_name: &str) -> Option<String> {
    let escaped = path
        .split(['/', '\\'])
        .any(|segment| segment.trim() == "..");
    if !escaped {
        return None;
    }
    Some(format!(
        "'{bare_tool_name}' writes through arg '{arg_name}' with a parent-directory \
         segment in '{path}' — refuse; vault writes must not traverse outside the vault"
    ))
}

fn normalise_relative(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut first = true;
    for segment in path.split(['/', '\\']) {
        if segment.is_empty() || segment.trim() == "." {
            continue;
        }
        if !first {
            out.push('/');
        }
        out.push_str(segment);
        first = false;
    }
    out
}

#[cfg(test)]
#[path = "write_target_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "write_target_proptest.rs"]
mod proptest_tests;

fn zone_of_normalised(
    normalised: &str,
    original: &str,
    arg_name: &str,
    bare_tool_name: &str,
) -> Result<String, String> {
    if normalised.is_empty() {
        return Err(format!(
            "'{bare_tool_name}' writes through arg '{arg_name}' which normalises to nothing"
        ));
    }
    if !normalised.contains('/') {
        return Err(format!(
            "'{bare_tool_name}' writes to '{original}' which names no zone (expected \
             '<zone>/...'); vault writes must include a leading zone name and a separator"
        ));
    }
    match normalised.split('/').next() {
        Some(zone) if !zone.is_empty() => Ok(zone.to_string()),
        _ => Err(format!(
            "'{bare_tool_name}' writes through arg '{arg_name}' with no leading \
             segment after normalisation"
        )),
    }
}
