//! Gitea REST implementation of [`ForgeClient`] over `api/v1` with a token
//! (`docs/future-work/delegate-network-plan.md` §11). The homelab forge; mirrors the
//! GitHub surface closely enough for branches, PRs, comments, statuses, and merge.
//!
//! Auth is the `token` scheme (`Authorization: token <t>`), which Gitea expects for
//! API tokens. Check verification computes the overall verdict from the *required*
//! names only — a green combined status with a required context absent still reads
//! [`CheckState::Pending`], because branch protection is optional on homelab Gitea and
//! the delegator verifies checks itself rather than trusting the forge.

use super::{
    CheckState, CheckStates, ForgeClient, ForgeError, MergeCommit, MergeMethod, OpenPr, PrRef,
    RepoPath,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

pub struct GiteaForge {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

impl GiteaForge {
    /// `base_url` like `https://git.example.com` (or `http://192.168.x.y:3000`);
    /// trailing slashes are tolerated. The token is a Gitea access token, never a password.
    pub fn new(base_url: &str, token: &str) -> Result<Self, ForgeError> {
        Self::with_tls(base_url, token, false)
    }

    /// Same, with explicit control over certificate validation. `accept_invalid_certs`
    /// exists for LAN forges whose TLS certificates are not signed by a public CA
    /// (homelab Gitea behind a local CA); it is a deliberate deployment choice, never
    /// a default — set it only for hosts you already trust at the network layer.
    pub fn with_tls(
        base_url: &str,
        token: &str,
        accept_invalid_certs: bool,
    ) -> Result<Self, ForgeError> {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(accept_invalid_certs)
            .build()
            .map_err(ForgeError::Http)?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        })
    }

    fn repos_api(&self, repo: &RepoPath, tail: &str) -> String {
        format!(
            "{}/api/v1/repos/{}/{}",
            self.base_url,
            repo.api_segment(),
            tail
        )
    }

    async fn send(
        &self,
        method: reqwest::Method,
        url: String,
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response, ForgeError> {
        let mut request = self.http.request(method, &url);
        if !self.token.is_empty() {
            request = request.header("Authorization", format!("token {}", self.token));
        }
        request = match body {
            Some(value) => request.json(&value),
            None => request,
        };
        request.send().await.map_err(|error| {
            // The endpoint travels in the message: delegation failures are read off logs.
            ForgeError::Shape(format!("request to {url} failed: {error}"))
        })
    }

    async fn require(
        &self,
        response: reqwest::Response,
        url: &str,
        expected: reqwest::StatusCode,
    ) -> Result<reqwest::Response, ForgeError> {
        let status = response.status();
        if status == expected || (expected.is_success() && status.is_success()) {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        Err(ForgeError::Status {
            code: status.as_u16(),
            body: format!("{url}: {body}"),
        })
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
        url: &str,
    ) -> Result<T, ForgeError> {
        response
            .json()
            .await
            .map_err(|error| ForgeError::Shape(format!("unusable response from {url}: {error}")))
    }

    async fn get_pull_head(&self, pr: &PrRef) -> Result<String, ForgeError> {
        let url = self.repos_api(&pr.repo, &format!("pulls/{}", pr.number));
        let pull: GiteaPull = self
            .json(
                self.require(
                    self.send(reqwest::Method::GET, url.clone(), None).await?,
                    &url,
                    reqwest::StatusCode::OK,
                )
                .await?,
                &url,
            )
            .await?;
        Ok(pull.head_branch.sha)
    }
}

#[derive(Deserialize)]
struct GiteaPull {
    number: u64,
    html_url: String,
    #[serde(rename = "head")]
    head_branch: HeadInfo,
}

#[derive(Deserialize)]
struct HeadInfo {
    sha: String,
}

/// After a successful merge, the PR carries the id of the commit the merge produced.
#[derive(Deserialize)]
struct MergedPull {
    /// Gitea's field name has drifted across versions (`merged_commit_id` on most
    /// instances, `merge_commit_sha` on others); both deserialize here. A missing id
    /// with a 200 readback still means merged — the sha is informational.
    #[serde(default, alias = "merge_commit_sha")]
    merged_commit_id: Option<String>,
    #[serde(default)]
    merged: bool,
}

#[derive(Deserialize)]
struct CombinedStatus {
    statuses: Vec<NamedStatus>,
}

#[derive(Deserialize)]
struct NamedStatus {
    status: String,
    context: String,
}

fn map_state(raw: &str) -> CheckState {
    match raw {
        "success" => CheckState::Success,
        "failure" | "error" => CheckState::Failure,
        _ => CheckState::Pending,
    }
}

/// The overall verdict comes from the required names alone: any failure fails, anything
/// missing or unfinished is pending, and only every-required-green is success.
fn overall(named: &[(String, CheckState)]) -> CheckState {
    named
        .iter()
        .map(|(_, state)| *state)
        .fold(CheckState::Success, |worst, state| match (worst, state) {
            (_, CheckState::Failure) | (CheckState::Failure, _) => CheckState::Failure,
            (CheckState::Pending, _) | (_, CheckState::Pending) => CheckState::Pending,
            _ => CheckState::Success,
        })
}

#[async_trait]
impl ForgeClient for GiteaForge {
    async fn open_pr(&self, req: &OpenPr) -> Result<PrRef, ForgeError> {
        let url = self.repos_api(&req.repo, "pulls");
        let body = json!({
            "title": req.title,
            "head": req.head,
            "base": req.base,
            "body": req.body,
        });
        let response = self
            .require(
                self.send(reqwest::Method::POST, url.clone(), Some(body))
                    .await?,
                &url,
                reqwest::StatusCode::CREATED,
            )
            .await?;
        let pull: GiteaPull = self.json(response, &url).await?;
        Ok(PrRef {
            repo: req.repo.clone(),
            number: pull.number,
            url: pull.html_url,
        })
    }

    async fn comment(&self, pr: &PrRef, body: &str) -> Result<(), ForgeError> {
        // Pull requests share the issue namespace in Gitea; comments go through issues/.
        let url = self.repos_api(&pr.repo, &format!("issues/{}/comments", pr.number));
        self.require(
            self.send(
                reqwest::Method::POST,
                url.clone(),
                Some(json!({ "body": body })),
            )
            .await?,
            &url,
            reqwest::StatusCode::CREATED,
        )
        .await?;
        Ok(())
    }

    async fn checks(&self, pr: &PrRef, names: &[String]) -> Result<CheckStates, ForgeError> {
        let sha = self.get_pull_head(pr).await?;
        let status_url = self.repos_api(&pr.repo, &format!("commits/{sha}/status"));
        let combined: CombinedStatus = self
            .json(
                self.require(
                    self.send(reqwest::Method::GET, status_url.clone(), None)
                        .await?,
                    &status_url,
                    reqwest::StatusCode::OK,
                )
                .await?,
                &status_url,
            )
            .await?;

        let named = names
            .iter()
            .map(|name| {
                let found = combined
                    .statuses
                    .iter()
                    .find(|reported| reported.context == *name)
                    .map(|reported| map_state(&reported.status))
                    .unwrap_or(CheckState::Pending);
                (name.clone(), found)
            })
            .collect::<Vec<_>>();
        Ok(CheckStates {
            overall: overall(&named),
            named,
        })
    }

    async fn diff(&self, pr: &PrRef) -> Result<String, ForgeError> {
        // Gitea serves `.diff` as plain text off the HTML route, not the API prefix.
        let url = format!(
            "{}/repos/{}/pulls/{}.diff",
            self.base_url,
            pr.repo.api_segment(),
            pr.number
        );
        let response = self
            .http
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ForgeError::Status {
                code: status.as_u16(),
                body: format!("GET {url}: {body}"),
            });
        }
        Ok(body)
    }

    async fn merge(&self, pr: &PrRef, method: MergeMethod) -> Result<MergeCommit, ForgeError> {
        let url = self.repos_api(&pr.repo, &format!("pulls/{}/merge", pr.number));
        self.require(
            self.send(
                reqwest::Method::POST,
                url.clone(),
                Some(json!({ "Do": method.gitea_verb() })),
            )
            .await?,
            &url,
            reqwest::StatusCode::OK,
        )
        .await?;
        let readback_url = self.repos_api(&pr.repo, &format!("pulls/{}", pr.number));
        let merged: MergedPull = self
            .json(
                self.require(
                    self.send(reqwest::Method::GET, readback_url.clone(), None)
                        .await?,
                    &readback_url,
                    reqwest::StatusCode::OK,
                )
                .await?,
                &readback_url,
            )
            .await?;
        resolve_merge_commit(merged, pr)
    }
}

/// Interpret a post-merge readback. Gitea's sha field has drifted across versions
/// (`merged_commit_id`, `merge_commit_sha`); both deserialize. A 200 readback that
/// claims merged but carries no sha still means merged — report the PR reference
/// rather than failing uselessly after the fact.
fn resolve_merge_commit(merged: MergedPull, pr: &PrRef) -> Result<MergeCommit, ForgeError> {
    match (merged.merged, merged.merged_commit_id) {
        (_, Some(sha)) => Ok(MergeCommit { sha }),
        (true, None) => Ok(MergeCommit {
            sha: format!("pr/{}", pr.number),
        }),
        (false, None) => Err(ForgeError::Shape(format!(
            "pull {}/#{} reports itself as unmerged after merge",
            pr.repo.api_segment(),
            pr.number
        ))),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod merge_shape_tests {
    use super::{MergeCommit, resolve_merge_commit};
    use crate::{ForgeError, PrRef, RepoPath};

    fn pr() -> PrRef {
        PrRef {
            repo: RepoPath("o/r".into()),
            number: 7,
            url: "http://f/o/r/pulls/7".into(),
        }
    }

    #[test]
    fn either_sha_field_wins_over_everything() {
        let ok = resolve_merge_commit(
            serde_json::from_value(serde_json::json!({
                "merged": true, "merge_commit_sha": "abc123"
            }))
            .unwrap(),
            &pr(),
        )
        .unwrap();
        assert_eq!(
            ok,
            MergeCommit {
                sha: "abc123".into()
            }
        );
    }

    /// The live-dogfood case: Gitea answered 200 and the readback carried no sha.
    /// The forge DID merge; failing here would lie about the outcome.
    #[test]
    fn merged_without_a_sha_reports_the_pr_reference() {
        let ok = resolve_merge_commit(
            serde_json::from_value(serde_json::json!({ "merged": true })).unwrap(),
            &pr(),
        )
        .unwrap();
        assert_eq!(ok, MergeCommit { sha: "pr/7".into() });
    }

    #[test]
    fn unmerged_readbacks_are_honest_errors() {
        let err = resolve_merge_commit(
            serde_json::from_value(serde_json::json!({ "merged": false })).unwrap(),
            &pr(),
        );
        assert!(matches!(err, Err(ForgeError::Shape(_))));
    }
}
