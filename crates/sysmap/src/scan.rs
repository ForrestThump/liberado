//! Scans the repository's `crates/*/Cargo.toml` manifests and an optional `topology.toml` into
//! [`MapNode`]s. This is the *data acquisition* half of the map; it reuses the same `toml`-based
//! manifest reading as `crates/test-support/tests/layer_rules.rs` and the real config model from
//! `liberado-config-loader` (no re-declared field names to drift).

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use liberado_common::Consequence;
use liberado_config_loader::model::SessionProfile;
use liberado_config_loader::{CronSchedule, HookConfig, McpConfig, McpTransport, PoolConfig};
use liberado_config_loader::{ProjectConfig, Topology};

use crate::model::{DeclaredFlow, EdgeKind, Layer, MapNode, NodeKind};

/// An error while scanning manifests or topology.
#[derive(Debug)]
pub enum ScanError {
    Io {
        path: PathBuf,
        source: Box<std::io::Error>,
    },
    Toml {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Io { path, source } => write!(f, "reading {}: {source}", path.display()),
            ScanError::Toml { path, source } => write!(f, "parsing {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for ScanError {}

type Result<T> = std::result::Result<T, ScanError>;

fn is_internal(dep: &str) -> bool {
    // `sysmap-core` is the one workspace crate without the `liberado-` prefix (it is the liftable
    // core); the prefix heuristic here is replaced by workspace-membership in the cargo-metadata
    // phase (see docs/future-work/sysmap-generic-core-plan.md).
    dep.starts_with("liberado-") || dep == "chat-client-contract" || dep == "sysmap-core"
}

/// The layer a runtime node is *grouped near* for coloring. Runtime nodes use their kind color for
/// the building itself; this value feeds only ordering and the detail panel, not the building
/// color (see [`crate::style::node_color`]).
pub fn runtime_layer(kind: &str) -> Layer {
    let layer = match kind {
        "provider" | "notifier" => "foundation",
        "mcp" | "hook" => "service",
        "pool" | "profile" | "schedule" => "kernel",
        "project" => "pack",
        "vault" => "store",
        _ => "unknown",
    };
    Layer::from(layer)
}

/// Scan every `crates/*/Cargo.toml` under `root`, returning crate nodes sorted by id.
pub fn scan_repository(root: &Path) -> Result<Vec<MapNode>> {
    let crates_dir = root.join("crates");
    let mut nodes = Vec::new();
    let entries = fs::read_dir(&crates_dir).map_err(|e| ScanError::Io {
        path: crates_dir.clone(),
        source: Box::new(e),
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| ScanError::Io {
            path: crates_dir.clone(),
            source: Box::new(e),
        })?;
        let dir = entry.path();
        let manifest_path = dir.join("Cargo.toml");
        if !manifest_path.is_file() {
            continue;
        }
        if let Some(node) = read_manifest(&manifest_path)? {
            nodes.push(node);
        }
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(nodes)
}

fn read_manifest(manifest_path: &Path) -> Result<Option<MapNode>> {
    let raw = fs::read_to_string(manifest_path).map_err(|e| ScanError::Io {
        path: manifest_path.to_path_buf(),
        source: Box::new(e),
    })?;
    let manifest: toml::Value = toml::from_str(&raw).map_err(|e| ScanError::Toml {
        path: manifest_path.to_path_buf(),
        source: Box::new(e),
    })?;

    let Some(package) = manifest.get("package") else {
        return Ok(None);
    };
    let Some(name) = package.get("name").and_then(|v| v.as_str()) else {
        return Ok(None);
    };

    let role_str = package
        .get("metadata")
        .and_then(|m| m.get("liberado"))
        .and_then(|l| l.get("role"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let description = package
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let mut deps: Vec<String> = manifest
        .get("dependencies")
        .and_then(|d| d.as_table())
        .map(|deps| deps.keys().filter(|k| is_internal(k)).cloned().collect())
        .unwrap_or_default();
    deps.sort();

    // Declared runtime wiring: `[[package.metadata.liberado.flows]]`. A crate states its own
    // outbound flows here; the tool only reads them (see `DeclaredFlow`).
    let flows: Vec<DeclaredFlow> = package
        .get("metadata")
        .and_then(|m| m.get("liberado"))
        .and_then(|l| l.get("flows"))
        .and_then(|f| f.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    let to = entry.get("to")?.as_str()?.to_string();
                    let kind = match entry.get("kind").and_then(|v| v.as_str()) {
                        Some("control") => EdgeKind::Control,
                        _ => EdgeKind::Data,
                    };
                    let label = entry
                        .get("label")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    Some(DeclaredFlow { to, kind, label })
                })
                .collect()
        })
        .unwrap_or_default();

    let layer = if role_str.is_empty() {
        Layer::unknown()
    } else {
        Layer::from(role_str)
    };

    Ok(Some(MapNode {
        id: name.to_string(),
        label: name.to_string(),
        kind: NodeKind::crate_kind(),
        layer,
        description,
        deps,
        flows,
        meta: BTreeMap::new(),
        enabled: true,
    }))
}

/// Load `topology.toml` from `config_dir` if present. `None` means "no runtime overlay".
pub fn load_topology(config_dir: Option<&Path>) -> Result<Option<Topology>> {
    let Some(dir) = config_dir else {
        return Ok(None);
    };
    let path = dir.join("topology.toml");
    if !path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).map_err(|e| ScanError::Io {
        path: path.clone(),
        source: Box::new(e),
    })?;
    let topo: Topology = toml::from_str(&raw).map_err(|e| ScanError::Toml {
        path: path.clone(),
        source: Box::new(e),
    })?;
    Ok(Some(topo))
}

fn enabled(v: &bool) -> bool {
    *v
}

/// Build runtime nodes from a loaded topology. These are the *instances* the wiring points at.
pub fn build_runtime_nodes(topo: &Topology) -> Vec<MapNode> {
    let mut nodes = Vec::new();

    // The vault is always present in a topology (it is the source of truth), but only meaningful
    // once a path is declared.
    let vault_meta = if topo.vault_path.as_os_str().is_empty() {
        BTreeMap::new()
    } else {
        BTreeMap::from([(
            "path".to_string(),
            topo.vault_path.to_string_lossy().into_owned(),
        )])
    };
    nodes.push(MapNode {
        id: "vault".to_string(),
        label: "vault".to_string(),
        kind: NodeKind::from("vault"),
        layer: runtime_layer("vault"),
        description: "The Obsidian vault — source of truth and write target".to_string(),
        deps: Vec::new(),
        flows: Vec::new(),
        meta: vault_meta,
        enabled: true,
    });

    for p in &topo.providers {
        let mut meta = BTreeMap::from([
            ("base_url".to_string(), p.base_url.clone()),
            ("default_model".to_string(), p.default_model.clone()),
            ("api_key_env".to_string(), p.api_key_env.clone()),
        ]);
        if topo.provider == p.name {
            meta.insert("active".to_string(), "true".to_string());
        }
        nodes.push(MapNode {
            id: format!("provider:{}", p.name),
            label: p.name.clone(),
            kind: NodeKind::from("provider"),
            layer: runtime_layer("provider"),
            description: "Inference backend (OpenAI-compatible)".to_string(),
            deps: Vec::new(),
            flows: Vec::new(),
            meta,
            enabled: true,
        });
    }

    for m in &topo.mcps {
        nodes.push(mcp_node(m));
    }

    for pool in &topo.pools {
        nodes.push(pool_node(pool));
    }

    for profile in &topo.session_profiles {
        nodes.push(profile_node(profile));
    }

    for project in &topo.projects {
        nodes.push(project_node(project));
    }

    for schedule in &topo.schedules {
        nodes.push(schedule_node(schedule));
    }

    for hook in &topo.hooks {
        nodes.push(hook_node(hook));
    }

    // The notifier is fixed infrastructure today (Telegram); it exists so the notify payload path
    // has a destination even without a topology declaring it.
    nodes.push(MapNode {
        id: "notifier:telegram".to_string(),
        label: "telegram".to_string(),
        kind: NodeKind::from("notifier"),
        layer: runtime_layer("notifier"),
        description: "Human-facing notification channel".to_string(),
        deps: Vec::new(),
        flows: Vec::new(),
        meta: BTreeMap::new(),
        enabled: true,
    });

    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    nodes
}

fn mcp_node(m: &McpConfig) -> MapNode {
    let mut meta = BTreeMap::from([
        ("consequence".to_string(), format!("{:?}", m.consequence)),
        (
            "transport".to_string(),
            transport_label(&m.transport).to_string(),
        ),
    ]);
    match &m.transport {
        McpTransport::Stdio { command, args } => {
            meta.insert("command".to_string(), command.clone());
            if !args.is_empty() {
                meta.insert("args".to_string(), args.join(" "));
            }
        }
        McpTransport::Http { url } => {
            meta.insert("url".to_string(), url.clone());
        }
        McpTransport::Managed => {
            meta.insert("managed".to_string(), "true".to_string());
        }
        McpTransport::Docker { image, .. } => {
            meta.insert("image".to_string(), image.clone());
        }
    }
    if m.default_zone.is_some() {
        meta.insert(
            "default_zone".to_string(),
            m.default_zone.clone().unwrap_or_default(),
        );
    }
    if m.writes_vault == Some(false) {
        meta.insert("writes_vault".to_string(), "false".to_string());
    }

    MapNode {
        id: format!("mcp:{}", m.name),
        label: m.name.clone(),
        kind: NodeKind::from("mcp"),
        layer: runtime_layer("mcp"),
        description: if m.description.is_empty() {
            "MCP server".to_string()
        } else {
            m.description.clone()
        },
        deps: Vec::new(),
        flows: Vec::new(),
        meta,
        enabled: enabled(&m.enabled),
    }
}

fn pool_node(pool: &PoolConfig) -> MapNode {
    MapNode {
        id: format!("pool:{}", pool.name),
        label: pool.name.clone(),
        kind: NodeKind::from("pool"),
        layer: runtime_layer("pool"),
        description: "Authority-segregated dispatcher/executor pool".to_string(),
        deps: Vec::new(),
        flows: Vec::new(),
        meta: BTreeMap::new(),
        enabled: enabled(&pool.enabled),
    }
}

fn profile_node(profile: &SessionProfile) -> MapNode {
    let mut meta: BTreeMap<String, String> = BTreeMap::new();
    if let Some(domain) = &profile.domain {
        meta.insert("domain".to_string(), domain.to_string());
    }
    if let Some(component) = &profile.component {
        meta.insert("component".to_string(), component.to_string());
    }
    if let Some(description) = &profile.description {
        meta.insert("note".to_string(), description.to_string());
    }
    MapNode {
        id: format!("profile:{}", profile.name),
        label: profile.name.clone(),
        kind: NodeKind::from("profile"),
        layer: runtime_layer("profile"),
        description: "Session profile (pack + authority hat)".to_string(),
        deps: Vec::new(),
        flows: Vec::new(),
        meta,
        enabled: enabled(&profile.enabled),
    }
}

fn project_node(project: &ProjectConfig) -> MapNode {
    let mut meta = BTreeMap::from([
        (
            "root".to_string(),
            project.root.to_string_lossy().into_owned(),
        ),
        (
            "write_class".to_string(),
            format!("{:?}", project.write_class),
        ),
    ]);
    meta.insert("name".to_string(), project.name.clone());
    MapNode {
        id: format!("project:{}", project.name),
        label: project.name.clone(),
        kind: NodeKind::from("project"),
        layer: runtime_layer("project"),
        description: "Authorized coding workspace root".to_string(),
        deps: Vec::new(),
        flows: Vec::new(),
        meta,
        enabled: enabled(&project.enabled),
    }
}

fn schedule_node(schedule: &CronSchedule) -> MapNode {
    let mut meta = BTreeMap::from([
        ("cron_expr".to_string(), schedule.cron_expr.clone()),
        ("goal".to_string(), schedule.goal.clone()),
    ]);
    if let Some(pool) = &schedule.pool {
        meta.insert("pool".to_string(), pool.clone());
    }
    if let Some(profile) = &schedule.profile {
        meta.insert("profile".to_string(), profile.clone());
    }
    MapNode {
        id: format!("schedule:{}", schedule.name),
        label: schedule.name.clone(),
        kind: NodeKind::from("schedule"),
        layer: runtime_layer("schedule"),
        description: "Cron schedule (temporal event source)".to_string(),
        deps: Vec::new(),
        flows: Vec::new(),
        meta,
        enabled: enabled(&schedule.enabled),
    }
}

fn hook_node(hook: &HookConfig) -> MapNode {
    let mut meta = BTreeMap::from([
        ("secret_ref".to_string(), hook.secret_ref.clone()),
        ("goal".to_string(), hook.goal.clone()),
    ]);
    if let Some(pool) = &hook.pool {
        meta.insert("pool".to_string(), pool.clone());
    }
    if let Some(profile) = &hook.profile {
        meta.insert("profile".to_string(), profile.clone());
    }
    MapNode {
        id: format!("hook:{}", hook.name),
        label: hook.name.clone(),
        kind: NodeKind::from("hook"),
        layer: runtime_layer("hook"),
        description: "Webhook (network event source)".to_string(),
        deps: Vec::new(),
        flows: Vec::new(),
        meta,
        enabled: enabled(&hook.enabled),
    }
}

fn transport_label(t: &McpTransport) -> &'static str {
    match t {
        McpTransport::Stdio { .. } => "stdio",
        McpTransport::Http { .. } => "http",
        McpTransport::Managed => "managed",
        McpTransport::Docker { .. } => "docker",
    }
}

/// Which MCPs write into vault zones (so we can draw their data path to the vault). A non-read-only
/// MCP that has a zone declared or a path-addressed write target writes the vault; `writes_vault =
/// false` is an explicit opt-out.
pub fn mcp_writes_vault(m: &McpConfig) -> bool {
    if m.consequence == Consequence::ReadOnly {
        return false;
    }
    if m.writes_vault == Some(false) {
        return false;
    }
    m.default_zone.is_some() || m.zone_from_arg.is_some()
}

/// Whether a profile runs the coding domain (the coding pack is the only declared domain today).
pub fn profile_domain_is_coding(profile: &SessionProfile) -> bool {
    profile.domain.as_deref() == Some("coding")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn scans_manifest_name_role_description_deps() {
        let dir = tempdir().unwrap();
        let crate_dir = dir.path().join("crates/demo");
        fs::create_dir_all(&crate_dir).unwrap();
        fs::write(
            crate_dir.join("Cargo.toml"),
            r#"
[package]
name = "liberado-demo"
description = "A demo crate"
[package.metadata.liberado]
role = "kernel"
[dependencies]
liberado-common = { workspace = true }
serde = "1"
[dev-dependencies]
liberado-provider = { workspace = true }
"#,
        )
        .unwrap();

        let nodes = scan_repository(dir.path()).unwrap();
        assert_eq!(nodes.len(), 1);
        let n = &nodes[0];
        assert_eq!(n.id, "liberado-demo");
        assert_eq!(n.layer, Layer::from("kernel"));
        assert_eq!(n.description, "A demo crate");
        // dev-dependencies are excluded; only real internal deps count.
        assert_eq!(n.deps, vec!["liberado-common".to_string()]);
    }

    #[test]
    fn untagged_crate_maps_to_unknown_not_skipped() {
        let dir = tempdir().unwrap();
        let crate_dir = dir.path().join("crates/demo");
        fs::create_dir_all(&crate_dir).unwrap();
        fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname = \"liberado-untagged\"\ndescription = \"x\"\n",
        )
        .unwrap();
        let nodes = scan_repository(dir.path()).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].layer, Layer::unknown());
    }

    #[test]
    fn mcp_writes_vault_classifies_by_zone_declaration() {
        let read_only = McpConfig {
            name: "ro".into(),
            enabled: true,
            description: "reads".into(),
            consequence: Consequence::ReadOnly,
            transport: McpTransport::Managed,
            default_zone: None,
            tools: Vec::new(),
            zone_from_arg: None,
            write_tools: Vec::new(),
            writes_vault: None,
        };
        assert!(!mcp_writes_vault(&read_only));

        let fixed_zone = McpConfig {
            name: "w".into(),
            enabled: true,
            description: "writes".into(),
            consequence: Consequence::Reversible,
            transport: McpTransport::Managed,
            default_zone: Some("tasks".into()),
            tools: Vec::new(),
            zone_from_arg: None,
            write_tools: Vec::new(),
            writes_vault: None,
        };
        assert!(mcp_writes_vault(&fixed_zone));

        let opted_out = McpConfig {
            name: "opt".into(),
            enabled: true,
            description: "external writes".into(),
            consequence: Consequence::Reversible,
            transport: McpTransport::Managed,
            default_zone: None,
            tools: Vec::new(),
            zone_from_arg: None,
            write_tools: Vec::new(),
            writes_vault: Some(false),
        };
        assert!(!mcp_writes_vault(&opted_out));
    }
}
