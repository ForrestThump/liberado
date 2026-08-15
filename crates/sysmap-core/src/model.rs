//! The serializable system-map model: nodes, edges, layers, and node kinds.
//!
//! Everything here derives `Serialize`/`Deserialize` so the whole map can be emitted as JSON
//! (`liberado-sysmap --write-json`) and re-rendered by any consumer without re-deriving it. That
//! is the regeneration contract: the map is a pure function of the repository's `Cargo.toml`
//! files and an optional `topology.toml`, never a hand-drawn artifact.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The architectural layer a crate belongs to (its `[package.metadata.liberado] role`).
///
/// Order follows the dependency direction: `Foundation` is the bottom, `Root` the top. The order
/// is load-bearing — the layout stacks layers in this order so a viewer reads the system bottom-up
/// the same way the dependency rules enforce it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Foundation,
    Client,
    Kernel,
    Store,
    Pack,
    Service,
    Surface,
    Root,
    Tooling,
    Testing,
    /// A crate with no declared role. Not part of [`Layer::ALL`]; rendered last so a missing
    /// `[package.metadata.liberado] role` is visible instead of silently re-homed.
    Unknown,
}

impl Layer {
    /// The role strings used in `[package.metadata.liberado] role`, in dependency order.
    pub const ALL: [Layer; 10] = [
        Layer::Foundation,
        Layer::Client,
        Layer::Kernel,
        Layer::Store,
        Layer::Pack,
        Layer::Service,
        Layer::Surface,
        Layer::Root,
        Layer::Tooling,
        Layer::Testing,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Layer::Foundation => "foundation",
            Layer::Client => "client",
            Layer::Kernel => "kernel",
            Layer::Store => "store",
            Layer::Pack => "pack",
            Layer::Service => "service",
            Layer::Surface => "surface",
            Layer::Root => "root",
            Layer::Tooling => "tooling",
            Layer::Testing => "testing",
            Layer::Unknown => "unknown",
        }
    }

    pub fn from_role_str(s: &str) -> Option<Layer> {
        Layer::ALL.iter().copied().find(|l| l.as_str() == s)
    }

    /// One-line explanation of what this layer is for, shown in the legend and explainer.
    pub const fn blurb(self) -> &'static str {
        match self {
            Layer::Foundation => "Vocabulary and narrow-waist traits; depends on nothing above.",
            Layer::Client => "Front-end building blocks, liftable into any UI.",
            Layer::Kernel => "The orchestration engine: decide/act loops, sessions, capability.",
            Layer::Store => {
                "Persistent and shared information: vault, conversations, memory, search."
            }
            Layer::Pack => "Domain packs (coding first); never beneath kernel/config/store.",
            Layer::Service => "Out-of-process adapters: MCP servers, bots, the forge.",
            Layer::Surface => "UIs — clients of the wire contract only.",
            Layer::Root => "Composition roots: the only crates allowed to see everything.",
            Layer::Tooling => "Meta tooling (evals, tuner, this map). Not a build dependency.",
            Layer::Testing => "Dev-dependency-only test support.",
            Layer::Unknown => "No declared role — should pick a layer.",
        }
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a node *is*. Crates are the build-time units; the rest are runtime instances declared in
/// `topology.toml` (or fixed infrastructure) that participate in the control/data paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A workspace crate (`crates/*`).
    Crate,
    /// An inference backend declared under `[[providers]]`.
    Provider,
    /// An MCP server declared under `[[mcps]]`.
    Mcp,
    /// A named dispatcher/executor pool under `[[pools]]`.
    Pool,
    /// A session profile under `[[session_profiles]]`.
    Profile,
    /// A coding project root under `[[projects]]`.
    Project,
    /// A cron schedule under `[[schedules]]`.
    Schedule,
    /// An external webhook under `[[hooks]]`.
    Hook,
    /// The Obsidian vault (source of truth).
    Vault,
    /// A human-facing notification channel (Telegram).
    Notifier,
}

impl NodeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            NodeKind::Crate => "crate",
            NodeKind::Provider => "provider",
            NodeKind::Mcp => "mcp",
            NodeKind::Pool => "pool",
            NodeKind::Profile => "profile",
            NodeKind::Project => "project",
            NodeKind::Schedule => "schedule",
            NodeKind::Hook => "hook",
            NodeKind::Vault => "vault",
            NodeKind::Notifier => "notifier",
        }
    }

    /// Human label for a node of this kind (used in the legend).
    pub const fn label(self) -> &'static str {
        match self {
            NodeKind::Crate => "Crate",
            NodeKind::Provider => "Provider",
            NodeKind::Mcp => "MCP server",
            NodeKind::Pool => "Pool",
            NodeKind::Profile => "Session profile",
            NodeKind::Project => "Coding project",
            NodeKind::Schedule => "Cron schedule",
            NodeKind::Hook => "Webhook",
            NodeKind::Vault => "Vault",
            NodeKind::Notifier => "Notifier",
        }
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

/// A runtime edge a crate declares about itself, under `[[package.metadata.liberado.flows]]` in
/// its `Cargo.toml`. This is the *declarative* form of the runtime wiring: instead of the map tool
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
    /// For crates, their declared role; for runtime nodes, the layer they are grouped near for
    /// coloring (see [`scan::runtime_layer`](crate::scan::runtime_layer)).
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
        let kind = self.kind.label();
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
}
