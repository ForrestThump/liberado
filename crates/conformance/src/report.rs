//! Vault report writer — every run, under `conformance/reports/`.

use std::path::Path;

use chrono::Utc;

use crate::result::{PathStatus, RunReport};

/// Preferred owner for suite residue under the vault. TurboVault (and the host user) are uid 1000
/// on the homelab; the conformance binary often runs as root inside the daemon container. Files
/// left as root cause `write_note` → Permission denied on the next agent write into that tree.
#[cfg(unix)]
const VAULT_OWNER_UID: u32 = 1000;
#[cfg(unix)]
const VAULT_OWNER_GID: u32 = 1000;

/// Write `conformance/reports/<ts>-<pass|fail>.md` and return the path relative to the vault.
pub fn write_vault_report(vault_path: &Path, report: &RunReport) -> Result<std::path::PathBuf, String> {
    let tag = match report.overall {
        PathStatus::Pass => "pass",
        PathStatus::Fail => "fail",
        PathStatus::Skipped => "skipped",
    };
    let ts = Utc::now().format("%Y-%m-%dT%H%M%SZ");
    let rel = std::path::PathBuf::from("conformance")
        .join("reports")
        .join(format!("{ts}-{tag}.md"));
    let abs = vault_path.join(&rel);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        // Best-effort: keep the suite zone writable by TurboVault / host user.
        reclaim_owner(parent);
        reclaim_owner(vault_path.join("conformance"));
    }

    let mut md = String::new();
    md.push_str(&format!("# Tier 3 conformance — {tag}\n\n"));
    md.push_str(&format!("- **started**: {}\n", report.started_at));
    md.push_str(&format!("- **finished**: {}\n", report.finished_at));
    md.push_str(&format!("- **overall**: {:?}\n", report.overall));
    md.push_str(&format!("- **base_url**: `{}`\n\n", report.base_url));
    md.push_str("## Paths\n\n");
    for r in &report.results {
        md.push_str(&format!(
            "### `{}` — {:?}\n\n",
            r.path, r.status
        ));
        if !r.assertion.is_empty() {
            md.push_str(&format!("- assertion: {}\n", r.assertion));
        }
        md.push_str(&format!("- duration_ms: {}\n", r.duration_ms));
        if r.advisory {
            md.push_str("- advisory: true\n");
        }
        if let Some(reason) = &r.reason {
            md.push_str(&format!("- reason: {reason}\n"));
        }
        if let Some(ev) = &r.evidence {
            md.push_str("- evidence:\n\n```json\n");
            md.push_str(&serde_json::to_string_pretty(ev).unwrap_or_else(|_| "{}".into()));
            md.push_str("\n```\n");
        }
        md.push('\n');
    }

    std::fs::write(&abs, md).map_err(|e| format!("write {}: {e}", abs.display()))?;
    reclaim_owner(&abs);
    Ok(rel)
}

#[cfg(unix)]
fn reclaim_owner(path: impl AsRef<Path>) {
    let path = path.as_ref();
    let _ = std::os::unix::fs::chown(path, Some(VAULT_OWNER_UID), Some(VAULT_OWNER_GID));
}

#[cfg(not(unix))]
fn reclaim_owner(_path: impl AsRef<Path>) {}
