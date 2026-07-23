//! Mechanical enforcement of the workspace layer rules (2026-07-11 alignment audit).
//!
//! Every crate declares its layer in `[package.metadata.liberado] role = "..."`; this test parses
//! every `crates/*/Cargo.toml` and asserts the dependency rules that used to live only in prose
//! (`docs/architecture/modularity.md`, `docs/architecture/agentic-loops.md` "Dependency rules").
//! The `config-loader → coder-core` leak this replaces was found by a hand audit; the next one
//! should be found here.
//!
//! Rules (real `[dependencies]` only — dev-dependencies are deliberately exempt so live tests can
//! reach for concrete providers/notifiers):
//!
//! 1. **Pack containment** — only `pack`, `root`, and `tooling` crates may depend on `pack`
//!    crates. The kernel/config/store layers must never sit on a domain pack.
//! 2. **Surface thinness** — `surface` crates (TUI, WebUI) may depend only on `client` crates:
//!    the wire contract, shared commands, markdown, theme. A surface importing kernel internals
//!    is a client no more.
//! 3. **Client purity** — `client` crates may depend only on other `client` crates.
//! 4. **Foundation purity** — `foundation` crates may depend only on `foundation` crates.
//! 5. **Dependency budget** — non-`root`, non-`tooling` crates hold ≤ 8 internal deps; wide
//!    composition belongs in composition roots, not libraries.
//! 6. **Every crate is tagged** — a new crate without a role fails here, forcing a conscious
//!    layering decision at birth.

use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug)]
struct CrateInfo {
    name: String,
    role: String,
    internal_deps: Vec<String>,
}

fn workspace_crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .to_path_buf()
}

fn load_crates() -> BTreeMap<String, CrateInfo> {
    let mut crates = BTreeMap::new();
    let dir = workspace_crates_dir();
    for entry in std::fs::read_dir(&dir).expect("read crates/") {
        let entry = entry.expect("dir entry");
        let manifest_path = entry.path().join("Cargo.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&manifest_path).expect("read manifest");
        let manifest: toml::Value = raw.parse().expect("parse manifest TOML");

        let package = manifest.get("package").expect("[package] section");
        let name = package
            .get("name")
            .and_then(|v| v.as_str())
            .expect("package.name")
            .to_string();
        let role = package
            .get("metadata")
            .and_then(|m| m.get("liberado"))
            .and_then(|l| l.get("role"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let internal_deps = manifest
            .get("dependencies")
            .and_then(|d| d.as_table())
            .map(|deps| {
                deps.keys()
                    .filter(|k| is_internal(k))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        crates.insert(
            name.clone(),
            CrateInfo {
                name,
                role,
                internal_deps,
            },
        );
    }
    assert!(
        crates.len() >= 30,
        "expected the full workspace, found only {} crates — wrong directory?",
        crates.len()
    );
    crates
}

fn is_internal(dep: &str) -> bool {
    dep.starts_with("liberado-") || dep == "chat-client-contract"
}

const ROLES: &[&str] = &[
    "foundation",
    "client",
    "kernel",
    "store",
    "service",
    "pack",
    "surface",
    "root",
    "tooling",
    "testing",
];

#[test]
fn every_crate_declares_a_known_role() {
    for c in load_crates().values() {
        assert!(
            !c.role.is_empty(),
            "{}: missing [package.metadata.liberado] role — new crates must pick a layer \
             (see docs/architecture/contracts.md)",
            c.name
        );
        assert!(
            ROLES.contains(&c.role.as_str()),
            "{}: unknown role '{}' (known: {ROLES:?})",
            c.name,
            c.role
        );
    }
}

#[test]
fn packs_never_sit_beneath_the_system() {
    let crates = load_crates();
    let may_use_packs = ["pack", "root", "tooling"];
    for c in crates.values() {
        if may_use_packs.contains(&c.role.as_str()) {
            continue;
        }
        for dep in &c.internal_deps {
            let dep_role = crates.get(dep).map(|d| d.role.as_str()).unwrap_or("");
            assert_ne!(
                dep_role, "pack",
                "{} (role {}) depends on pack crate {} — domain packs must never sit beneath \
                 kernel/config/store layers (this is the config-loader → coder-core class of bug)",
                c.name, c.role, dep
            );
        }
    }
}

#[test]
fn surfaces_depend_only_on_client_crates() {
    let crates = load_crates();
    for c in crates.values().filter(|c| c.role == "surface") {
        for dep in &c.internal_deps {
            let dep_role = crates.get(dep).map(|d| d.role.as_str()).unwrap_or("");
            assert_eq!(
                dep_role, "client",
                "{} (surface) depends on {} (role {}) — surfaces are clients of the wire \
                 contract, never of internals",
                c.name, dep, dep_role
            );
        }
    }
}

#[test]
fn client_crates_stay_pure() {
    let crates = load_crates();
    for c in crates.values().filter(|c| c.role == "client") {
        for dep in &c.internal_deps {
            let dep_role = crates.get(dep).map(|d| d.role.as_str()).unwrap_or("");
            assert_eq!(
                dep_role, "client",
                "{} (client) depends on {} (role {}) — client crates must stay liftable into \
                 any front-end without dragging the system along",
                c.name, dep, dep_role
            );
        }
    }
}

#[test]
fn foundation_crates_stay_pure() {
    let crates = load_crates();
    for c in crates.values().filter(|c| c.role == "foundation") {
        for dep in &c.internal_deps {
            let dep_role = crates.get(dep).map(|d| d.role.as_str()).unwrap_or("");
            assert_eq!(
                dep_role, "foundation",
                "{} (foundation) depends on {} (role {}) — the bottom layer depends on nothing \
                 above itself",
                c.name, dep, dep_role
            );
        }
    }
}

#[test]
fn only_composition_roots_go_wide() {
    let crates = load_crates();
    const BUDGET: usize = 8;
    for c in crates.values() {
        if matches!(c.role.as_str(), "root" | "tooling") {
            continue;
        }
        assert!(
            c.internal_deps.len() <= BUDGET,
            "{} (role {}) has {} internal deps (budget {BUDGET}) — wide composition belongs in \
             a composition root, not a library crate: {:?}",
            c.name,
            c.role,
            c.internal_deps.len(),
            c.internal_deps
        );
    }
}

#[test]
fn god_modules_are_partitioned_into_lifecycle_files() {
    // Architectural hardening (2026-07-23): api / daemon / config model / executor budget
    // must stay multi-file so complexity does not re-accumulate in single god modules.
    let crates = workspace_crates_dir();
    let must_exist = [
        "server/src/api/mod.rs",
        "server/src/api/chat.rs",
        "server/src/api/goals.rs",
        "server/src/api/status.rs",
        "server/src/api/sessions.rs",
        "server/src/api/search.rs",
        "daemon/src/types.rs",
        "daemon/src/react.rs",
        "daemon/src/proposals.rs",
        "daemon/src/helpers.rs",
        "executor/src/budget.rs",
        "config-loader/src/model/mod.rs",
        "config-loader/src/model/topology.rs",
        "config-loader/src/model/policy.rs",
        "config-loader/src/model/tuning.rs",
        "config-loader/src/model/config.rs",
        "config-loader/src/model/builder.rs",
    ];
    for rel in must_exist {
        let path = crates.join(rel);
        assert!(
            path.is_file(),
            "expected partitioned module file missing: {} — do not re-monolith without updating this gate",
            path.display()
        );
    }
    // Former monolith files must not return.
    for rel in ["server/src/api.rs", "config-loader/src/model.rs"] {
        let path = crates.join(rel);
        assert!(
            !path.exists(),
            "god-module path returned: {} — keep the multi-file split",
            path.display()
        );
    }
}
