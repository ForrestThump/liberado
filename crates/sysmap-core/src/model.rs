//! The serializable system-map model: nodes, edges, edge kinds, and the open layer/node-kind ids.
//!
//! Everything here derives `Serialize`/`Deserialize` so the whole map can be emitted as JSON and
//! re-rendered by any consumer without re-deriving it. The *vocabulary* (which layer ids exist,
//! their colors, blurbs, and layout order) lives in [`crate::vocab::Vocabulary`], not here — so
//! this crate stays reusable across projects with different architectures.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::vocab::Vocabulary;

/// An open architectural-layer id. Crates declare their role string in their manifest; the set of
/// known roles, their colors, blurbs, and layout order lives in a [`Vocabulary`]. A crate with no
/// role gets [`Layer::unknown`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Layer(String);

impl Layer {
    /// The id a crate with no declared role maps to.
    pub const UNKNOWN: &'static str = "unknown";

    pub fn unknown() -> Self {
        Layer(Self::UNKNOWN.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_unknown(&self) -> bool {
        self.0 == Self::UNKNOWN
    }
}

impl From<&str> for Layer {
    fn from(s: &str) -> Self {
        Layer(s.to_string())
    }
}

impl From<String> for Layer {
    fn from(s: String) -> Self {
        Layer(s)
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An open node-kind id for non-crate (runtime) nodes. Like layers, the vocabulary of known kinds
/// is external; crates use the fixed [`NodeKind::CRATE`] id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeKind(String);

impl NodeKind {
    /// The id every workspace crate carries.
    pub const CRATE: &'static str = "crate";

    pub fn crate_kind() -> Self {
        NodeKind(Self::CRATE.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_crate(&self) -> bool {
        self.0 == Self::CRATE
    }
}

impl From<&str> for NodeKind {
    fn from(s: &str) -> Self {
        NodeKind(s.to_string())
    }
}

impl From<String> for NodeKind {
    fn from(s: String) -> Self {
        NodeKind(s)
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What an edge represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// A build-time dependency (`[dependencies]` in a `Cargo.toml`).
    Dependency,
    /// Runtime control flow: one component *decides* that another should act.
    Control,
    /// Runtime data flow: payloads (requests, writes, events) moving between components.
    Data,
}

impl EdgeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Dependency => "dependency",
            EdgeKind::Control => "control",
            EdgeKind::Data => "data",
        }
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A runtime edge a crate declares about itself, under `[[package.metadata.<ns>.flows]]` in its
/// `Cargo.toml`. This is the *declarative* form of the runtime wiring: instead of the map tool
/// hardcoding who sends what to whom, each crate states its own outbound flows and the tool reads
/// them — so the map grows and evolves with the codebase, not with the tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclaredFlow {
    /// Target node id: a crate name (`liberado-daemon`) or a runtime id (`vault`, `mcp:<name>`, …).
    pub to: String,
    pub kind: EdgeKind,
    /// Payload label, e.g. "decision → Task + provenance".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// One node in the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapNode {
    /// Stable id. Crates use their crate name (`liberado-daemon`); runtime nodes use a
    /// kind-prefixed id (`provider:deepseek`, `mcp:tasks-mcp`, `vault`, `notifier:telegram`).
    pub id: String,
    /// Display label.
    pub label: String,
    pub kind: NodeKind,
    /// For crates, their declared role id; for runtime nodes, the layer they are grouped near
    /// (informational — runtime nodes are colored by kind, not layer).
    pub layer: Layer,
    /// Short description (crate `description`, or a derived summary for runtime nodes).
    pub description: String,
    /// Internal crate dependencies (crates only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
    /// Outbound runtime flows the crate declares about itself (see [`DeclaredFlow`]). When
    /// non-empty, these *replace* the built-in seed wiring for this crate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flows: Vec<DeclaredFlow>,
    /// Free-form metadata for the detail panel (transport kind, base URL, pool, cron expr, …).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, String>,
    /// Runtime nodes can be declared but disabled; disabled nodes render dimmed.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl MapNode {
    /// A compact one-line summary for tooltips.
    pub fn summary(&self) -> String {
        let kind = self.kind.as_str();
        if self.description.is_empty() {
            format!("{kind} · {}", self.label)
        } else {
            format!("{kind} · {} — {}", self.label, self.description)
        }
    }
}

/// One directed edge in the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapEdge {
    /// Node id the edge leaves.
    pub from: String,
    /// Node id the edge arrives at.
    pub to: String,
    pub kind: EdgeKind,
    /// Human label for the payload carried (runtime edges), e.g. "Execute / Subagent / Clarify".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// The assembled, layout-ready system map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemMap {
    /// When the map was generated (UTC, RFC 3339).
    pub generated_at: String,
    /// The repository root the crates were scanned from.
    pub repository_root: String,
    /// The config directory the topology was read from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    /// The layer/kind vocabulary this map is rendered with. Carried here so the JSON export is
    /// self-contained: any renderer can draw the legend and colors without project knowledge.
    pub vocabulary: Vocabulary,
    pub nodes: Vec<MapNode>,
    pub edges: Vec<MapEdge>,
}

impl SystemMap {
    pub fn node(&self, id: &str) -> Option<&MapNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Index of a node id, if present.
    pub fn node_index(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    /// Edges incident to a node, for neighbor highlighting.
    pub fn neighbors(&self, id: &str) -> Vec<&str> {
        let mut out = Vec::new();
        for e in &self.edges {
            if e.from == id {
                out.push(e.to.as_str());
            }
            if e.to == id {
                out.push(e.from.as_str());
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Number of workspace crates that directly depend on this node.
    ///
    /// Fan-out is intentionally excluded: this measures how load-bearing the node is, not how many
    /// services it consumes.
    pub fn dependency_fan_in(&self, id: &str) -> usize {
        self.edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Dependency && edge.to == id && edge.from != id)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_fan_in_excludes_outgoing_and_runtime_edges() {
        let map = SystemMap {
            generated_at: String::new(),
            repository_root: String::new(),
            config_dir: None,
            vocabulary: Vocabulary {
                layers: vec![],
                kinds: vec![],
            },
            nodes: vec![],
            edges: vec![
                MapEdge {
                    from: "a".into(),
                    to: "hub".into(),
                    kind: EdgeKind::Dependency,
                    label: String::new(),
                },
                MapEdge {
                    from: "b".into(),
                    to: "hub".into(),
                    kind: EdgeKind::Dependency,
                    label: String::new(),
                },
                MapEdge {
                    from: "hub".into(),
                    to: "leaf".into(),
                    kind: EdgeKind::Dependency,
                    label: String::new(),
                },
                MapEdge {
                    from: "runtime".into(),
                    to: "hub".into(),
                    kind: EdgeKind::Control,
                    label: String::new(),
                },
            ],
        };
        assert_eq!(map.dependency_fan_in("hub"), 2);
    }
}
