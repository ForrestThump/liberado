//! `GET /api/library` — a capped, read-only list of vault files.
//!
//! v1 lists files under vault zones the default profile may read, and only when the zone
//! name is an artifact folder (`attachments`, `media`, and the same kind of name). Notes in
//! `tasks` or `decisions` stay out. Session artifacts and executor offload stay out. There
//! is no upload and no delete.

use std::path::{Component, Path};
use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use liberado_common::{Capability, CapabilitySet, Zone};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

const DEFAULT_CHAT_PROFILE: &str = "chat-default";
const DEFAULT_COMPONENT: &str = "main-agent";
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const SCAN_CAP: usize = 2_000;
const MAX_DEPTH: u8 = 8;

/// Zone names v1 treats as library folders. Comparison is ASCII case-insensitive.
/// A readable zone with any other name is not a library item.
const ARTIFACT_ZONES: &[&str] = &[
    "attachments",
    "media",
    "images",
    "files",
    "exports",
    "artifacts",
];

#[derive(Debug, Deserialize)]
pub(crate) struct LibraryQuery {
    limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub path_or_url: String,
    pub created_at: String,
    /// Always `"vault"` in v1.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryResponse {
    pub items: Vec<LibraryItem>,
}

/// `GET /api/library?limit=50`.
///
/// `limit` defaults to 50 and is clamped to 1..=200. A missing vault is an empty list.
pub async fn list_library(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LibraryQuery>,
) -> impl IntoResponse {
    let limit = clamped_limit(query.limit);
    let zones = library_zones(&state.config);
    let items = list_bounded(Path::new(&state.vault_path), &zones, limit, SCAN_CAP);
    Json(LibraryResponse { items })
}

fn clamped_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn library_zones(config: &liberado_bootstrap::Config) -> Vec<String> {
    let mut zones = Vec::new();
    for name in read_zone_names(&profile_caps(config)) {
        if artifact_zone(&name) {
            push_unique(&mut zones, name);
        }
    }
    zones
}

fn profile_caps(config: &liberado_bootstrap::Config) -> CapabilitySet {
    match config.resolve_session_profile(Some(DEFAULT_CHAT_PROFILE), "") {
        Ok(profile) => profile.capabilities,
        Err(_) => config.policy.capabilities_for(DEFAULT_COMPONENT),
    }
}

fn read_zone_names(caps: &CapabilitySet) -> Vec<String> {
    let mut names = Vec::new();
    for cap in &caps.capabilities {
        if let Capability::Read(Zone::Vault(name)) = cap {
            names.push(name.clone());
        }
    }
    names
}

fn artifact_zone(name: &str) -> bool {
    ARTIFACT_ZONES
        .iter()
        .any(|zone| zone.eq_ignore_ascii_case(name))
}

fn push_unique(zones: &mut Vec<String>, name: String) {
    if !zones.iter().any(|zone| zone == &name) {
        zones.push(name);
    }
}

fn list_bounded(root: &Path, zones: &[String], limit: usize, scan_cap: usize) -> Vec<LibraryItem> {
    let mut items = Vec::new();
    for zone in zones {
        if items.len() >= scan_cap {
            break;
        }
        walk(&root.join(zone), root, 0, scan_cap, &mut items);
    }
    items.sort_by(newest_first);
    items.truncate(limit);
    items
}

fn newest_first(a: &LibraryItem, b: &LibraryItem) -> std::cmp::Ordering {
    b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id))
}

fn walk(dir: &Path, root: &Path, depth: u8, scan_cap: usize, out: &mut Vec<LibraryItem>) {
    if out.len() >= scan_cap || depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= scan_cap {
            return;
        }
        visit(&entry, root, depth, scan_cap, out);
    }
}

fn visit(
    entry: &std::fs::DirEntry,
    root: &Path,
    depth: u8,
    scan_cap: usize,
    out: &mut Vec<LibraryItem>,
) {
    if hidden_entry(entry) {
        return;
    }
    let Ok(kind) = entry.file_type() else {
        return;
    };
    if kind.is_symlink() {
        return;
    }
    if kind.is_dir() {
        walk(&entry.path(), root, depth + 1, scan_cap, out);
        return;
    }
    if kind.is_file()
        && let Some(item) = file_item(&entry.path(), root)
    {
        out.push(item);
    }
}

fn hidden_entry(entry: &std::fs::DirEntry) -> bool {
    match entry.file_name().to_str() {
        Some(name) => name.starts_with('.'),
        None => true,
    }
}

fn file_item(path: &Path, root: &Path) -> Option<LibraryItem> {
    let rel = vault_rel(root, path)?;
    let meta = std::fs::metadata(path).ok()?;
    let created_at = modified_stamp(&meta)?;
    let title = path.file_name()?.to_str()?.to_string();
    Some(LibraryItem {
        id: rel.clone(),
        kind: kind_of(path),
        title,
        path_or_url: rel,
        created_at,
        source: "vault".to_string(),
    })
}

fn vault_rel(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut out = String::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str()?;
                if !out.is_empty() {
                    out.push('/');
                }
                out.push_str(part);
            }
            _ => return None,
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn kind_of(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
        .unwrap_or_else(|| "file".to_string())
}

/// Last modification time in UTC. Birth time is not portable, so v1 does not use it.
fn modified_stamp(meta: &std::fs::Metadata) -> Option<String> {
    let modified = meta.modified().ok()?;
    let dt: chrono::DateTime<chrono::Utc> = modified.into();
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

#[cfg(test)]
#[path = "library_tests.rs"]
mod tests;
