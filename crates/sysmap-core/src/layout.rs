//! Deterministic layout: assigns each node a world-space footprint and height.
//!
//! The layout is a pure function of the graph and its [`Vocabulary`]. Nodes stay in their layer or
//! runtime-kind group. A deterministic multi-start pair-swap search then reduces edge crossings.
//! Nothing here depends on screen size; the viewport scale and node rendering live in the GUI.

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

fn crate_height(fan_in: usize) -> f32 {
    // This legacy geometry field now reflects load-bearing fan-in only. Renderers can map the same
    // signal to area instead of height.
    let weight = fan_in.min(12) as f32;
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
            let (fan_in, _) = fan.get(&node.id).copied().unwrap_or((0, 0));
            placed.push(PlacedNode {
                id: node.id.clone(),
                wx: col as f32 * CELL_DX,
                wy: -(rank as f32) * LAYER_DY,
                height: crate_height(fan_in),
                half: FOOT_HALF,
            });
        }
    }

    // Meta district: crates outside the main stack (tooling/testing/unknown and undeclared roles),
    // to the west of the main stack.
    meta.sort_by(|a, b| a.layer.cmp(&b.layer).then_with(|| a.id.cmp(&b.id)));
    let meta_cols = 3usize;
    for (i, node) in meta.iter().enumerate() {
        let col = i % meta_cols;
        let row = i / meta_cols;
        let (fan_in, _) = fan.get(&node.id).copied().unwrap_or((0, 0));
        placed.push(PlacedNode {
            id: node.id.clone(),
            wx: -(col as f32 + 3.0) * CELL_DX,
            wy: -(row as f32) * LAYER_DY,
            height: crate_height(fan_in),
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
    Layout {
        placed: minimize_crossings(map, placed),
    }
}

fn minimize_crossings(map: &SystemMap, initial: Vec<PlacedNode>) -> Vec<PlacedNode> {
    let groups = movable_groups(map, &initial);
    let edges = indexed_edges(map, &initial);
    let mut best = initial.clone();
    let mut best_score = score_indexed_edges(&best, &edges);

    for start in 0..4_u64 {
        let mut candidate = initial.clone();
        if start > 0 {
            shuffle_group_positions(&mut candidate, &groups, 0x9e37_79b9 ^ start);
        }
        improve_by_pair_swaps(&mut candidate, &groups, &edges);
        let score = score_indexed_edges(&candidate, &edges);
        if score < best_score {
            best = candidate;
            best_score = score;
        }
    }
    best
}

fn movable_groups(map: &SystemMap, placed: &[PlacedNode]) -> Vec<Vec<usize>> {
    let mut by_group: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, placed_node) in placed.iter().enumerate() {
        let Some(node) = map.node(&placed_node.id) else {
            continue;
        };
        let key = if node.kind.is_crate() {
            format!("layer:{}", node.layer)
        } else {
            format!("kind:{}", node.kind)
        };
        by_group.entry(key).or_default().push(index);
    }
    by_group
        .into_values()
        .filter(|group| group.len() > 1)
        .collect()
}

fn shuffle_group_positions(placed: &mut [PlacedNode], groups: &[Vec<usize>], mut state: u64) {
    for group in groups {
        for cursor in (1..group.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let other = state as usize % (cursor + 1);
            swap_positions(placed, group[cursor], group[other]);
        }
    }
}

fn improve_by_pair_swaps(
    placed: &mut [PlacedNode],
    groups: &[Vec<usize>],
    edges: &[(usize, usize)],
) {
    let mut score = score_indexed_edges(placed, edges);
    for _ in 0..3 {
        let mut improved = false;
        for group in groups {
            for left in 0..group.len() {
                for right in (left + 1)..group.len() {
                    swap_positions(placed, group[left], group[right]);
                    let candidate = score_indexed_edges(placed, edges);
                    if candidate < score {
                        score = candidate;
                        improved = true;
                    } else {
                        swap_positions(placed, group[left], group[right]);
                    }
                }
            }
        }
        if !improved {
            break;
        }
    }
}

fn swap_positions(placed: &mut [PlacedNode], left: usize, right: usize) {
    if left == right {
        return;
    }
    let (left_node, right_node) = if left < right {
        let (before, after) = placed.split_at_mut(right);
        (&mut before[left], &mut after[0])
    } else {
        let (before, after) = placed.split_at_mut(left);
        (&mut after[0], &mut before[right])
    };
    std::mem::swap(&mut left_node.wx, &mut right_node.wx);
    std::mem::swap(&mut left_node.wy, &mut right_node.wy);
}

fn indexed_edges(map: &SystemMap, placed: &[PlacedNode]) -> Vec<(usize, usize)> {
    let indices: BTreeMap<&str, usize> = placed
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect();
    map.edges
        .iter()
        .filter_map(|edge| {
            let from = *indices.get(edge.from.as_str())?;
            let to = *indices.get(edge.to.as_str())?;
            (from != to).then_some((from, to))
        })
        .collect()
}

fn score_indexed_edges(placed: &[PlacedNode], edges: &[(usize, usize)]) -> (usize, u64) {
    let mut crossings = 0;
    for left in 0..edges.len() {
        for right in (left + 1)..edges.len() {
            let (a1, a2) = edges[left];
            let (b1, b2) = edges[right];
            if a1 != b1
                && a1 != b2
                && a2 != b1
                && a2 != b2
                && segments_cross(
                    position(&placed[a1]),
                    position(&placed[a2]),
                    position(&placed[b1]),
                    position(&placed[b2]),
                )
            {
                crossings += 1;
            }
        }
    }
    let length = edges
        .iter()
        .map(|&(from, to)| {
            let a = position(&placed[from]);
            let b = position(&placed[to]);
            let dx = a.0 - b.0;
            let dy = a.1 - b.1;
            ((dx * dx + dy * dy) * 1000.0) as u64
        })
        .sum();
    (crossings, length)
}

#[cfg(test)]
fn layout_score(map: &SystemMap, placed: &[PlacedNode]) -> (usize, u64) {
    score_indexed_edges(placed, &indexed_edges(map, placed))
}

fn position(node: &PlacedNode) -> (f32, f32) {
    (node.wx, node.wy)
}

fn segments_cross(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: (f32, f32)) -> bool {
    fn side(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
        (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
    }
    let (c1, d1) = (side(a, b, c), side(a, b, d));
    let (a1, b1) = (side(c, d, a), side(c, d, b));
    c1 * d1 < 0.0 && a1 * b1 < 0.0
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
    fn fan_in_increases_load_bearing_signal_but_fan_out_does_not() {
        assert!(crate_height(0) < crate_height(10));
        let mut m = map(vec![
            node("dependent", "root", "crate"),
            node("dependency", "foundation", "crate"),
        ]);
        m.edges.push(crate::model::MapEdge {
            from: "dependent".into(),
            to: "dependency".into(),
            kind: EdgeKind::Dependency,
            label: String::new(),
        });
        let result = layout(&m, &m.vocabulary);
        let dependency = result
            .placed
            .iter()
            .find(|node| node.id == "dependency")
            .unwrap();
        let dependent = result
            .placed
            .iter()
            .find(|node| node.id == "dependent")
            .unwrap();
        assert!(dependency.height > dependent.height);
    }

    #[test]
    fn pair_swap_search_removes_a_simple_crossing_without_moving_layers() {
        let mut m = map(vec![
            node("a", "foundation", "crate"),
            node("b", "foundation", "crate"),
            node("x", "root", "crate"),
            node("y", "root", "crate"),
        ]);
        m.edges = vec![
            crate::model::MapEdge {
                from: "a".into(),
                to: "y".into(),
                kind: EdgeKind::Dependency,
                label: String::new(),
            },
            crate::model::MapEdge {
                from: "b".into(),
                to: "x".into(),
                kind: EdgeKind::Dependency,
                label: String::new(),
            },
        ];
        let result = layout(&m, &m.vocabulary);
        assert_eq!(layout_score(&m, &result.placed).0, 0);
        let layer_y = |id: &str| result.placed.iter().find(|node| node.id == id).unwrap().wy;
        assert_eq!(layer_y("a"), layer_y("b"));
        assert_eq!(layer_y("x"), layer_y("y"));
    }
}
