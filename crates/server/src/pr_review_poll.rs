//! Poll-loop mechanics for the PR review observer.

use std::collections::BTreeMap;
use std::time::Duration;

use liberado_coder_core::ReviewWorkerConfig;
use liberado_coder_core::pr_review::bounded_pages;
use liberado_config::{ShepherdProjectConfig, Topology};
use reqwest::Client;
use serde_json::Value;

pub(crate) fn spawn(
    topology: &Topology,
    review_workers: BTreeMap<String, ReviewWorkerConfig>,
) -> Option<tokio::task::JoinHandle<()>> {
    topology
        .shepherd
        .review
        .enabled
        .then(|| tokio::spawn(poll_forever(topology.clone(), review_workers)))
}

async fn poll_forever(topology: Topology, review_workers: BTreeMap<String, ReviewWorkerConfig>) {
    let mut ticker =
        tokio::time::interval(Duration::from_secs(topology.shepherd.review.poll_seconds));
    if !topology.shepherd.review.reconcile_on_start {
        ticker.tick().await;
    }
    loop {
        ticker.tick().await;
        if let Err(error) = reconcile(&topology, &review_workers).await {
            tracing::warn!(%error, "PR review observer pass failed");
        }
    }
}

async fn reconcile(
    topology: &Topology,
    review_workers: &BTreeMap<String, ReviewWorkerConfig>,
) -> Result<(), String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    for project in &topology.shepherd.projects {
        poll_configured(&client, topology, project, review_workers).await?;
    }
    Ok(())
}

async fn poll_configured(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    review_workers: &BTreeMap<String, ReviewWorkerConfig>,
) -> Result<(), String> {
    let Some(token) = configured_token(topology, project)? else {
        return Ok(());
    };
    poll_pages(client, topology, project, &token, review_workers).await
}

fn configured_token(
    topology: &Topology,
    project: &ShepherdProjectConfig,
) -> Result<Option<String>, String> {
    project
        .controller
        .as_ref()
        .map(|_| token_for(topology, project))
        .transpose()
}

async fn poll_pages(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
    review_workers: &BTreeMap<String, ReviewWorkerConfig>,
) -> Result<(), String> {
    for page in bounded_pages(topology.shepherd.review.max_pages) {
        if !poll_page(client, topology, project, token, page, review_workers).await? {
            break;
        }
    }
    Ok(())
}

fn token_for(topology: &Topology, project: &ShepherdProjectConfig) -> Result<String, String> {
    let name = project
        .auth
        .as_deref()
        .ok_or_else(|| format!("{} has no shepherd auth", project.name))?;
    let auth = topology
        .shepherd
        .auth
        .iter()
        .find(|auth| auth.name == name)
        .ok_or_else(|| format!("{} names unknown shepherd auth {name}", project.name))?;
    std::env::var(&auth.token_ref).map_err(|_| format!("{} is not set", auth.token_ref))
}

async fn poll_page(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
    page: usize,
    review_workers: &BTreeMap<String, ReviewWorkerConfig>,
) -> Result<bool, String> {
    let path = format!(
        "/repos/{}/pulls?state=all&sort=updated&direction=desc&per_page={}&page={page}",
        project.repository, topology.shepherd.review.page_size
    );
    let rows = super::pr_review_observer::github(client, token, &path).await?;
    observe_rows(client, topology, project, token, &rows, review_workers).await?;
    Ok(rows.as_array().map(|rows| rows.len()).unwrap_or(0) >= topology.shepherd.review.page_size)
}

async fn observe_rows(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
    rows: &Value,
    review_workers: &BTreeMap<String, ReviewWorkerConfig>,
) -> Result<(), String> {
    let Some(rows) = rows.as_array() else {
        return Err("pull list was not an array".into());
    };
    for row in rows {
        super::pr_review_observer::observe_pr(
            client,
            topology,
            project,
            token,
            row,
            review_workers,
        )
        .await?;
    }
    Ok(())
}
