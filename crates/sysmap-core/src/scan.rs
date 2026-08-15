//! Scans a cargo workspace into crate [`MapNode`]s via `cargo metadata`. Workspace membership
//! decides what counts as an *internal* dependency (no name-prefix heuristic); the declared `role`
//! and `flows` come from each package's `[package.metadata.<namespace>]`, where the namespace is a
//! profile setting — so this crate stays reusable across projects.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use cargo_metadata::{Dependency, DependencyKind, MetadataCommand, Package};

use crate::model::{DeclaredFlow, EdgeKind, Layer, MapNode, NodeKind};

/// An error while scanning the workspace.
#[derive(Debug)]
pub enum ScanError {
    /// `cargo metadata` failed (missing cargo, malformed workspace, …).
    Cargo { source: cargo_metadata::Error },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Cargo { source } => write!(f, "cargo metadata: {source}"),
        }
    }
}

impl std::error::Error for ScanError {}

type Result<T> = std::result::Result<T, ScanError>;

/// Which dependency kinds become internal edges. Defaults match the old `[dependencies]`-only
/// scanner (dev- and build-dependencies are excluded), so behavior is unchanged unless a caller
/// opts in.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanOptions {
    pub include_dev: bool,
    pub include_build: bool,
}

/// Scan the workspace rooted at `root` (its `Cargo.toml` must be the workspace manifest) into
/// crate nodes, sorted by id.
pub fn scan_repository(root: &Path, namespace: &str) -> Result<Vec<MapNode>> {
    scan_repository_with(root, namespace, ScanOptions::default())
}

/// Scan with an explicit dependency-kind policy.
pub fn scan_repository_with(
    root: &Path,
    namespace: &str,
    opts: ScanOptions,
) -> Result<Vec<MapNode>> {
    let metadata = MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .map_err(|e| ScanError::Cargo { source: e })?;

    // Workspace members are the "internal" packages: an internal dependency edge is any direct
    // dependency that resolves to one of these. This is the real workspace-membership relation, so
    // any workspace layout and any crate naming convention works.
    let members: BTreeSet<String> = metadata
        .packages
        .iter()
        .map(|p| p.name.to_string())
        .collect();

    let mut nodes = Vec::new();
    for package in &metadata.packages {
        if let Some(node) = node_from_package(package, namespace, &members, &opts) {
            nodes.push(node);
        }
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(nodes)
}

fn node_from_package(
    package: &Package,
    namespace: &str,
    members: &BTreeSet<String>,
    opts: &ScanOptions,
) -> Option<MapNode> {
    let role_str = package
        .metadata
        .get(namespace)
        .and_then(|l| l.get("role"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let description = package.description.clone().unwrap_or_default();

    let mut deps: Vec<String> = package
        .dependencies
        .iter()
        .filter(|d| include_dep(d, opts))
        .filter(|d| members.contains(d.name.as_str()))
        .map(|d| d.name.clone())
        .collect();
    deps.sort();
    deps.dedup();

    let layer = if role_str.is_empty() {
        Layer::unknown()
    } else {
        Layer::from(role_str)
    };

    Some(MapNode {
        id: package.name.to_string(),
        label: package.name.to_string(),
        kind: NodeKind::crate_kind(),
        layer,
        description,
        deps,
        flows: parse_flows(package, namespace),
        meta: BTreeMap::new(),
        enabled: true,
    })
}

/// Declared runtime wiring: `[[package.metadata.<namespace>.flows]]`. A crate states its own
/// outbound flows here; the tool only reads them (see `DeclaredFlow`).
fn parse_flows(package: &Package, namespace: &str) -> Vec<DeclaredFlow> {
    package
        .metadata
        .get(namespace)
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
        .unwrap_or_default()
}

fn include_dep(dep: &Dependency, opts: &ScanOptions) -> bool {
    match dep.kind {
        DependencyKind::Normal => true,
        DependencyKind::Development => opts.include_dev,
        DependencyKind::Build => opts.include_build,
        DependencyKind::Unknown => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Layer;
    use std::fs;
    use tempfile::tempdir;

    fn write_workspace(root: &Path) {
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
    }

    fn add_crate(root: &Path, name: &str, role: Option<&str>, deps: &[&str], dev_deps: &[&str]) {
        let dir = root.join("crates").join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/lib.rs"), "// test fixture\n").unwrap();
        let role_toml = match role {
            Some(r) => format!("[package.metadata.liberado]\nrole = \"{r}\"\n"),
            None => String::new(),
        };
        let deps_toml = deps
            .iter()
            .map(|d| format!("{d} = {{ path = \"../{d}\" }}\n"))
            .collect::<String>();
        let dev_deps_toml = if dev_deps.is_empty() {
            String::new()
        } else {
            format!(
                "[dev-dependencies]\n{}",
                dev_deps
                    .iter()
                    .map(|d| format!("{d} = {{ path = \"../{d}\" }}\n"))
                    .collect::<String>()
            )
        };
        fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\ndescription = \"{name} crate\"\n{role_toml}[dependencies]\n{deps_toml}{dev_deps_toml}"
            ),
        )
        .unwrap();
    }

    #[test]
    fn scans_name_role_description_and_internal_deps_by_workspace_membership() {
        let dir = tempdir().unwrap();
        write_workspace(dir.path());
        add_crate(dir.path(), "demo-common", None, &[], &[]);
        add_crate(dir.path(), "demo-tools", None, &[], &[]);
        // `demo` depends on `demo-common` (internal) and dev-depends on `demo-tools` (excluded by
        // default). None of these names carry a `liberado-` prefix — membership decides, not the name.
        add_crate(
            dir.path(),
            "demo",
            Some("kernel"),
            &["demo-common"],
            &["demo-tools"],
        );

        let nodes = scan_repository(dir.path(), "liberado").unwrap();
        assert_eq!(nodes.len(), 3);
        let n = nodes.iter().find(|n| n.id == "demo").unwrap();
        assert_eq!(n.layer, Layer::from("kernel"));
        assert_eq!(n.description, "demo crate");
        assert_eq!(n.deps, vec!["demo-common".to_string()]);
        // A member with no role maps to "unknown" and is still present.
        assert!(
            nodes
                .iter()
                .any(|n| n.id == "demo-common" && n.layer == Layer::unknown())
        );
    }

    #[test]
    fn include_dev_adds_dev_dependencies() {
        let dir = tempdir().unwrap();
        write_workspace(dir.path());
        add_crate(dir.path(), "demo-common", None, &[], &[]);
        add_crate(dir.path(), "demo", Some("kernel"), &[], &["demo-common"]);

        let nodes = scan_repository_with(
            dir.path(),
            "liberado",
            ScanOptions {
                include_dev: true,
                include_build: false,
            },
        )
        .unwrap();
        let n = nodes.iter().find(|n| n.id == "demo").unwrap();
        assert_eq!(n.deps, vec!["demo-common".to_string()]);
    }
}
