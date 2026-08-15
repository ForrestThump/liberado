//! The runtime control/data paths — the "payloads" half of the map.
//!
//! Dependency edges come from `Cargo.toml` (build-time). These edges are the *runtime* flows that
//! the dependency graph alone does not convey: who sends what to whom when the system runs. They
//! are curated from `docs/spec/architecture/overview.md` (the perceive→decide→act→loop-break loop,
//! the "star around one daemon" process shape) and `docs/spec/architecture/contracts.md` (the
//! narrow-waist seams), not invented. Each edge carries a label naming the payload.
//!
//! Edge ids reference either a crate name (`liberado-daemon`) or a runtime node id (`vault`,
//! `provider:deepseek`, `mcp:tasks-mcp`, …). Edges whose endpoint does not exist (e.g. a hook when
//! no topology is loaded) are dropped at assembly time, so the map degrades gracefully.

use liberado_config_loader::Topology;

use crate::model::{EdgeKind, MapEdge};
use crate::scan::mcp_writes_vault;

/// The canonical crate-to-crate runtime flows, in the order they execute.
///
/// Source: `docs/spec/architecture/overview.md` §"The loop" and §"Cross-cutting concepts".
const CRATE_FLOWS: &[(&str, &str, &str, EdgeKind)] = &[
    // ── The perceive → decide → act → don't-loop loop (overview.md mermaid) ──
    (
        "vault",
        "liberado-daemon",
        "external change",
        EdgeKind::Data,
    ),
    (
        "liberado-daemon",
        "liberado-dispatcher",
        "Execute / Subagent / Clarify",
        EdgeKind::Control,
    ),
    (
        "liberado-dispatcher",
        "liberado-orchestrator",
        "decision → Task + provenance",
        EdgeKind::Control,
    ),
    (
        "liberado-orchestrator",
        "liberado-executor",
        "tool calls",
        EdgeKind::Control,
    ),
    (
        "liberado-executor",
        "liberado-mcp",
        "invoke ToolRuntime",
        EdgeKind::Control,
    ),
    (
        "liberado-mcp",
        "vault",
        "writes carry provenance (_meta)",
        EdgeKind::Data,
    ),
    (
        "liberado-daemon",
        "liberado-daemon",
        "suppress own write (loop-break)",
        EdgeKind::Control,
    ),
    // ── Event sources fan into one daemon channel (overview.md "star around one daemon") ──
    (
        "liberado-cron",
        "liberado-daemon",
        "timer event",
        EdgeKind::Data,
    ),
    (
        "liberado-server",
        "liberado-daemon",
        "inject event (event_sender)",
        EdgeKind::Control,
    ),
    // ── Surfaces are clients of the HTTP/SSE wire contract ──
    (
        "liberado-tui",
        "liberado-server",
        "HTTP/SSE",
        EdgeKind::Data,
    ),
    (
        "liberado-webui",
        "liberado-server",
        "HTTP/SSE",
        EdgeKind::Data,
    ),
    (
        "liberado-cli",
        "liberado-server",
        "HTTP/SSE",
        EdgeKind::Data,
    ),
    (
        "liberado-acp-bridge",
        "liberado-server",
        "HTTP/SSE",
        EdgeKind::Data,
    ),
    // ── Chat: the server turns a turn over to the main agent, which delegates ──
    (
        "liberado-server",
        "liberado-main-agent",
        "chat turn",
        EdgeKind::Control,
    ),
    (
        "liberado-main-agent",
        "liberado-dispatcher",
        "delegate",
        EdgeKind::Control,
    ),
    (
        "liberado-main-agent",
        "liberado-executor",
        "streaming loop",
        EdgeKind::Control,
    ),
    // ── Inference: anything that thinks reaches the Provider narrow waist ──
    (
        "liberado-dispatcher",
        "liberado-provider",
        "classify",
        EdgeKind::Data,
    ),
    (
        "liberado-orchestrator",
        "liberado-provider",
        "worker completion",
        EdgeKind::Data,
    ),
    (
        "liberado-executor",
        "liberado-provider",
        "agent-loop completion",
        EdgeKind::Data,
    ),
    (
        "liberado-main-agent",
        "liberado-provider",
        "chat completion",
        EdgeKind::Data,
    ),
    // ── Notification: the engine notifies humans, even unattended ──
    (
        "liberado-orchestrator",
        "liberado-notify",
        "notify proposal",
        EdgeKind::Control,
    ),
    (
        "liberado-executor",
        "liberado-notify",
        "risk-gated notify",
        EdgeKind::Control,
    ),
    (
        "liberado-notify",
        "notifier:telegram",
        "send",
        EdgeKind::Data,
    ),
    // ── Coding pack rides the same kernel (dispatch-pack runs dispatcher+orchestrator) ──
    (
        "liberado-dispatch-pack",
        "liberado-dispatcher",
        "goal dispatch",
        EdgeKind::Control,
    ),
    (
        "liberado-dispatch-pack",
        "liberado-orchestrator",
        "run execution",
        EdgeKind::Control,
    ),
    (
        "liberado-coder-agent",
        "liberado-executor",
        "coding session loop",
        EdgeKind::Control,
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
