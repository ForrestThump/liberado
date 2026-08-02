//! Vault report writer — every run, under `conformance/reports/`.

use std::path::Path;

use chrono::Utc;

use crate::result::{PathStatus, RunReport};

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
    Ok(rel)
}
