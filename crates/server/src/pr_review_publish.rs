//! GitHub publication adapter for Slice 3. Credentials stay in shepherd.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use crate::pr_review_diff::changed_lines;
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

    fn post_graphql(&self, body: Value) -> Result<Value, String> {
        self.client
            .post("https://api.github.com/graphql")
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
        let pr = self.get(&format!("/repos/{repo}/pulls/{number}"))?;
        let id = pr
            .get("node_id")
            .and_then(Value::as_str)
            .ok_or("pull request node id is missing")?;
        let response = self.post_graphql(json!({
            "query": "mutation($id: ID!) { convertPullRequestToDraft(input: {pullRequestId: $id}) { pullRequest { isDraft } } }",
            "variables": { "id": id },
        }))?;
        graphql_draft_confirmed(&response)
    }
}

fn graphql_draft_confirmed(response: &Value) -> Result<(), String> {
    if let Some(errors) = response.get("errors").filter(|errors| !errors.is_null()) {
        return Err(format!("convert-to-draft GraphQL error: {errors}"));
    }
    (response.pointer("/data/convertPullRequestToDraft/pullRequest/isDraft")
        == Some(&Value::Bool(true)))
    .then_some(())
    .ok_or_else(|| "convert-to-draft response did not confirm draft state".into())
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

pub(crate) struct PublishForPrParams<'a> {
    pub(crate) ledger: &'a mut TaskLedger,
    pub(crate) topology: &'a Topology,
    pub(crate) project: &'a ShepherdProjectConfig,
    pub(crate) token: &'a str,
    pub(crate) coding_root: &'a Path,
    pub(crate) task_id: &'a str,
    pub(crate) pr_number: u64,
    pub(crate) shadow: bool,
    pub(crate) changed_lines: &'a BTreeSet<(String, u32)>,
}

pub(crate) struct PublishObservation {
    pub(crate) ledger: TaskLedger,
    pub(crate) topology: Topology,
    pub(crate) project: ShepherdProjectConfig,
    pub(crate) token: String,
    pub(crate) coding_root: std::path::PathBuf,
    pub(crate) task_id: String,
    pub(crate) pr_number: u64,
    pub(crate) base_sha: String,
    pub(crate) head_sha: String,
    pub(crate) shadow: bool,
}

pub(crate) async fn publish_for_observation(mut params: PublishObservation) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let changed = if params.shadow {
            BTreeSet::new()
        } else {
            changed_lines(&params.coding_root, &params.base_sha, &params.head_sha)?
        };
        publish_for_pr(PublishForPrParams {
            ledger: &mut params.ledger,
            topology: &params.topology,
            project: &params.project,
            token: &params.token,
            coding_root: &params.coding_root,
            task_id: &params.task_id,
            pr_number: params.pr_number,
            shadow: params.shadow,
            changed_lines: &changed,
        })
    })
    .await
    .map_err(|error| format!("PR review publication task failed: {error}"))?
}

pub(crate) fn publish_for_pr(params: PublishForPrParams<'_>) -> Result<(), String> {
    if params.shadow {
        return Ok(());
    }
    let expected_login = expected_login_for(params.topology, params.project)?;
    let port = GithubReviewPublishPort::new(params.token)?;
    reconcile_publication(
        params.ledger,
        &port,
        &PublishRequest {
            task_id: params.task_id,
            repository: &params.project.repository,
            pr_number: params.pr_number,
            expected_login: &expected_login,
            coding_root: params.coding_root,
            changed_lines: params.changed_lines,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_draft_response_must_confirm_the_effect() {
        let confirmed = json!({
            "data": {"convertPullRequestToDraft": {"pullRequest": {"isDraft": true}}}
        });
        assert_eq!(graphql_draft_confirmed(&confirmed), Ok(()));
        assert!(graphql_draft_confirmed(&json!({"data": null})).is_err());
        assert!(graphql_draft_confirmed(&json!({"errors": [{"message": "denied"}]})).is_err());
    }

    #[test]
    fn draft_conversion_uses_the_documented_graphql_mutation() {
        let source = include_str!("pr_review_publish.rs");
        assert!(source.contains("convertPullRequestToDraft"));
        let graphql_endpoint = ["https://api.github.com", "graphql"].join("/");
        assert!(source.contains(&graphql_endpoint));
        let removed_rest_route = ["pulls/{number}", "convert_to_draft"].join("/");
        assert!(!source.contains(&removed_rest_route));
    }

    #[tokio::test]
    async fn blocking_publication_work_runs_off_the_async_worker() {
        let async_thread = std::thread::current().id();
        let blocking_thread = tokio::task::spawn_blocking(|| std::thread::current().id())
            .await
            .unwrap();
        assert_ne!(async_thread, blocking_thread);
        let blocking_call = ["spawn", "blocking"].join("_");
        assert!(include_str!("pr_review_publish.rs").contains(&blocking_call));
    }

    #[test]
    fn production_publisher_loads_changed_lines() {
        let changed_line_call = format!("{}(&params.coding_root", ["changed", "lines"].join("_"));
        assert!(include_str!("pr_review_publish.rs").contains(&changed_line_call));
    }
}
