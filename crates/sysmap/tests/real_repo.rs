//! Integration tests that exercise the scanner and layout against the *real* workspace, not a
//! fixture. These pin the regeneration contract: the map is produced from the repository on disk,
//! every node is placed exactly once, and every edge endpoint resolves.

use std::collections::BTreeSet;
use std::path::PathBuf;

use liberado_sysmap::{EdgeKind, build, layout::layout};

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/sysmap → repo root")
        .to_path_buf()
}

#[test]
fn real_workspace_map_is_well_formed() {
    let root = repo_root();
    let map = build(&root, None).expect("scan real repo");
    assert!(
        map.nodes.len() >= 46,
        "expected the full workspace, found {} nodes",
        map.nodes.len()
    );

    let layout = layout(&map);
    assert_eq!(layout.placed.len(), map.nodes.len());

    let mut positions = BTreeSet::new();
    for p in &layout.placed {
        assert!(p.wx.is_finite() && p.wy.is_finite() && p.height.is_finite());
        assert!(p.height > 0.0);
        // No two buildings share a ground position.
        assert!(
            positions.insert((p.wx.to_bits(), p.wy.to_bits())),
            "duplicate ground position for {}",
            p.id
        );
    }

    let placed: BTreeSet<String> = layout.placed.iter().map(|p| p.id.clone()).collect();
    for e in &map.edges {
        assert!(
            placed.contains(&e.from),
            "edge from missing node {}",
            e.from
        );
        assert!(placed.contains(&e.to), "edge to missing node {}", e.to);
    }
}

#[test]
fn real_workspace_map_includes_runtime_overlay_from_example_topology() {
    let root = repo_root();
    let config = root.join("config.example");
    if !config.join("topology.toml").is_file() {
        return; // the example template is part of the repo; bail if a checkout lacks it
    }
    let map = build(&root, Some(&config)).expect("scan with example topology");
    assert!(map.nodes.iter().any(|n| n.id == "vault"));
    assert!(map.nodes.iter().any(|n| n.id == "provider:deepseek"));
    assert!(map.nodes.iter().any(|n| n.id == "mcp:tasks-mcp"));
    assert!(
        map.nodes
            .iter()
            .any(|n| n.id == "profile:coding-unattended")
    );
    assert!(map.edges.iter().any(|e| e.kind == EdgeKind::Control));
    assert!(map.edges.iter().any(|e| e.kind == EdgeKind::Data));
    assert!(map.edges.iter().any(|e| e.kind == EdgeKind::Dependency));
}
