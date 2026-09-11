//! Admit one review attempt for an eligible cycle. Enabled adapters follow `harness_order`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use liberado_coder_core::has_command_issue;
use liberado_coder_core::next_review_worker;
use liberado_coder_core::pr_review::ObserverIntent;
use liberado_coder_core::pr_review::valid_full_sha;
use liberado_coder_core::pr_review_admission::{issue_review, review_command_id};
use liberado_coder_core::pr_review_port::{
    CodexReviewPort, OpenCodeReviewPort, PromptReviewPort, REVIEW_RESULT_SCHEMA,
    ReviewInvokeRequest, ReviewPort,
};
use liberado_coder_core::review_run_id;
use liberado_coder_core::{ReviewWorkerConfig, TaskLedger};
use liberado_common::process::std_command;

pub(crate) struct DispatchRequest<'a> {
    pub ledger: &'a mut TaskLedger,
    pub task_id: &'a str,
    pub repository: &'a str,
    pub pr_number: u64,
    pub head_sha: &'a str,
    pub base_sha: &'a str,
    pub base_branch: &'a str,
    pub coding_root: &'a Path,
    pub token: &'a str,
    pub review_workers: &'a BTreeMap<String, ReviewWorkerConfig>,
    pub harness_order: &'a [String],
    pub intents: &'a [ObserverIntent],
    pub shadow: bool,
}

struct DispatchPlan<'a> {
    worker_id: &'a str,
    worker: &'a ReviewWorkerConfig,
    command_id: String,
    run_id: String,
}

pub(crate) fn maybe_dispatch(req: DispatchRequest<'_>) -> Result<(), String> {
    let Some(plan) = dispatch_plan(&req) else {
        return Ok(());
    };
    let url = github_repository_url(req.repository)?;
    execute_planned_review(req, plan, &url)
}

fn execute_planned_review(
    req: DispatchRequest<'_>,
    plan: DispatchPlan<'_>,
    url: &str,
) -> Result<(), String> {
    let authorization = format!(
        "Authorization: Basic {}",
        STANDARD.encode(format!("x-access-token:{}", req.token))
    );
    fetch_pr_commits_from(
        req.coding_root,
        url,
        req.pr_number,
        req.base_branch,
        req.base_sha,
        req.head_sha,
        Some(&authorization),
    )?;
    let workspace = pin_checkout(req.coding_root, req.head_sha)?;
    let schema_path = write_schema_outside(&workspace)?;
    let request = ReviewInvokeRequest {
        task_id: req.task_id.into(),
        command_id: plan.command_id,
        run_id: plan.run_id,
        repository: req.repository.into(),
        pr_number: req.pr_number,
        base_sha: req.base_sha.into(),
        expected_sha: req.head_sha.into(),
        workspace,
        schema_path,
    };
    let port = review_port(plan.worker)?;
    let artifact_dir = req.coding_root.join(".liberado").join("review-artifacts");
    let _ = issue_review(
        req.ledger,
        port.as_ref(),
        plan.worker_id,
        &request,
        &artifact_dir,
    )?;
    Ok(())
}

fn review_port(worker: &ReviewWorkerConfig) -> Result<Box<dyn ReviewPort>, String> {
    match worker {
        ReviewWorkerConfig::Codex { executable, .. } => {
            Ok(Box::new(CodexReviewPort::new(executable)))
        }
        ReviewWorkerConfig::OpenCode {
            executable, model, ..
        } => Ok(Box::new(OpenCodeReviewPort::new(executable, model))),
        ReviewWorkerConfig::GrokBuild { executable, .. } => {
            Ok(Box::new(PromptReviewPort::grok(executable)))
        }
        ReviewWorkerConfig::CursorLocal { executable, .. } => {
            Ok(Box::new(PromptReviewPort::cursor(executable)))
        }
        _ => Err("review worker is not an enabled review adapter".into()),
    }
}

fn dispatch_plan<'a>(req: &DispatchRequest<'a>) -> Option<DispatchPlan<'a>> {
    if req.shadow || req.base_sha.len() != 40 {
        return None;
    }
    if !req
        .intents
        .iter()
        .any(|intent| matches!(intent, ObserverIntent::Eligible { sha } if sha == req.head_sha))
    {
        return None;
    }
    let command_id = review_command_id(req.repository, req.pr_number, req.head_sha);
    let (worker_id, worker) = next_review_worker(
        req.ledger,
        &command_id,
        req.harness_order,
        req.review_workers,
    )?;
    let run_id = if has_command_issue(req.ledger, &command_id) {
        review_run_id(&command_id, worker_id)
    } else {
        command_id.clone()
    };
    Some(DispatchPlan {
        worker_id,
        worker,
        command_id,
        run_id,
    })
}

fn fetch_pr_commits_from(
    repo_root: &Path,
    url: &str,
    pr_number: u64,
    base_branch: &str,
    base_sha: &str,
    head_sha: &str,
    authorization: Option<&str>,
) -> Result<(), String> {
    if pr_number == 0 || !valid_full_sha(base_sha) || !valid_full_sha(head_sha) {
        return Err("review fetch requires a PR number and full base/head SHAs".into());
    }
    validate_branch(repo_root, base_branch)?;
    let namespace = format!("refs/liberado/reviews/{pr_number}/{head_sha}");
    let head_ref = format!("{namespace}/head");
    let base_ref = format!("{namespace}/base");
    let mut command = std_command("git");
    command
        .args(["fetch", "--no-tags", "--force", "--no-write-fetch-head"])
        .arg(url)
        .arg(format!("+refs/pull/{pr_number}/head:{head_ref}"))
        .arg(format!("+refs/heads/{base_branch}:{base_ref}"))
        .current_dir(repo_root)
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(authorization) = authorization {
        set_fetch_auth(&mut command, repo_root, authorization);
    }
    let output = command.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!("failed to fetch pull request #{pr_number}"));
    }
    let observed = git_stdout(repo_root, &["rev-parse", "--verify", &head_ref])?;
    if observed.trim() != head_sha {
        return Err(format!(
            "pull request #{pr_number} moved while its review checkout was prepared"
        ));
    }
    if !commit_exists(repo_root, base_sha) {
        return Err(format!(
            "base SHA for pull request #{pr_number} was not fetched"
        ));
    }
    tracing::info!(
        repository = url,
        pr_number,
        head_sha,
        "fetched exact pull-request revision"
    );
    Ok(())
}

fn github_repository_url(repository: &str) -> Result<String, String> {
    let parts: Vec<_> = repository.split('/').collect();
    if parts.len() != 2 || parts.iter().any(|part| !valid_repository_component(part)) {
        return Err("GitHub repository must be an owner/name pair".into());
    }
    Ok(format!("https://github.com/{repository}.git"))
}

fn valid_repository_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_branch(repo_root: &Path, branch: &str) -> Result<(), String> {
    let output = std_command("git")
        .args(["check-ref-format", "--branch", branch])
        .current_dir(repo_root)
        .output()
        .map_err(|error| error.to_string())?;
    output
        .status
        .success()
        .then_some(())
        .ok_or_else(|| "review base branch is not a valid Git branch".into())
}

fn set_fetch_auth(command: &mut Command, repo_root: &Path, authorization: &str) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_CONFIG_") {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "safe.directory")
        .env("GIT_CONFIG_VALUE_0", repo_root)
        .env("GIT_CONFIG_KEY_1", "http.https://github.com/.extraHeader")
        .env("GIT_CONFIG_VALUE_1", authorization);
}

fn commit_exists(repo_root: &Path, sha: &str) -> bool {
    std_command("git")
        .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .current_dir(repo_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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
