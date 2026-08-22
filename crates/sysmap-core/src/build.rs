//! Assembles a [`SystemMap`] from a cargo workspace, a [`Profile`], and any extra (non-cargo)
//! nodes a project adapter supplies. This is the generic build pipeline: scan crates, apply the
//! declared flows and profile rules, and drop edges whose endpoints are absent.

use std::collections::BTreeSet;
use std::path::Path;

use crate::model::{EdgeKind, MapEdge, MapNode, SystemMap};
use crate::profile::Profile;
use crate::scan::{self, ScanError, ScanOptions};

/// Build a system map.
///
/// * `root` — the workspace root (its `Cargo.toml` must be the workspace manifest).
/// * `profile` — the declared vocabulary + wiring (`sysmap.toml`).
/// * `extra_nodes` — non-cargo nodes the project adapter emits (runtime instances).
/// * `config_dir` — optional provenance string recorded on the map (where extra config came from).
pub fn build(
    root: &Path,
    profile: &Profile,
    extra_nodes: Vec<MapNode>,
    config_dir: Option<String>,
) -> Result<SystemMap, ScanError> {
    let mut nodes = scan::scan_repository_with(
        root,
        &profile.manifest_namespace,
        ScanOptions {
            include_dev: true,
            include_build: true,
        },
    )?;
    nodes.extend(profile.map_nodes());
    nodes.extend(extra_nodes);

    // Deduplicate by id (a crate id can never collide with a `kind:`-prefixed runtime id).
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    nodes.dedup_by(|a, b| a.id == b.id);

    let existing: BTreeSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let mut edges: Vec<MapEdge> = Vec::new();

    // Build-time dependency edges (crates only).
    for node in &nodes {
        if !node.kind.is_crate() {
            continue;
        }
        push_dependency_edges(
            &mut edges,
            node,
            &node.deps,
            EdgeKind::Dependency,
            &existing,
        );
        push_dependency_edges(
            &mut edges,
            node,
            &node.dev_deps,
            EdgeKind::DevelopmentDependency,
            &existing,
        );
        push_dependency_edges(
            &mut edges,
            node,
            &node.build_deps,
            EdgeKind::BuildDependency,
            &existing,
        );
    }

    // Declared per-crate runtime flows (each crate states its own outbound wiring).
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

    // Declared runtime wiring from the profile: static edges + per-node rules + routes.
    edges.extend(profile.apply(&nodes));

    // Drop edges referencing missing nodes and deduplicate (from,to,kind).
    edges.retain(|e| existing.contains(&e.from) && existing.contains(&e.to));
    edges.sort_by(|a, b| {
        (&a.from, &a.to, &a.kind, &a.label).cmp(&(&b.from, &b.to, &b.kind, &b.label))
    });
    edges.dedup_by(|a, b| a.from == b.from && a.to == b.to && a.kind == b.kind);

    Ok(SystemMap {
        generated_at: chrono::Utc::now().to_rfc3339(),
        repository_root: root.to_string_lossy().into_owned(),
        config_dir,
        vocabulary: profile.vocabulary(),
        nodes,
        edges,
    })
}

fn push_dependency_edges(
    edges: &mut Vec<MapEdge>,
    node: &MapNode,
    dependencies: &[String],
    kind: EdgeKind,
    existing: &BTreeSet<String>,
) {
    for dependency in dependencies {
        if existing.contains(dependency) {
            edges.push(MapEdge {
                from: node.id.clone(),
                to: dependency.clone(),
                kind,
                label: String::new(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EdgeKind, Layer};
    use crate::profile::{DeclaredEdge, Profile};
    use std::fs;
    use tempfile::tempdir;

    fn write_workspace(root: &Path) {
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
    }

    fn add_crate(
        root: &Path,
        name: &str,
        role: &str,
        deps: &[&str],
        dev_deps: &[&str],
        build_deps: &[&str],
    ) {
        let dir = root.join("crates").join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/lib.rs"), "// test fixture\n").unwrap();
        let deps_toml = deps
            .iter()
            .map(|d| format!("{d} = {{ path = \"../{d}\" }}\n"))
            .collect::<String>();
        let dev_deps_toml = dependency_table("dev-dependencies", dev_deps);
        let build_deps_toml = dependency_table("build-dependencies", build_deps);
        fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\ndescription = \"{name} crate\"\n[package.metadata.liberado]\nrole = \"{role}\"\n[dependencies]\n{deps_toml}{dev_deps_toml}{build_deps_toml}"
            ),
        )
        .unwrap();
    }

    fn dependency_table(name: &str, dependencies: &[&str]) -> String {
        if dependencies.is_empty() {
            return String::new();
        }
        let entries = dependencies
            .iter()
            .map(|dependency| format!("{dependency} = {{ path = \"../{dependency}\" }}\n"))
            .collect::<String>();
        format!("[{name}]\n{entries}")
    }

    fn profile_with_edge() -> Profile {
        Profile {
            manifest_namespace: "liberado".into(),
            layers: vec![],
            kinds: vec![],
            nodes: vec![],
            edges: vec![DeclaredEdge {
                from: "liberado-daemon".into(),
                to: "liberado-common".into(),
                kind: EdgeKind::Control,
                label: "act".into(),
            }],
            edge_rules: vec![],
            routes: vec![],
        }
    }

    #[test]
    fn builds_crates_dependency_edges_and_profile_edges() {
        let dir = tempdir().unwrap();
        write_workspace(dir.path());
        add_crate(dir.path(), "liberado-common", "foundation", &[], &[], &[]);
        add_crate(
            dir.path(),
            "liberado-daemon",
            "root",
            &["liberado-common"],
            &[],
            &[],
        );

        let profile = profile_with_edge();
        let map = build(dir.path(), &profile, vec![], None).unwrap();

        assert_eq!(map.nodes.len(), 2);
        assert!(map.edges.iter().any(|e| e.from == "liberado-daemon"
            && e.to == "liberado-common"
            && e.kind == EdgeKind::Dependency));
        assert!(map.edges.iter().any(|e| e.from == "liberado-daemon"
            && e.to == "liberado-common"
            && e.kind == EdgeKind::Control
            && e.label == "act"));
    }

    #[test]
    fn builds_distinct_development_and_build_dependency_edges() {
        let dir = tempdir().unwrap();
        write_workspace(dir.path());
        add_crate(dir.path(), "common", "foundation", &[], &[], &[]);
        add_crate(
            dir.path(),
            "consumer",
            "kernel",
            &[],
            &["common"],
            &["common"],
        );

        let map = build(dir.path(), &profile_with_edge(), vec![], None).unwrap();
        assert!(map.edges.iter().any(|edge| {
            edge.from == "consumer"
                && edge.to == "common"
                && edge.kind == EdgeKind::DevelopmentDependency
        }));
        assert!(map.edges.iter().any(|edge| {
            edge.from == "consumer" && edge.to == "common" && edge.kind == EdgeKind::BuildDependency
        }));
    }

    #[test]
    fn drops_edges_to_missing_nodes() {
        let dir = tempdir().unwrap();
        write_workspace(dir.path());
        add_crate(dir.path(), "liberado-common", "foundation", &[], &[], &[]);

        let profile = profile_with_edge();
        // The profile edge's target (liberado-common) exists, but its source (liberado-daemon)
        // does not — so the edge must be dropped, and no dangling edges remain.
        let map = build(dir.path(), &profile, vec![], None).unwrap();
        assert!(
            map.edges
                .iter()
                .all(|e| map.node(&e.from).is_some() && map.node(&e.to).is_some())
        );
        assert_eq!(map.nodes[0].layer, Layer::from("foundation"));
    }
}
