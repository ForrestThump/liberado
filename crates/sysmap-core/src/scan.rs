//! Scans a cargo workspace into crate [`MapNode`]s via `cargo metadata`. Workspace membership
//! decides what counts as an *internal* dependency (no name-prefix heuristic); the declared `role`
//! and `flows` come from each package's `[package.metadata.<namespace>]`, where the namespace is a
//! profile setting — so this crate stays reusable across projects.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, Package};

use crate::model::{DeclaredFlow, EdgeKind, Layer, MapNode, NodeKind};

/// An error while scanning the workspace.
#[derive(Debug)]
pub enum ScanError {
    /// `cargo metadata` failed (missing cargo, malformed workspace, …).
    Cargo { source: cargo_metadata::Error },
    /// A caller supplied malformed `cargo metadata` JSON.
    MetadataJson { source: serde_json::Error },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl ScanError {
    fn message(&self) -> String {
        match self {
            ScanError::Cargo { source } => format!("cargo metadata: {source}"),
            ScanError::MetadataJson { source } => format!("cargo metadata JSON: {source}"),
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
    let metadata = MetadataCommand::default()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .map_err(|e| ScanError::Cargo { source: e })?;

    Ok(scan_metadata(&metadata, namespace, opts))
}

/// Scan metadata supplied by a caller that already ran `cargo metadata`.
///
/// This is the subprocess-free composition seam for IDEs, CI tools, and alternate front ends.
pub fn scan_metadata(metadata: &Metadata, namespace: &str, opts: ScanOptions) -> Vec<MapNode> {
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
    nodes
}

/// Parse and scan a `cargo metadata --format-version 1 --no-deps` JSON document.
pub fn scan_metadata_json(json: &str, namespace: &str, opts: ScanOptions) -> Result<Vec<MapNode>> {
    let metadata =
        serde_json::from_str(json).map_err(|source| ScanError::MetadataJson { source })?;
    Ok(scan_metadata(&metadata, namespace, opts))
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

    let deps = internal_dependencies(package, members, DependencyKind::Normal, true);
    let dev_deps = internal_dependencies(
        package,
        members,
        DependencyKind::Development,
        opts.include_dev,
    );
    let build_deps =
        internal_dependencies(package, members, DependencyKind::Build, opts.include_build);

    let mut meta = BTreeMap::from([("version".to_string(), package.version.to_string())]);
    insert_optional(&mut meta, "license", package.license.as_deref());
    insert_list(&mut meta, "keywords", &package.keywords);
    insert_list(&mut meta, "categories", &package.categories);
    let mut targets: Vec<String> = package
        .targets
        .iter()
        .flat_map(|target| target.kind.iter())
        .map(|kind| format!("{kind:?}").to_ascii_lowercase())
        .collect();
    targets.sort();
    targets.dedup();
    insert_list(&mut meta, "targets", &targets);

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
        dev_deps,
        build_deps,
        flows: parse_flows(package, namespace),
        meta,
        enabled: true,
    })
}

fn internal_dependencies(
    package: &Package,
    members: &BTreeSet<String>,
    kind: DependencyKind,
    include: bool,
) -> Vec<String> {
    if !include {
        return Vec::new();
    }
    let mut dependencies: Vec<String> = package
        .dependencies
        .iter()
        .filter(|dependency| dependency.kind == kind)
        .filter(|dependency| members.contains(dependency.name.as_str()))
        .map(|dependency| dependency.name.clone())
        .collect();
    dependencies.sort();
    dependencies.dedup();
    dependencies
}

fn insert_optional(meta: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        meta.insert(key.to_string(), value.to_string());
    }
}

fn insert_list(meta: &mut BTreeMap<String, String>, key: &str, values: &[String]) {
    if !values.is_empty() {
        meta.insert(key.to_string(), values.join(", "));
    }
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
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"MIT\"\nkeywords = [\"agent\"]\ncategories = [\"development-tools\"]\ndescription = \"{name} crate\"\n{role_toml}[dependencies]\n{deps_toml}{dev_deps_toml}"
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
        assert_eq!(n.meta["version"], "0.1.0");
        assert_eq!(n.meta["license"], "MIT");
        assert_eq!(n.meta["keywords"], "agent");
        assert_eq!(n.meta["categories"], "development-tools");
        assert_eq!(n.meta["targets"], "lib");
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
        assert_eq!(n.dev_deps, vec!["demo-common".to_string()]);
    }

    #[test]
    fn metadata_json_seam_matches_direct_metadata_scan() {
        let dir = tempdir().unwrap();
        write_workspace(dir.path());
        add_crate(dir.path(), "demo", Some("kernel"), &[], &[]);
        let metadata = MetadataCommand::default()
            .manifest_path(dir.path().join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap();
        let json = serde_json::to_string(&metadata).unwrap();

        assert_eq!(
            scan_metadata_json(&json, "liberado", ScanOptions::default()).unwrap(),
            scan_metadata(&metadata, "liberado", ScanOptions::default())
        );
    }
}
