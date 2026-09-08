//! GitHub publication adapter for Slice 3. Credentials stay in shepherd.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use liberado_coder_core::TaskLedger;
use liberado_coder_core::pr_review_publish::{
    InlineComment, PublishPr, PublishRequest, ReviewPublishPort, reconcile_publication,
};
use liberado_config::{ShepherdProjectConfig, Topology};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde_json::{Value, json};

pub(crate) struct GithubReviewPublishPort {
    client: Client,
    token: String,
}

impl GithubReviewPublishPort {
    pub(crate) fn new(token: impl Into<String>) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            token: token.into(),
        })
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        self.client
            .get(format!("https://api.github.com{path}"))
            .header(USER_AGENT, "liberado-pr-review-publisher")
            .header(ACCEPT, "application/vnd.github+json")
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .send()
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .map_err(|e| e.to_string())
    }

    fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.client
            .post(format!("https://api.github.com{path}"))
            .header(USER_AGENT, "liberado-pr-review-publisher")
            .header(ACCEPT, "application/vnd.github+json")
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .json(&body)
            .send()
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .map_err(|e| e.to_string())
    }

    fn put(&self, path: &str, body: Value) -> Result<Value, String> {
        self.client
            .put(format!("https://api.github.com{path}"))
            .header(USER_AGENT, "liberado-pr-review-publisher")
            .header(ACCEPT, "application/vnd.github+json")
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .json(&body)
            .send()
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .map_err(|e| e.to_string())
    }
}

impl ReviewPublishPort for GithubReviewPublishPort {
    fn read_pr(&self, repo: &str, number: u64) -> Result<PublishPr, String> {
        let value = self.get(&format!("/repos/{repo}/pulls/{number}"))?;
        Ok(PublishPr {
            head_sha: value
                .pointer("/head/sha")
                .and_then(|v| v.as_str())
                .ok_or("missing head sha")?
                .into(),
            draft: value
                .get("draft")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            open: value.get("state").and_then(|v| v.as_str()) == Some("open"),
        })
    }

    fn authenticated_login(&self) -> Result<String, String> {
        let value = self.get("/user")?;
        value
            .get("login")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| "missing authenticated login".into())
    }

    fn find_comment_review_by_marker(
        &self,
        repo: &str,
        number: u64,
        marker: &str,
    ) -> Result<Option<u64>, String> {
        let value = self.get(&format!(
            "/repos/{repo}/pulls/{number}/reviews?per_page=100"
        ))?;
        let rows = value.as_array().ok_or("reviews list was not an array")?;
        Ok(rows.iter().find_map(|row| {
            let body = row.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let event = row.get("event").and_then(|v| v.as_str()).unwrap_or("");
            // GitHub may omit event on list; also check state COMMENTED.
            let state = row.get("state").and_then(|v| v.as_str()).unwrap_or("");
            if body.contains(marker)
                && (event == "COMMENT" || event.is_empty())
                && (state == "COMMENTED" || state.is_empty() || state == "PENDING")
            {
                row.get("id").and_then(|v| v.as_u64())
            } else {
                None
            }
        }))
    }

    fn create_comment_review(
        &self,
        repo: &str,
        number: u64,
        commit_id: &str,
        body: &str,
        inline: &[InlineComment],
    ) -> Result<u64, String> {
        let comments: Vec<Value> = inline
            .iter()
            .map(|c| {
                json!({
                    "path": c.path,
                    "line": c.line,
                    "side": "RIGHT",
                    "body": c.body,
                })
            })
            .collect();
        let value = self.post(
            &format!("/repos/{repo}/pulls/{number}/reviews"),
            json!({
                "commit_id": commit_id,
                "body": body,
                "event": "COMMENT",
                "comments": comments,
            }),
        )?;
        value
            .get("id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| "create review missing id".into())
    }

    fn find_issue_comment_by_marker(
        &self,
        repo: &str,
        number: u64,
        marker: &str,
    ) -> Result<Option<u64>, String> {
        let value = self.get(&format!(
            "/repos/{repo}/issues/{number}/comments?per_page=100"
        ))?;
        let rows = value.as_array().ok_or("issue comments were not an array")?;
        Ok(rows.iter().find_map(|row| {
            let body = row.get("body").and_then(|v| v.as_str()).unwrap_or("");
            body.contains(marker)
                .then(|| row.get("id").and_then(|v| v.as_u64()))
                .flatten()
        }))
    }

    fn create_issue_comment(&self, repo: &str, number: u64, body: &str) -> Result<u64, String> {
        let value = self.post(
            &format!("/repos/{repo}/issues/{number}/comments"),
            json!({ "body": body }),
        )?;
        value
            .get("id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| "create comment missing id".into())
    }

    fn convert_to_draft(&self, repo: &str, number: u64) -> Result<(), String> {
        // REST convert endpoint.
        let _ = self.put(
            &format!("/repos/{repo}/pulls/{number}/convert_to_draft"),
            json!({}),
        )?;
        Ok(())
    }
}

pub(crate) fn expected_login_for(
    topology: &Topology,
    project: &ShepherdProjectConfig,
) -> Result<String, String> {
    let name = project
        .auth
        .as_deref()
        .ok_or_else(|| format!("{} has no shepherd auth", project.name))?;
    let auth = topology
        .shepherd
        .auth
        .iter()
        .find(|auth| auth.name == name)
        .ok_or_else(|| format!("{name} auth is missing"))?;
    auth.expected_login
        .as_deref()
        .filter(|login| !login.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{name} expected_login is required for review writes"))
}

pub(crate) fn publish_for_pr(
    ledger: &mut TaskLedger,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
    coding_root: &Path,
    task_id: &str,
    pr_number: u64,
    shadow: bool,
    changed_lines: &BTreeSet<(String, u32)>,
) -> Result<(), String> {
    if shadow {
        return Ok(());
    }
    let expected_login = expected_login_for(topology, project)?;
    let port = GithubReviewPublishPort::new(token)?;
    reconcile_publication(
        ledger,
        &port,
        &PublishRequest {
            task_id,
            repository: &project.repository,
            pr_number,
            expected_login: &expected_login,
            coding_root,
            changed_lines,
        },
    )
}
