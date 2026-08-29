//! # liberado-sysmap
//!
//! The Liberado adapter behind its **interactive 2D system map**: a deterministic, serializable
//! graph of (a) the workspace's crates and their build-time dependencies, and (b) the runtime
//! control/data paths that connect the daemon, dispatcher, orchestrator, executor, MCP servers,
//! vault, providers, schedules, hooks, and surfaces.
//!
//! The map is **regenerated from source**, never hand-drawn:
//!
//! * crate scanning and map assembly live in `sysmap-core` (`cargo metadata` + the `sysmap.toml`
//!   profile rules); this crate supplies Liberado's profile and translates `topology.toml` into
//!   runtime nodes,
//! * the layout and projection are pure functions of the node set ([`layout`], [`iso`]), so a
//!   change to any `Cargo.toml` or `topology.toml` is reflected on the next launch with no
//!   re-examination.
//!
//! [`SystemMap`] round-trips through JSON (`liberado-sysmap --write-json`), which is the seam a
//! future renderer (web, headless export) can consume without re-deriving the graph.

pub mod profile;
pub mod scan;

// The project-agnostic half (model, layout, projection, styling, vocabulary, profile/rule engine)
// lives in `sysmap-core` and is re-exported unchanged so `liberado_sysmap::model`, `::layout`, …
// keep resolving. `profile` is Liberado's sysmap.toml data (the part that stays project-specific).
pub use sysmap_core::{iso, layout, model, style, vocab};

use std::fmt;
use std::path::{Path, PathBuf};

pub use model::{EdgeKind, Layer, MapEdge, MapNode, NodeKind, SystemMap};

/// An error while building the map: a topology read/parse failure, or a core (cargo metadata)
/// failure.
#[derive(Debug)]
pub enum BuildError {
    Topology(scan::ScanError),
    Core(sysmap_core::scan::ScanError),
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Topology(e) => write!(f, "{e}"),
            BuildError::Core(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BuildError {}

/// Build the system map from a repository root and an optional config directory.
///
/// `config_dir` names a directory that may contain `topology.toml`; `None` produces a crates-only
/// map (dependency graph plus the always-present crate-to-crate runtime flows). The cargo scan and
/// assembly are delegated to `sysmap-core`; this crate only supplies the profile and the runtime
/// topology nodes.
pub fn build(root: &Path, config_dir: Option<&Path>) -> Result<SystemMap, BuildError> {
    let profile = profile::liberado_profile();

    let extra_nodes = match scan::load_topology(config_dir).map_err(BuildError::Topology)? {
        Some(topo) => scan::build_runtime_nodes(&topo),
        None => Vec::new(),
    };

    sysmap_core::build::build(
        root,
        &profile,
        extra_nodes,
        config_dir.map(|p| p.to_string_lossy().into_owned()),
    )
    .map_err(BuildError::Core)
}

/// Walk up from the current directory to the repository root (a directory containing both
/// `Cargo.toml` and `crates/`). Mirrors the CLI's `repository_root` resolution.
pub fn repository_root() -> Result<PathBuf, String> {
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    find_repo_root(&current).ok_or_else(|| {
        "could not find repository root (expected Cargo.toml and crates/)".to_string()
    })
}

/// Walk up from `start` to the repository root (a directory containing both `Cargo.toml` and
/// `crates/`). Pure, so tests can drive it without touching the process cwd. `None` means the
/// walk ran off the top of `start` without finding a candidate.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join("Cargo.toml").is_file() && current.join("crates").is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
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

    fn write_workspace(dir: &Path) {
        fs::write(
            dir.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
    }

    fn add_crate(dir: &Path, name: &str, role: &str, deps: &[&str], flows: &[(&str, &str, &str)]) {
        let crate_dir = dir.join("crates").join(name);
        fs::create_dir_all(crate_dir.join("src")).unwrap();
        fs::write(crate_dir.join("src/lib.rs"), "// test fixture\n").unwrap();
        let deps_toml = deps
            .iter()
            .map(|d| format!("{d} = {{ path = \"../{d}\" }}\n"))
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
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\ndescription = \"{name} crate\"\n[package.metadata.liberado]\nrole = \"{role}\"\n{flows_toml}[dependencies]\n{deps_toml}"
            ),
        )
        .unwrap();
    }

    fn repo_with_crates() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        write_workspace(dir.path());
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
        write_workspace(dir.path());
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
        assert!(map.nodes.iter().all(|n| n.kind.is_crate()));
    }

    #[test]
    fn resolve_config_dir_prefers_explicit_path() {
        let dir = repo_with_crates();
        let explicit = dir.path().join("config");
        let resolved = resolve_config_dir(Some(&explicit)).unwrap();
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn repository_root_returns_an_absolute_workspace_root() {
        let root = repository_root().expect("workspace root");
        assert!(
            root.is_absolute(),
            "repository_root must return an absolute path: {root:?}"
        );
        assert!(root.join("Cargo.toml").is_file());
        assert!(root.join("crates").is_dir());
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

    #[test]
    fn find_repo_root_walks_up_through_crates_only_directories() {
        // A directory with `crates/` but no `Cargo.toml` is NOT the repo root — the function must
        // keep walking until it finds both. The `||` mutation on the predicate used to short-circuit
        // here and return the first directory containing `crates/`.
        let dir = tempdir().unwrap();
        let nested = dir.path().join("deep").join("nested");
        fs::create_dir_all(nested.join("crates")).unwrap();
        // No Cargo.toml at `nested` — the walk must continue.
        let root = dir.path().to_path_buf();
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::create_dir(root.join("crates")).unwrap();
        assert_eq!(find_repo_root(&nested).as_ref(), Some(&root));
    }

    #[test]
    fn find_repo_root_returns_none_at_filesystem_top() {
        // Walking off the top without finding a Cargo.toml+crates pair returns None, not a panic
        // and not a wrong root. This pins the no-match contract. The walk-up test above, not this
        // one, catches deletion of `!` from the `current.pop()` termination check.
        let dir = tempdir().unwrap();
        let mut no_workspace = dir.path().to_path_buf();
        // Strip everything down to a leaf directory that has no Cargo.toml/crates at any ancestor
        // within the tempdir. tempdir() returns a path whose first component IS unique, so no
        // ancestor above it qualifies.
        no_workspace.push("leaf");
        fs::create_dir_all(&no_workspace).unwrap();
        assert_eq!(find_repo_root(&no_workspace), None);
    }

    #[test]
    fn find_repo_root_accepts_cwd_at_the_root_itself() {
        // Starting AT the repo root must return that root, not walk up to the temp parent.
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::create_dir(root.join("crates")).unwrap();
        assert_eq!(find_repo_root(&root).as_ref(), Some(&root));
    }
}
