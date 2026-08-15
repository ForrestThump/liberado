//! Deterministic layout: assigns each node a world-space footprint and height.
//!
//! The layout is a pure function of the node set, the dependency fan-in/fan-out (heights), and the
//! map's [`Vocabulary`] (which layers stack, their order, and the runtime-kind ordering/heights).
//! Nothing here depends on screen size; the viewport scale/offset live in the GUI.

use std::collections::BTreeMap;

use crate::model::{EdgeKind, MapNode, NodeKind, SystemMap};
use crate::vocab::Vocabulary;

/// World-space spacing between building centers along both axes. Exported so the GUI's ground-grid
/// can use the same step as the layout (single source of truth).
pub const GRID_STEP: f32 = 2.4;
/// World-space spacing between building centers along the east-west axis.
const CELL_DX: f32 = GRID_STEP;
/// World-space spacing between layer rows along the north-south axis.
const LAYER_DY: f32 = GRID_STEP;
/// Base footprint half-extent of a crate building (world units).
const FOOT_HALF: f32 = 0.56;

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

/// Position of a runtime kind in the vocabulary's ordering (for the runtime district grouping).
fn kind_rank(vocab: &Vocabulary, kind: &NodeKind) -> usize {
    vocab
        .kinds
        .iter()
        .position(|k| k.id == kind.as_str())
        .unwrap_or(usize::MAX)
}

fn runtime_height(vocab: &Vocabulary, kind: &NodeKind) -> f32 {
    vocab.kind(kind.as_str()).map(|k| k.height).unwrap_or(0.7)
}

/// Compute the layout for a map.
pub fn layout(map: &SystemMap, vocab: &Vocabulary) -> Layout {
    let fan = fan_in_out(map);
    let main_layers: Vec<&str> = vocab.main_stack().map(|l| l.id.as_str()).collect();

    // Partition nodes into the three districts.
    let mut main: Vec<&MapNode> = Vec::new();
    let mut meta: Vec<&MapNode> = Vec::new();
    let mut runtime: Vec<&MapNode> = Vec::new();

    for n in &map.nodes {
        if n.kind.is_crate() {
            if main_layers.iter().any(|l| *l == n.layer.as_str()) {
                main.push(n);
            } else {
                meta.push(n);
            }
        } else {
            runtime.push(n);
        }
    }

    // Main district: one row per main-stack layer, bottom → top (in vocabulary order).
    let mut placed = Vec::new();
    let mut widest = 0usize;
    for (rank, layer_id) in main_layers.iter().enumerate() {
        let mut row: Vec<&MapNode> = main
            .iter()
            .copied()
            .filter(|n| n.layer.as_str() == *layer_id)
            .collect();
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

    // Meta district: crates outside the main stack (tooling/testing/unknown and undeclared roles),
    // to the west of the main stack.
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

    // Runtime district: to the east of the main stack, grouped by kind (vocabulary order).
    runtime.sort_by(|a, b| {
        kind_rank(vocab, &a.kind)
            .cmp(&kind_rank(vocab, &b.kind))
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
            height: runtime_height(vocab, &node.kind),
            half: FOOT_HALF * 0.9,
        });
    }

    placed.sort_by(|a, b| a.id.cmp(&b.id));
    Layout { placed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MapNode;
    use crate::vocab::{KindSpec, LayerSpec};

    fn vocab() -> Vocabulary {
        Vocabulary {
            layers: vec![
                LayerSpec {
                    id: "foundation".into(),
                    label: "foundation".into(),
                    color: "#8a94a6".into(),
                    blurb: String::new(),
                    main: true,
                },
                LayerSpec {
                    id: "root".into(),
                    label: "root".into(),
                    color: "#c0453a".into(),
                    blurb: String::new(),
                    main: true,
                },
                LayerSpec {
                    id: "tooling".into(),
                    label: "tooling".into(),
                    color: "#9ab82f".into(),
                    blurb: String::new(),
                    main: false,
                },
            ],
            kinds: vec![KindSpec {
                id: "mcp".into(),
                label: "MCP server".into(),
                color: "#6a8fd0".into(),
                blurb: String::new(),
                height: 0.95,
            }],
        }
    }

    fn node(id: &str, layer: &str, kind: &str) -> MapNode {
        MapNode {
            id: id.to_string(),
            label: id.to_string(),
            kind: kind.into(),
            layer: layer.into(),
            description: String::new(),
            deps: Vec::new(),
            flows: Vec::new(),
            meta: BTreeMap::new(),
            enabled: true,
        }
    }

    fn map(nodes: Vec<MapNode>) -> SystemMap {
        SystemMap {
            generated_at: "t".into(),
            repository_root: "r".into(),
            config_dir: None,
            vocabulary: vocab(),
            nodes,
            edges: vec![],
        }
    }

    #[test]
    fn layout_is_deterministic() {
        let m = map(vec![
            node("liberado-common", "foundation", "crate"),
            node("liberado-server", "root", "crate"),
            node("mcp:tasks", "service", "mcp"),
        ]);
        let a = layout(&m, &m.vocabulary);
        let b = layout(&m, &m.vocabulary);
        assert_eq!(a, b);
    }

    #[test]
    fn main_stack_runs_foundation_below_root() {
        let m = map(vec![
            node("liberado-common", "foundation", "crate"),
            node("liberado-server", "root", "crate"),
        ]);
        let l = layout(&m, &m.vocabulary);
        let pos = |id: &str| l.placed.iter().find(|p| p.id == id).unwrap();
        // Root sits "north" of foundation: its world-y is smaller (more negative).
        assert!(pos("liberado-server").wy < pos("liberado-common").wy);
    }

    #[test]
    fn hubs_rise_higher_than_leaves() {
        assert!(crate_height(0, 0) < crate_height(10, 10));
    }
}
