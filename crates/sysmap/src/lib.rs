//! # liberado-sysmap
//!
//! The data model behind Liberado's **isometric system map**: a deterministic, serializable graph
//! of (a) the workspace's crates and their build-time dependencies, and (b) the runtime
//! control/data paths that connect the daemon, dispatcher, orchestrator, executor, MCP servers,
//! vault, providers, schedules, hooks, and surfaces.
//!
//! The map is **regenerated from source**, never hand-drawn:
//!
//! * crate nodes come from `crates/*/Cargo.toml` (`[package]` name/description,
//!   `[package.metadata.liberado] role`, `[dependencies]`),
//! * runtime nodes and payload edges come from an optional `topology.toml` plus the curated
//!   runtime wiring in [`wiring`] (grounded in `docs/spec/architecture/overview.md`),
//! * the layout and projection are pure functions of the node set ([`layout`], [`iso`]), so a
//!   change to any `Cargo.toml` or `topology.toml` is reflected on the next launch with no
//!   re-examination.
//!
//! [`SystemMap`] round-trips through JSON (`liberado-sysmap --write-json`), which is the seam a
//! future renderer (web, headless export) can consume without re-deriving the graph.

pub mod iso;
pub mod layout;
pub mod model;
pub mod scan;
pub mod style;
pub mod wiring;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::Utc;

pub use model::{EdgeKind, Layer, MapEdge, MapNode, NodeKind, SystemMap};
pub use scan::ScanError;

/// Build the system map from a repository root and an optional config directory.
///
/// `config_dir` names a directory that may contain `topology.toml`; `None` produces a crates-only
/// map (dependency graph, no runtime overlay). Runtime wiring edges whose endpoints are absent are
/// dropped, so a missing topology yields the crates DAG plus the always-present crate-to-crate
/// runtime loop.
pub fn build(root: &Path, config_dir: Option<&Path>) -> Result<SystemMap, ScanError> {
    let mut nodes = scan::scan_repository(root)?;

    let topo = scan::load_topology(config_dir)?;
    if let Some(t) = &topo {
        nodes.extend(scan::build_runtime_nodes(t));
    }

    // Deduplicate by id (a crate id can never collide with a `kind:`-prefixed runtime id).
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    nodes.dedup_by(|a, b| a.id == b.id);

    let existing: BTreeSet<String> = nodes.iter().map(|n| n.id.clone()).collect();

    let mut edges: Vec<MapEdge> = Vec::new();

    // Build-time dependency edges (crates only).
    for node in &nodes {
        if node.kind != NodeKind::Crate {
            continue;
        }
        for dep in &node.deps {
            if existing.contains(dep) {
                edges.push(MapEdge {
                    from: node.id.clone(),
                    to: dep.clone(),
                    kind: EdgeKind::Dependency,
                    label: String::new(),
                });
            }
        }
    }

    // Declared per-crate runtime flows — the generic, codebase-owned wiring. A crate that states
    // its own outbound flows replaces the built-in seed for itself (see `DeclaredFlow`).
    let declared_from: BTreeSet<String> = nodes
        .iter()
        .filter(|n| !n.flows.is_empty())
        .map(|n| n.id.clone())
        .collect();
    for node in &nodes {
        for flow in &node.flows {
            edges.push(MapEdge {
                from: node.id.clone(),
                to: flow.to.clone(),
                kind: flow.kind,
                label: flow.label.clone(),
            });
        }
    }

    // Seed runtime crate-to-crate flows, applied only to crates that did NOT declare their own.
    for edge in wiring::crate_runtime_edges() {
        if declared_from.contains(&edge.from) {
            continue;
        }
        edges.push(edge);
    }

    // Runtime instance flows, only when a topology declared the instances.
    if let Some(t) = &topo {
        edges.extend(wiring::topology_edges(t));
    }

    // Drop edges referencing missing nodes and deduplicate (from,to,kind).
    edges.retain(|e| existing.contains(&e.from) && existing.contains(&e.to));
    edges.sort_by(|a, b| {
        (&a.from, &a.to, &a.kind, &a.label).cmp(&(&b.from, &b.to, &b.kind, &b.label))
    });
    edges.dedup_by(|a, b| a.from == b.from && a.to == b.to && a.kind == b.kind);

    Ok(SystemMap {
        generated_at: Utc::now().to_rfc3339(),
        repository_root: root.to_string_lossy().into_owned(),
        config_dir: config_dir.map(|p| p.to_string_lossy().into_owned()),
        nodes,
        edges,
    })
}

/// Walk up from the current directory to the repository root (a directory containing both
/// `Cargo.toml` and `crates/`). Mirrors the CLI's `repository_root` resolution.
pub fn repository_root() -> Result<PathBuf, String> {
    let mut current = std::env::current_dir().map_err(|e| e.to_string())?;
    loop {
        if current.join("Cargo.toml").is_file() && current.join("crates").is_dir() {
            return Ok(current);
        }
        if !current.pop() {
            return Err(
                "could not find repository root (expected Cargo.toml and crates/)".to_string(),
            );
        }
    }
}

/// Resolve a config directory for the runtime overlay: an explicit `--config-dir` wins, then
/// `LIBERADO_CONFIG_DIR`, then the platform config dir's `liberado/` subfolder (via the `dirs`
/// crate). Returns `None` when no directory is resolvable.
pub fn resolve_config_dir(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = explicit {
        return Some(dir.to_path_buf());
    }
    if let Ok(dir) = std::env::var("LIBERADO_CONFIG_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    dirs_config_dir().map(|base| base.join("liberado"))
}

fn dirs_config_dir() -> Option<PathBuf> {
    // `dirs` is avoided as a direct dependency here; the platform config dir is computed from the
    // standard environment variables so the sysmap model stays dependency-light.
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| {
            std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData/Roaming"))
        })
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn add_crate(dir: &Path, name: &str, role: &str, deps: &[&str], flows: &[(&str, &str, &str)]) {
        let crate_dir = dir
            .join("crates")
            .join(name.strip_prefix("liberado-").unwrap());
        fs::create_dir_all(&crate_dir).unwrap();
        let deps_toml = deps
            .iter()
            .map(|d| format!("{d} = {{ workspace = true }}\n"))
            .collect::<String>();
        let flows_toml = flows
            .iter()
            .map(|(to, kind, label)| {
                format!(
                    "[[package.metadata.liberado.flows]]\nto = \"{to}\"\nkind = \"{kind}\"\nlabel = \"{label}\"\n"
                )
            })
            .collect::<String>();
        fs::write(
            crate_dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\ndescription = \"{name} crate\"\n[package.metadata.liberado]\nrole = \"{role}\"\n{flows_toml}[dependencies]\n{deps_toml}"
            ),
        )
        .unwrap();
    }

    fn repo_with_crates() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        add_crate(dir.path(), "liberado-common", "foundation", &[], &[]);
        add_crate(
            dir.path(),
            "liberado-provider",
            "foundation",
            &["liberado-common"],
            &[],
        );
        add_crate(
            dir.path(),
            "liberado-daemon",
            "root",
            &["liberado-common", "liberado-provider"],
            &[],
        );
        dir
    }

    #[test]
    fn builds_crate_dependency_edges() {
        let dir = repo_with_crates();
        let map = build(dir.path(), None).unwrap();
        assert_eq!(map.nodes.len(), 3);
        // liberado-provider depends on liberado-common; liberado-daemon depends on both.
        let dep_edges: Vec<_> = map
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Dependency)
            .collect();
        assert_eq!(dep_edges.len(), 3);
        assert!(map.node("liberado-common").is_some());
    }

    #[test]
    fn declared_flows_produce_runtime_edges() {
        let dir = tempdir().unwrap();
        add_crate(dir.path(), "liberado-common", "foundation", &[], &[]);
        add_crate(
            dir.path(),
            "liberado-daemon",
            "root",
            &[],
            &[("liberado-common", "control", "act")],
        );
        let map = build(dir.path(), None).unwrap();
        assert!(map.edges.iter().any(|e| e.from == "liberado-daemon"
            && e.to == "liberado-common"
            && e.kind == EdgeKind::Control
            && e.label == "act"));
        // The flow is also serialized on the node for round-tripping.
        let daemon = map.node("liberado-daemon").unwrap();
        assert_eq!(daemon.flows.len(), 1);
        assert_eq!(daemon.flows[0].to, "liberado-common");
    }

    #[test]
    fn drops_runtime_edges_to_missing_nodes() {
        let dir = repo_with_crates();
        // No topology: vault/runtime nodes are absent, so the vault→daemon data edge must be dropped.
        let map = build(dir.path(), None).unwrap();
        assert!(
            !map.edges
                .iter()
                .any(|e| e.from == "vault" || e.to == "vault")
        );
        assert!(map.nodes.iter().all(|n| n.kind == NodeKind::Crate));
    }

    #[test]
    fn resolve_config_dir_prefers_explicit_path() {
        let dir = repo_with_crates();
        let explicit = dir.path().join("config");
        let resolved = resolve_config_dir(Some(&explicit)).unwrap();
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn topology_adds_runtime_nodes_and_instance_edges() {
        let dir = repo_with_crates();
        // Add the crates the runtime wiring references so the loop paths materialize. The server
        // declares its own flow (as the real crate does), exercising the declared-flow mechanism.
        add_crate(
            dir.path(),
            "liberado-server",
            "root",
            &["liberado-daemon"],
            &[("liberado-daemon", "control", "inject event (event_sender)")],
        );
        add_crate(
            dir.path(),
            "liberado-mcp",
            "kernel",
            &["liberado-provider"],
            &[],
        );
        add_crate(
            dir.path(),
            "liberado-cron",
            "kernel",
            &["liberado-common"],
            &[],
        );
        let config = dir.path().join("config");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("topology.toml"),
            r#"
vault_path = "/tmp/vault"
provider = "deepseek"

[[mcps]]
name = "tasks-mcp"
description = "task tools"
consequence = "reversible"
transport = { kind = "stdio", command = "npx", args = ["-y", "@liberado/tasks-mcp"] }
default_zone = "tasks"

[[hooks]]
name = "nightly-backup"
secret_ref = "LIBERADO_HOOK_BACKUP_SECRET"
goal = "Run the nightly vault backup."
enabled = true
"#,
        )
        .unwrap();

        let map = build(dir.path(), Some(&config)).unwrap();
        assert!(map.node("vault").is_some());
        assert!(map.node("provider:deepseek").is_some());
        assert!(map.node("mcp:tasks-mcp").is_some());
        assert!(map.node("hook:nightly-backup").is_some());

        // The vault-writing MCP gets a data edge to the vault.
        assert!(
            map.edges
                .iter()
                .any(|e| e.from == "mcp:tasks-mcp" && e.to == "vault" && e.kind == EdgeKind::Data)
        );
        // The hook injects into the server (which exists here), then the server into the daemon
        // via its declared flow.
        assert!(
            map.edges
                .iter()
                .any(|e| e.from == "hook:nightly-backup" && e.to == "liberado-server")
        );
        assert!(map.edges.iter().any(|e| e.from == "liberado-server"
            && e.to == "liberado-daemon"
            && e.kind == EdgeKind::Control));
        // The provider backend is served by the openai-compat crate when that crate exists.
        // (Here it is absent, so that edge is dropped — no dangling edges are allowed at all.)
        assert!(map.edges.iter().all(|e| {
            let a = map.node(&e.from).is_some();
            let b = map.node(&e.to).is_some();
            a && b
        }));
    }
}
