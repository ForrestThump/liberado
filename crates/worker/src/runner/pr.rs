//! The PR boundary of a delegated run (plan §7.4/§10): open on the first pass,
//! update on a kickback. Split from the pipeline file for module health; as a child
//! module it reads [`super::RunContext`] and the store without widening anything.

use liberado_delegate_contract::{TaskSpec, TaskStatus};
use liberado_forge::{ForgeClient, OpenPr, PrRef, RepoPath};

use super::{RunContext, RunShape, first_line, pr_body};

pub(super) async fn open_or_update_pr(
    ctx: &RunContext,
    spec: &TaskSpec,
    branch: &str,
    shape: RunShape,
    result: &liberado_coder_core::CoderRunResult,
) -> Result<(), String> {
    let Some(forge) = ctx.forge.as_deref() else {
        return Err("forge is not configured on this worker; cannot open PR".into());
    };
    if let Some(url) = &shape.existing_pr_url {
        return update_pull_request(forge, ctx, spec, url, result).await;
    }
    open_pull_request(forge, ctx, spec, branch, result).await
}

/// Kickback path: report the new outcome on the existing PR and keep its url. The
/// summary comment is the visible audit trail a reviewer reads before re-reviewing.
async fn update_pull_request(
    forge: &dyn ForgeClient,
    ctx: &RunContext,
    spec: &TaskSpec,
    pr_url: &str,
    result: &liberado_coder_core::CoderRunResult,
) -> Result<(), String> {
    let repo = RepoPath(spec.repository.clone());
    let pr = pr_ref_from_url(pr_url, repo)
        .ok_or_else(|| format!("cannot parse PR reference from {pr_url}"))?;
    forge
        .comment(
            &pr,
            &format!(
                "Kickback applied. Outcome: {:?}\n\n{}",
                result.outcome, result.summary
            ),
        )
        .await
        .map_err(|error| format!("comment on {pr_url}: {error}"))?;
    ctx.store
        .finish(
            &spec.id,
            TaskStatus::PrOpened {
                url: pr_url.to_string(),
            },
        )
        .map_err(|error| format!("record PR status: {error}"))?;
    Ok(())
}

/// Parse the worker's own canonical PR url back into an addressable reference. Only
/// urls this crate mints from forge responses are accepted (`…/pulls/<n>` for Gitea).
pub(super) fn pr_ref_from_url(url: &str, repo: RepoPath) -> Option<PrRef> {
    let number: u64 = url.rsplit('/').next()?.parse().ok()?;
    Some(PrRef {
        repo,
        number,
        url: url.to_string(),
    })
}

async fn open_pull_request(
    forge: &dyn ForgeClient,
    ctx: &RunContext,
    spec: &TaskSpec,
    branch: &str,
    result: &liberado_coder_core::CoderRunResult,
) -> Result<(), String> {
    let pr = forge
        .open_pr(&OpenPr {
            repo: RepoPath(spec.repository.clone()),
            title: truncate(first_line(&spec.goal), 72),
            head: branch.to_string(),
            base: spec.base_branch.clone(),
            body: pr_body(spec, result),
        })
        .await
        .map_err(|error| format!("open PR: {error}"))?;
    ctx.store
        .finish(
            &spec.id,
            TaskStatus::PrOpened {
                url: pr.url.clone(),
            },
        )
        .map_err(|error| format!("record PR status: {error}"))?;
    tracing::info!(task = %spec.id, pr_url = %pr.url, "delegated task opened a PR");
    Ok(())
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{cut}…")
}
