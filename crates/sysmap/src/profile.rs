//! Liberado's vocabulary: the layer and node-kind vocabulary this project renders the map with.
//!
//! This is the *profile* — the project-specific content that `sysmap-core` must not carry. It is
//! Rust data today; a later phase moves it into a `sysmap.toml` that the generic core reads (see
//! `docs/future-work/sysmap-generic-core-plan.md`). Nothing in `sysmap-core` depends on this.

use sysmap_core::vocab::{KindSpec, LayerSpec, Vocabulary};

/// The layer vocabulary: 8 main-stack layers (bottom-up), then the side-district layers.
pub fn liberado_vocabulary() -> Vocabulary {
    Vocabulary {
        layers: vec![
            LayerSpec {
                id: "foundation".into(),
                label: "foundation".into(),
                color: "#8a94a6".into(),
                blurb: "Vocabulary and narrow-waist traits; depends on nothing above.".into(),
                main: true,
            },
            LayerSpec {
                id: "client".into(),
                label: "client".into(),
                color: "#2fb8a0".into(),
                blurb: "Front-end building blocks, liftable into any UI.".into(),
                main: true,
            },
            LayerSpec {
                id: "kernel".into(),
                label: "kernel".into(),
                color: "#4f7ce0".into(),
                blurb: "The orchestration engine: decide/act loops, sessions, capability.".into(),
                main: true,
            },
            LayerSpec {
                id: "store".into(),
                label: "store".into(),
                color: "#d99a3d".into(),
                blurb: "Persistent and shared information: vault, conversations, memory, search."
                    .into(),
                main: true,
            },
            LayerSpec {
                id: "pack".into(),
                label: "pack".into(),
                color: "#4caf50".into(),
                blurb: "Domain packs (coding first); never beneath kernel/config/store.".into(),
                main: true,
            },
            LayerSpec {
                id: "service".into(),
                label: "service".into(),
                color: "#8e5bc0".into(),
                blurb: "Out-of-process adapters: MCP servers, bots, the forge.".into(),
                main: true,
            },
            LayerSpec {
                id: "surface".into(),
                label: "surface".into(),
                color: "#d05a8a".into(),
                blurb: "UIs — clients of the wire contract only.".into(),
                main: true,
            },
            LayerSpec {
                id: "root".into(),
                label: "root".into(),
                color: "#c0453a".into(),
                blurb: "Composition roots: the only crates allowed to see everything.".into(),
                main: true,
            },
            LayerSpec {
                id: "tooling".into(),
                label: "tooling".into(),
                color: "#9ab82f".into(),
                blurb: "Meta tooling (evals, tuner, this map). Not a build dependency.".into(),
                main: false,
            },
            LayerSpec {
                id: "testing".into(),
                label: "testing".into(),
                color: "#a07a50".into(),
                blurb: "Dev-dependency-only test support.".into(),
                main: false,
            },
            LayerSpec {
                id: "unknown".into(),
                label: "unknown".into(),
                color: "#5a5f68".into(),
                blurb: "No declared role — should pick a layer.".into(),
                main: false,
            },
        ],
        kinds: vec![
            KindSpec {
                id: "vault".into(),
                label: "Vault".into(),
                color: "#8a763a".into(),
                blurb: "Obsidian vault (source of truth)".into(),
                height: 1.4,
            },
            KindSpec {
                id: "provider".into(),
                label: "Provider".into(),
                color: "#399cb8".into(),
                blurb: "inference backend".into(),
                height: 1.2,
            },
            KindSpec {
                id: "mcp".into(),
                label: "MCP server".into(),
                color: "#6a8fd0".into(),
                blurb: "MCP server (agent tools)".into(),
                height: 0.95,
            },
            KindSpec {
                id: "pool".into(),
                label: "Pool".into(),
                color: "#7468c9".into(),
                blurb: "authority-segregated pool".into(),
                height: 0.7,
            },
            KindSpec {
                id: "profile".into(),
                label: "Session profile".into(),
                color: "#3da886".into(),
                blurb: "session profile (pack + hat)".into(),
                height: 0.7,
            },
            KindSpec {
                id: "project".into(),
                label: "Coding project".into(),
                color: "#c4842f".into(),
                blurb: "authorized coding root".into(),
                height: 0.7,
            },
            KindSpec {
                id: "schedule".into(),
                label: "Cron schedule".into(),
                color: "#b06a4a".into(),
                blurb: "cron schedule".into(),
                height: 0.7,
            },
            KindSpec {
                id: "hook".into(),
                label: "Webhook".into(),
                color: "#a64f6b".into(),
                blurb: "external webhook".into(),
                height: 0.7,
            },
            KindSpec {
                id: "notifier".into(),
                label: "Notifier".into(),
                color: "#5b9c8f".into(),
                blurb: "notification channel (Telegram)".into(),
                height: 0.85,
            },
        ],
    }
}
