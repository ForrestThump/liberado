//! The runtime control/data paths — the "payloads" half of the map.
//!
//! Dependency edges come from `Cargo.toml` (build-time). Runtime edges are the flows the
//! dependency graph alone does not convey: who sends what to whom when the system runs. They are
//! now **declared in the codebase**, not hardcoded here: each crate states its own outbound edges
//! under `[[package.metadata.liberado.flows]]` in its `Cargo.toml` (see `crates/sysmap/README.md`
//! and `model::DeclaredFlow`). The scanner reads those, so the map grows and evolves with the
//! codebase rather than with this tool.
//!
//! What remains here is the thin seed of edges whose *source* is infrastructure rather than a
//! crate, plus the topology-derived instance edges ([`topology_edges`]) that only exist once a
//! `topology.toml` declares them. Edge ids reference a crate name (`liberado-daemon`) or a runtime
//! node id (`vault`, `provider:deepseek`, `mcp:tasks-mcp`, …); edges whose endpoint does not exist
//! are dropped at assembly time, so the map degrades gracefully.

use liberado_config_loader::Topology;

use crate::model::{EdgeKind, MapEdge};
use crate::scan::mcp_writes_vault;

/// The seed runtime flows whose source is infrastructure, not a crate: `vault` is a runtime node,
/// not a workspace crate, so no crate can declare this edge about itself. Every other runtime flow
/// lives in its owning crate's `[[package.metadata.liberado.flows]]`.
const CRATE_FLOWS: &[(&str, &str, &str, EdgeKind)] = &[
    // The vault (external source of truth) changes; the daemon reacts.
    (
        "vault",
        "liberado-daemon",
        "external change",
        EdgeKind::Data,
    ),
];

/// The crate-to-crate runtime flows, as edges (endpoint existence is checked at assembly).
pub fn crate_runtime_edges() -> Vec<MapEdge> {
    CRATE_FLOWS
        .iter()
        .map(|(from, to, label, kind)| MapEdge {
            from: (*from).to_string(),
            to: (*to).to_string(),
            kind: *kind,
            label: (*label).to_string(),
        })
        .collect()
}

/// Topology-derived edges: connect the *instances* declared in `topology.toml` to the crates that
/// host them, and to each other where a payload actually moves.
pub fn topology_edges(topo: &Topology) -> Vec<MapEdge> {
    let mut edges = Vec::new();

    // The generic OpenAI-compatible backend crate serves every declared provider over HTTP.
    for p in &topo.providers {
        edges.push(MapEdge {
            from: "liberado-provider-openai-compat".to_string(),
            to: format!("provider:{}", p.name),
            kind: EdgeKind::Data,
            label: "chat-completions HTTP".to_string(),
        });
    }

    // Every enabled MCP is connected to by the liberado-mcp ToolRuntime.
    for m in &topo.mcps {
        if !m.enabled {
            continue;
        }
        edges.push(MapEdge {
            from: "liberado-mcp".to_string(),
            to: format!("mcp:{}", m.name),
            kind: EdgeKind::Control,
            label: "connect (spawn / http / docker)".to_string(),
        });
        if mcp_writes_vault(m) {
            edges.push(MapEdge {
                from: format!("mcp:{}", m.name),
                to: "vault".to_string(),
                kind: EdgeKind::Data,
                label: "zone write".to_string(),
            });
        }
    }

    // Pools are authority boundaries applied by the dispatcher.
    for pool in &topo.pools {
        if !pool.enabled {
            continue;
        }
        edges.push(MapEdge {
            from: format!("pool:{}", pool.name),
            to: "liberado-dispatcher".to_string(),
            kind: EdgeKind::Control,
            label: "authority boundary (caps)".to_string(),
        });
    }

    // Profiles name the domain pack that runs their sessions: coding → the coding pack, otherwise
    // the one-execution-engine dispatch pack.
    for profile in &topo.session_profiles {
        if !profile.enabled {
            continue;
        }
        let target = if crate::scan::profile_domain_is_coding(profile) {
            "liberado-coder-agent"
        } else {
            "liberado-dispatch-pack"
        };
        edges.push(MapEdge {
            from: format!("profile:{}", profile.name),
            to: target.to_string(),
            kind: EdgeKind::Control,
            label: "domain pack".to_string(),
        });
    }

    // Projects are the authorized roots the coding pack may touch.
    for project in &topo.projects {
        if !project.enabled {
            continue;
        }
        edges.push(MapEdge {
            from: "liberado-coder-agent".to_string(),
            to: format!("project:{}", project.name),
            kind: EdgeKind::Data,
            label: "authorized workspace root".to_string(),
        });
    }

    // Schedules declare timers the cron EventSource materializes; the fired event reaches the daemon.
    for schedule in &topo.schedules {
        if !schedule.enabled {
            continue;
        }
        edges.push(MapEdge {
            from: format!("schedule:{}", schedule.name),
            to: "liberado-cron".to_string(),
            kind: EdgeKind::Control,
            label: "cron_expr".to_string(),
        });
        if let Some(pool) = &schedule.pool {
            edges.push(MapEdge {
                from: format!("schedule:{}", schedule.name),
                to: format!("pool:{}", pool),
                kind: EdgeKind::Control,
                label: "routes through".to_string(),
            });
        }
    }

    // Hooks are the network-triggered counterpart: POST /api/hooks/{name} → server → daemon.
    for hook in &topo.hooks {
        if !hook.enabled {
            continue;
        }
        edges.push(MapEdge {
            from: format!("hook:{}", hook.name),
            to: "liberado-server".to_string(),
            kind: EdgeKind::Data,
            label: "POST /api/hooks/{name}".to_string(),
        });
        if let Some(pool) = &hook.pool {
            edges.push(MapEdge {
                from: format!("hook:{}", hook.name),
                to: format!("pool:{}", pool),
                kind: EdgeKind::Control,
                label: "routes through".to_string(),
            });
        }
    }

    edges
}
