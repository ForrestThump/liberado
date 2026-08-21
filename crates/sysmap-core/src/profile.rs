//! The declared profile a map is built from: the layer/kind vocabulary plus the runtime wiring
//! that cargo and the project's own config cannot derive. This is the `sysmap.toml` schema —
//! project-agnostic; a project supplies one (see `crates/sysmap/sysmap.toml` for the Liberado
//! profile and `docs/future-work/sysmap-generic-core-plan.md` for the template).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::model::{EdgeKind, Layer, MapEdge, MapNode, NodeKind};
use crate::vocab::{KindSpec, LayerSpec, Vocabulary};

/// Edge direction relative to the matched node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Edge leaves the matched node (`matched → to`).
    #[default]
    Out,
    /// Edge arrives at the matched node (`to → matched`).
    In,
}

/// A declared extra node — a runtime instance not emitted by cargo or the project's own adapter.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DeclaredNode {
    pub id: String,
    pub label: String,
    /// Node-kind id (e.g. "vault", "mcp").
    pub kind: String,
    /// Layer id the node is grouped near (informational).
    #[serde(default)]
    pub layer: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// A static runtime edge, emitted once regardless of the node set.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DeclaredEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    #[serde(default)]
    pub label: String,
}

/// A runtime-edge rule applied to every matching node. `to` may use `{meta.KEY}` placeholders,
/// resolved from the matched node's metadata (a missing key means the rule does not fire).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Rule {
    /// Selector like `kind=mcp`, `layer=store`, or `id=vault`.
    pub when: String,
    /// Meta key → value predicates; all must match.
    #[serde(default)]
    pub if_meta: BTreeMap<String, String>,
    /// Meta keys that must be present (for `{meta.KEY}` in `to` and "routes through" rules).
    #[serde(default)]
    pub require_meta: Vec<String>,
    /// Target id template (`vault`, `pool:{meta.pool}`, …).
    pub to: String,
    #[serde(default)]
    pub dir: Direction,
    pub kind: EdgeKind,
    #[serde(default)]
    pub label: String,
}

/// The whole `sysmap.toml` profile.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Profile {
    /// The `[package.metadata.<ns>]` namespace that carries each crate's `role` and `flows`.
    #[serde(default = "default_namespace")]
    pub manifest_namespace: String,
    /// Layer vocabulary (main-stack layers first, then meta layers).
    #[serde(default)]
    pub layers: Vec<LayerSpec>,
    /// Runtime node-kind vocabulary, in runtime-district order.
    #[serde(default)]
    pub kinds: Vec<KindSpec>,
    /// Extra nodes declared here (in addition to cargo crates and adapter-emitted nodes).
    #[serde(default)]
    pub nodes: Vec<DeclaredNode>,
    /// Static runtime edges.
    #[serde(default)]
    pub edges: Vec<DeclaredEdge>,
    /// Per-node edge rules (every matching rule fires).
    #[serde(default)]
    pub edge_rules: Vec<Rule>,
    /// Per-node routes (the first matching route fires; order specific-first).
    #[serde(default)]
    pub routes: Vec<Rule>,
}

fn default_namespace() -> String {
    "sysmap".to_string()
}

fn default_enabled() -> bool {
    true
}

impl Profile {
    pub fn from_toml_str(s: &str) -> Result<Self, String> {
        toml::from_str(s).map_err(|e| e.to_string())
    }

    /// The render-time vocabulary (layers + kinds) this profile declares.
    pub fn vocabulary(&self) -> Vocabulary {
        Vocabulary {
            layers: self.layers.clone(),
            kinds: self.kinds.clone(),
        }
    }

    /// Extra declared nodes as [`MapNode`]s.
    pub fn map_nodes(&self) -> Vec<MapNode> {
        self.nodes
            .iter()
            .map(|n| MapNode {
                id: n.id.clone(),
                label: n.label.clone(),
                kind: NodeKind::from(n.kind.as_str()),
                layer: if n.layer.is_empty() {
                    Layer::unknown()
                } else {
                    Layer::from(n.layer.as_str())
                },
                description: n.description.clone(),
                deps: Vec::new(),
                dev_deps: Vec::new(),
                build_deps: Vec::new(),
                flows: Vec::new(),
                meta: n.meta.clone(),
                enabled: n.enabled,
            })
            .collect()
    }

    /// Apply the declared wiring to a node set, returning runtime edges. Endpoint existence is
    /// *not* checked here — the caller drops dangling edges after assembly.
    pub fn apply(&self, nodes: &[MapNode]) -> Vec<MapEdge> {
        let mut out = Vec::new();

        for e in &self.edges {
            out.push(MapEdge {
                from: e.from.clone(),
                to: e.to.clone(),
                kind: e.kind,
                label: e.label.clone(),
            });
        }

        for node in nodes {
            if !node.enabled {
                continue;
            }
            for rule in &self.edge_rules {
                if let Some(edge) = rule.edge(node) {
                    out.push(edge);
                }
            }
            for route in &self.routes {
                if let Some(edge) = route.edge(node) {
                    out.push(edge);
                    break; // first matching route wins
                }
            }
        }

        out
    }
}

impl Rule {
    /// Build the edge for a node if this rule matches it.
    fn edge(&self, node: &MapNode) -> Option<MapEdge> {
        if !selector_matches(&self.when, node) {
            return None;
        }
        for (key, value) in &self.if_meta {
            if node.meta.get(key).map(String::as_str) != Some(value.as_str()) {
                return None;
            }
        }
        for key in &self.require_meta {
            if !node.meta.contains_key(key) {
                return None;
            }
        }
        let to = substitute(&self.to, node)?;
        let (from, to) = match self.dir {
            Direction::Out => (node.id.clone(), to),
            Direction::In => (to, node.id.clone()),
        };
        Some(MapEdge {
            from,
            to,
            kind: self.kind,
            label: self.label.clone(),
        })
    }
}

/// Match a `when` selector (`kind=mcp`, `layer=store`, `id=vault`) against a node.
fn selector_matches(when: &str, node: &MapNode) -> bool {
    let Some((key, value)) = when.split_once('=') else {
        return false;
    };
    match key {
        "kind" => node.kind.as_str() == value,
        "layer" => node.layer.as_str() == value,
        "id" => node.id == value,
        _ => false,
    }
}

/// Replace `{meta.KEY}` placeholders in `to` with the node's metadata. Returns `None` if a
/// placeholder is unknown or its key is absent (the rule then does not fire).
fn substitute(template: &str, node: &MapNode) -> Option<String> {
    let mut out = String::new();
    let mut rest = template;
    loop {
        let Some(start) = rest.find('{') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return None; // unterminated placeholder
        };
        let key = &after[..end];
        let Some(meta_key) = key.strip_prefix("meta.") else {
            return None; // unknown placeholder kind
        };
        out.push_str(node.meta.get(meta_key)?);
        rest = &after[end + 1..];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, kind: &str, meta: &[(&str, &str)]) -> MapNode {
        MapNode {
            id: id.to_string(),
            label: id.to_string(),
            kind: kind.into(),
            layer: Layer::unknown(),
            description: String::new(),
            deps: Vec::new(),
            dev_deps: Vec::new(),
            build_deps: Vec::new(),
            flows: Vec::new(),
            meta: meta
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            enabled: true,
        }
    }

    #[test]
    fn selector_matches_kind_layer_and_id() {
        let n = node("vault", "vault", &[]);
        assert!(selector_matches("kind=vault", &n));
        assert!(!selector_matches("kind=mcp", &n));
        assert!(selector_matches("id=vault", &n));
        assert!(!selector_matches("layer=store", &n));
    }

    #[test]
    fn substitute_resolves_meta_placeholders_and_rejects_missing() {
        let n = node("schedule:nightly", "schedule", &[("pool", "main")]);
        assert_eq!(
            substitute("pool:{meta.pool}", &n).as_deref(),
            Some("pool:main")
        );
        assert_eq!(substitute("pool:{meta.nope}", &n), None);
    }

    #[test]
    fn rule_fires_with_direction_and_meta_predicate() {
        let rule = Rule {
            when: "kind=schedule".into(),
            if_meta: BTreeMap::new(),
            require_meta: vec!["pool".into()],
            to: "pool:{meta.pool}".into(),
            dir: Direction::Out,
            kind: EdgeKind::Control,
            label: "routes through".into(),
        };
        let with_pool = node("schedule:a", "schedule", &[("pool", "main")]);
        let edge = rule.edge(&with_pool).unwrap();
        assert_eq!(
            (edge.from.as_str(), edge.to.as_str()),
            ("schedule:a", "pool:main")
        );
        assert_eq!(edge.kind, EdgeKind::Control);

        let without_pool = node("schedule:b", "schedule", &[]);
        assert_eq!(rule.edge(&without_pool), None);

        let disabled = MapNode {
            enabled: false,
            ..node("schedule:c", "schedule", &[("pool", "main")])
        };
        // `apply` skips disabled nodes; the rule itself still matches (enabled is checked there).
        assert!(rule.edge(&disabled).is_some());
    }

    #[test]
    fn routes_are_first_match_wins() {
        let profile = Profile {
            manifest_namespace: "liberado".into(),
            layers: vec![],
            kinds: vec![],
            nodes: vec![],
            edges: vec![],
            edge_rules: vec![],
            routes: vec![
                Rule {
                    when: "kind=profile".into(),
                    if_meta: BTreeMap::from([("domain".into(), "coding".into())]),
                    require_meta: vec![],
                    to: "liberado-coder-agent".into(),
                    dir: Direction::Out,
                    kind: EdgeKind::Control,
                    label: "domain pack".into(),
                },
                Rule {
                    when: "kind=profile".into(),
                    if_meta: BTreeMap::new(),
                    require_meta: vec![],
                    to: "liberado-dispatch-pack".into(),
                    dir: Direction::Out,
                    kind: EdgeKind::Control,
                    label: "domain pack".into(),
                },
            ],
        };

        let coding = node("profile:coding", "profile", &[("domain", "coding")]);
        let other = node("profile:other", "profile", &[("domain", "chat")]);
        let edges = profile.apply(&[coding.clone(), other.clone()]);
        assert!(
            edges
                .iter()
                .any(|e| e.from == "profile:coding" && e.to == "liberado-coder-agent")
        );
        assert!(
            edges
                .iter()
                .any(|e| e.from == "profile:other" && e.to == "liberado-dispatch-pack")
        );
        // Exactly one edge per profile node (first match wins, not both).
        assert_eq!(edges.len(), 2);
    }
}
