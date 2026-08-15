//! Deterministic isometric layout: assigns each node a world-space footprint and height.
//!
//! The layout is a pure function of the node set (and the dependency fan-in/fan-out used for
//! heights), so regenerating the map from changed manifests yields an identical arrangement for an
//! identical graph. Nothing here depends on screen size; the viewport scale/offset live in the GUI.

use std::collections::BTreeMap;

use crate::model::{EdgeKind, Layer, NodeKind, SystemMap};

/// World-space spacing between building centers along both axes. Exported so the GUI's ground-grid
/// can use the same step as the layout (single source of truth).
pub const GRID_STEP: f32 = 2.4;
/// World-space spacing between building centers along the east-west axis.
const CELL_DX: f32 = GRID_STEP;
/// World-space spacing between layer rows along the north-south axis.
const LAYER_DY: f32 = GRID_STEP;
/// Base footprint half-extent of a crate building (world units).
const FOOT_HALF: f32 = 0.56;

/// The "flow" layers that stack bottom-up in the main district. Tooling/testing/unknown live in a
/// side district; runtime nodes in another.
const MAIN_STACK: [Layer; 8] = [
    Layer::Foundation,
    Layer::Client,
    Layer::Kernel,
    Layer::Store,
    Layer::Pack,
    Layer::Service,
    Layer::Surface,
    Layer::Root,
];

/// A node placed in world space (before projection).
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedNode {
    pub id: String,
    /// World x (east-west; +x renders down-right).
    pub wx: f32,
    /// World y (north-south; +y renders down-left).
    pub wy: f32,
    /// Building height in world units.
    pub height: f32,
    /// Half-extent of the footprint (uniform for crates; runtime nodes use their own).
    pub half: f32,
}

/// The full layout: every node placed.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub placed: Vec<PlacedNode>,
}

fn fan_in_out(map: &SystemMap) -> BTreeMap<String, (usize, usize)> {
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for n in &map.nodes {
        counts.entry(n.id.clone()).or_insert((0, 0));
    }
    for e in &map.edges {
        if e.kind != EdgeKind::Dependency {
            continue;
        }
        counts.entry(e.from.clone()).or_insert((0, 0)).1 += 1; // fan-out
        counts.entry(e.to.clone()).or_insert((0, 0)).0 += 1; // fan-in
    }
    counts
}

fn crate_height(fan_in: usize, fan_out: usize) -> f32 {
    // Hub crates (many dependents / dependencies) rise higher than leaves.
    let weight = (fan_in + fan_out).min(12) as f32;
    0.55 + weight * 0.22
}

fn kind_rank(kind: NodeKind) -> usize {
    match kind {
        NodeKind::Vault => 0,
        NodeKind::Provider => 1,
        NodeKind::Mcp => 2,
        NodeKind::Pool => 3,
        NodeKind::Profile => 4,
        NodeKind::Project => 5,
        NodeKind::Schedule => 6,
        NodeKind::Hook => 7,
        NodeKind::Notifier => 8,
        NodeKind::Crate => usize::MAX,
    }
}

fn runtime_height(kind: NodeKind) -> f32 {
    match kind {
        NodeKind::Vault => 1.4,
        NodeKind::Provider => 1.2,
        NodeKind::Mcp => 0.95,
        NodeKind::Notifier => 0.85,
        NodeKind::Pool
        | NodeKind::Profile
        | NodeKind::Project
        | NodeKind::Schedule
        | NodeKind::Hook => 0.7,
        NodeKind::Crate => 0.55,
    }
}

/// Compute the layout for a map.
pub fn layout(map: &SystemMap) -> Layout {
    let fan = fan_in_out(map);

    // Partition nodes into the three districts.
    let mut main: Vec<&crate::model::MapNode> = Vec::new();
    let mut meta: Vec<&crate::model::MapNode> = Vec::new();
    let mut runtime: Vec<&crate::model::MapNode> = Vec::new();

    for n in &map.nodes {
        match n.kind {
            NodeKind::Crate => {
                if MAIN_STACK.contains(&n.layer) {
                    main.push(n);
                } else {
                    meta.push(n);
                }
            }
            _ => runtime.push(n),
        }
    }

    // Main district: one row per MAIN_STACK layer, bottom (Foundation) → top (Root).
    let mut placed = Vec::new();
    let mut widest = 0usize;
    for (rank, layer) in MAIN_STACK.iter().enumerate() {
        let mut row: Vec<&crate::model::MapNode> =
            main.iter().copied().filter(|n| n.layer == *layer).collect();
        row.sort_by(|a, b| a.id.cmp(&b.id));
        widest = widest.max(row.len());
        for (col, node) in row.iter().enumerate() {
            let (fan_in, fan_out) = fan.get(&node.id).copied().unwrap_or((0, 0));
            placed.push(PlacedNode {
                id: node.id.clone(),
                wx: col as f32 * CELL_DX,
                wy: -(rank as f32) * LAYER_DY,
                height: crate_height(fan_in, fan_out),
                half: FOOT_HALF,
            });
        }
    }

    // Meta district: tooling/testing/unknown crates, to the west of the main stack.
    meta.sort_by(|a, b| a.id.cmp(&b.id));
    let meta_cols = 3usize;
    for (i, node) in meta.iter().enumerate() {
        let col = i % meta_cols;
        let row = i / meta_cols;
        let (fan_in, fan_out) = fan.get(&node.id).copied().unwrap_or((0, 0));
        placed.push(PlacedNode {
            id: node.id.clone(),
            wx: -(col as f32 + 3.0) * CELL_DX,
            wy: -(row as f32) * LAYER_DY,
            height: crate_height(fan_in, fan_out),
            half: FOOT_HALF,
        });
    }

    // Runtime district: to the east of the main stack, grouped by kind.
    runtime.sort_by(|a, b| {
        kind_rank(a.kind)
            .cmp(&kind_rank(b.kind))
            .then_with(|| a.id.cmp(&b.id))
    });
    let runtime_cols = 6usize;
    let east_start = (widest.max(1) as f32 + 3.0) * CELL_DX;
    for (i, node) in runtime.iter().enumerate() {
        let col = i % runtime_cols;
        let row = i / runtime_cols;
        placed.push(PlacedNode {
            id: node.id.clone(),
            wx: east_start + col as f32 * CELL_DX,
            wy: -(row as f32) * LAYER_DY,
            height: runtime_height(node.kind),
            half: FOOT_HALF * 0.9,
        });
    }

    placed.sort_by(|a, b| a.id.cmp(&b.id));
    Layout { placed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MapNode, NodeKind};

    fn node(id: &str, layer: Layer, kind: NodeKind) -> MapNode {
        MapNode {
            id: id.to_string(),
            label: id.to_string(),
            kind,
            layer,
            description: String::new(),
            deps: Vec::new(),
            flows: Vec::new(),
            meta: BTreeMap::new(),
            enabled: true,
        }
    }

    #[test]
    fn layout_is_deterministic() {
        let map = SystemMap {
            generated_at: "t".into(),
            repository_root: "r".into(),
            config_dir: None,
            nodes: vec![
                node("liberado-common", Layer::Foundation, NodeKind::Crate),
                node("liberado-server", Layer::Root, NodeKind::Crate),
                node("mcp:tasks", Layer::Service, NodeKind::Mcp),
            ],
            edges: vec![],
        };
        let a = layout(&map);
        let b = layout(&map);
        assert_eq!(a, b);
    }

    #[test]
    fn main_stack_runs_foundation_below_root() {
        let map = SystemMap {
            generated_at: "t".into(),
            repository_root: "r".into(),
            config_dir: None,
            nodes: vec![
                node("liberado-common", Layer::Foundation, NodeKind::Crate),
                node("liberado-server", Layer::Root, NodeKind::Crate),
            ],
            edges: vec![],
        };
        let l = layout(&map);
        let pos = |id: &str| l.placed.iter().find(|p| p.id == id).unwrap();
        // Root sits "north" of foundation: its world-y is smaller (more negative).
        assert!(pos("liberado-server").wy < pos("liberado-common").wy);
    }

    #[test]
    fn hubs_rise_higher_than_leaves() {
        assert!(crate_height(0, 0) < crate_height(10, 10));
    }
}
