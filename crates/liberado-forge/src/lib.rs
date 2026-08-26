//! One seam for forge operations (`docs/future-work/delegate-network-plan.md` §11).
//!
//! Branches, commits, PRs, CI checks, and review comments live on the forge; nothing
//! large travels over the delegation control plane — a PR URL is the deliverable.
//! [`ForgeClient`] is the whole surface: workers open and comment, delegators verify
//! checks and merge. Workers can never merge through this trait being *available* to
//! them is a composition-root decision, and the D1 worker simply does not hold one
//! wired to `merge`.
//!
//! Implementations: [`gitea::GiteaForge`] (REST over `api/v1`, the homelab forge).
//! GitHub stays on the shepherd's `gh` shell-out until its migration lands here.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod gitea;

/// A repository path relative to the forge root: `"OWNER/REPO"` (Gitea also allows an
/// organization path prefix — everything before the final `/` is the owner path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoPath(pub String);

impl RepoPath {
    /// The URL path segment under `/api/v1/repos/`.
    pub fn api_segment(&self) -> String {
        self.0.trim_matches('/').to_string()
    }
}

/// Everything opening a pull request needs. The head branch must already exist on the
/// remote — pushing it is git's job, not the forge client's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPr {
    pub repo: RepoPath,
    pub title: String,
    pub head: String,
    pub base: String,
    pub body: String,
}

/// A created pull request, addressed for every later call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrRef {
    pub repo: RepoPath,
    pub number: u64,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Success,
    Failure,
    Pending,
}

/// The named checks the delegator required, plus the overall verdict. A required name
/// with no reported context counts as [`CheckState::Pending`] — fail-safe, matching the
/// shepherd's rule that an absent selected check means waiting, never success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckStates {
    pub overall: CheckState,
    pub named: Vec<(String, CheckState)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    /// Gitea's `Do` verb for the merge endpoint.
    pub fn gitea_verb(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeCommit {
    pub sha: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("forge http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("forge returned {code}: {body}")]
    Status { code: u16, body: String },
    #[error("forge response was not valid JSON for this call: {0}")]
    Shape(String),
}

/// The whole forge surface delegation needs (plan §11). Object-safe so composition
/// roots choose Gitea/GitHub per deployment.
#[async_trait]
pub trait ForgeClient: Send + Sync {
    async fn open_pr(&self, req: &OpenPr) -> Result<PrRef, ForgeError>;
    async fn comment(&self, pr: &PrRef, body: &str) -> Result<(), ForgeError>;
    async fn checks(&self, pr: &PrRef, names: &[String]) -> Result<CheckStates, ForgeError>;

    /// The pull request's unified diff — the change surface a cold review reads
    /// (plan §10). Text, not JSON; empty output is valid and means "no changes".
    async fn diff(&self, pr: &PrRef) -> Result<String, ForgeError>;
    async fn merge(&self, pr: &PrRef, method: MergeMethod) -> Result<MergeCommit, ForgeError>;
}
