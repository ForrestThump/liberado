//! Slice 2: admit one Codex review for an eligible cycle. No forge writes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use liberado_coder_core::pr_review::ObserverIntent;
use liberado_coder_core::pr_review_admission::{issue_review, review_command_id};
use liberado_coder_core::pr_review_port::{
    CodexReviewPort, REVIEW_RESULT_SCHEMA, ReviewInvokeRequest,
};
use liberado_coder_core::{ReviewWorkerConfig, TaskLedger};
use liberado_common::process::std_command;

pub(crate) struct DispatchRequest<'a> {
    pub ledger: &'a mut TaskLedger,
    pub task_id: &'a str,
    pub repository: &'a str,
    pub pr_number: u64,
    pub head_sha: &'a str,
    pub base_sha: &'a str,
    pub coding_root: &'a Path,
    pub review_workers: &'a BTreeMap<String, ReviewWorkerConfig>,
    pub harness_order: &'a [String],
    pub intents: &'a [ObserverIntent],
    pub shadow: bool,
}

pub(crate) fn maybe_dispatch(req: DispatchRequest<'_>) -> Result<(), String> {
    if req.shadow || req.base_sha.len() != 40 {
        return Ok(());
    }
    if !req
        .intents
        .iter()
        .any(|intent| matches!(intent, ObserverIntent::Eligible { sha } if sha == req.head_sha))
    {
        return Ok(());
    }
    let Some((worker_id, worker)) = req.harness_order.iter().find_map(|id| {
        req.review_workers
            .get_key_value(id)
            .filter(|(_, worker)| matches!(worker, ReviewWorkerConfig::Codex { enabled: true, .. }))
    }) else {
        return Ok(());
    };
    let ReviewWorkerConfig::Codex { executable, .. } = worker else {
        return Ok(());
    };
    let workspace = pin_checkout(req.coding_root, req.head_sha)?;
    let schema_path = write_schema_outside(&workspace)?;
    let command_id = review_command_id(req.repository, req.pr_number, req.head_sha);
    let request = ReviewInvokeRequest {
        task_id: req.task_id.into(),
        command_id: command_id.clone(),
        run_id: command_id,
        repository: req.repository.into(),
        pr_number: req.pr_number,
        base_sha: req.base_sha.into(),
        expected_sha: req.head_sha.into(),
        workspace,
        schema_path,
    };
    let port = CodexReviewPort::new(executable);
    let artifact_dir = req.coding_root.join(".liberado").join("review-artifacts");
    let _ = issue_review(req.ledger, &port, worker_id, &request, &artifact_dir)?;
    Ok(())
}

fn pin_checkout(repo_root: &Path, sha: &str) -> Result<PathBuf, String> {
    let short = sha.get(..12).unwrap_or(sha);
    let dest = repo_root
        .join(".liberado")
        .join("review-worktrees")
        .join(short);
    if checkout_matches(&dest, sha)? {
        return Ok(dest);
    }
    replace_checkout(repo_root, &dest, sha)?;
    Ok(dest)
}

fn checkout_matches(dest: &Path, sha: &str) -> Result<bool, String> {
    if !dest.is_dir() {
        return Ok(false);
    }
    Ok(git_stdout(dest, &["rev-parse", "HEAD"])?.trim() == sha)
}

fn replace_checkout(repo_root: &Path, dest: &Path, sha: &str) -> Result<(), String> {
    let _ = std::fs::create_dir_all(dest.parent().unwrap_or(repo_root));
    remove_existing_checkout(repo_root, dest);
    let status = std_command("git")
        .args(["worktree", "add", "--detach"])
        .arg(dest)
        .arg(sha)
        .current_dir(repo_root)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("failed to pin review checkout at {sha}"));
    }
    Ok(())
}

fn remove_existing_checkout(repo_root: &Path, dest: &Path) {
    if !dest.exists() {
        return;
    }
    let _ = std_command("git")
        .args(["worktree", "remove", "--force"])
        .arg(dest)
        .current_dir(repo_root)
        .status();
    let _ = std::fs::remove_dir_all(dest);
}

fn write_schema_outside(workspace: &Path) -> Result<PathBuf, String> {
    let path = workspace.parent().unwrap_or(workspace).join(format!(
        "schema-{}.json",
        workspace
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("review")
    ));
    std::fs::write(&path, REVIEW_RESULT_SCHEMA).map_err(|e| e.to_string())?;
    Ok(path)
}

fn git_stdout(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = std_command("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("git command failed".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
#[path = "pr_review_dispatch_tests.rs"]
mod tests;
