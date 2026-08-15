//! Assembles a [`SystemMap`] from a cargo workspace, a [`Profile`], and any extra (non-cargo)
//! nodes a project adapter supplies. This is the generic build pipeline: scan crates, apply the
//! declared flows and profile rules, and drop edges whose endpoints are absent.

use std::collections::BTreeSet;
use std::path::Path;

use crate::model::{EdgeKind, MapEdge, MapNode, SystemMap};
use crate::profile::Profile;
use crate::scan::{self, ScanError};

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
    let mut nodes = scan::scan_repository(root, &profile.manifest_namespace)?;
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

    fn add_crate(root: &Path, name: &str, role: &str, deps: &[&str]) {
        let dir = root.join("crates").join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/lib.rs"), "// test fixture\n").unwrap();
        let deps_toml = deps
            .iter()
            .map(|d| format!("{d} = {{ path = \"../{d}\" }}\n"))
            .collect::<String>();
        fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\ndescription = \"{name} crate\"\n[package.metadata.liberado]\nrole = \"{role}\"\n[dependencies]\n{deps_toml}"
            ),
        )
        .unwrap();
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
        add_crate(dir.path(), "liberado-common", "foundation", &[]);
        add_crate(dir.path(), "liberado-daemon", "root", &["liberado-common"]);

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
    fn drops_edges_to_missing_nodes() {
        let dir = tempdir().unwrap();
        write_workspace(dir.path());
        add_crate(dir.path(), "liberado-common", "foundation", &[]);

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
