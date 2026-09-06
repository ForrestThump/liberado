//! Configuration owned by the pull-request shepherd.

use liberado_common::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ShepherdConfig {
    #[serde(default)]
    pub projects: Vec<ShepherdProjectConfig>,
    #[serde(default)]
    pub review: ShepherdReviewConfig,
    #[serde(default)]
    pub auth: Vec<ShepherdAuthConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShepherdAuthConfig {
    pub name: String,
    pub kind: String,
    pub token_ref: String,
    #[serde(default)]
    pub webhook_secret_ref: Option<String>,
    #[serde(default)]
    pub expected_login: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShepherdReviewConfig {
    pub enabled: bool,
    pub delivery: String,
    pub poll_seconds: u64,
    pub reconcile_on_start: bool,
    pub max_pages: usize,
    pub page_size: usize,
}

impl Default for ShepherdReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            delivery: "poll".into(),
            poll_seconds: 120,
            reconcile_on_start: true,
            max_pages: 10,
            page_size: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShepherdProjectConfig {
    pub name: String,
    pub repository: String,
    pub coding_project: String,
    #[serde(default = "default_main_branch")]
    pub base_branch: String,
    #[serde(default = "default_shepherd_profile")]
    pub profile: String,
    #[serde(default)]
    pub check_names: Vec<String>,
    #[serde(default)]
    pub max_kickbacks: Option<usize>,
    #[serde(default)]
    pub cold_reviews: Option<usize>,
    #[serde(default)]
    pub cold_review_max_turns: Option<u32>,
    #[serde(default)]
    pub max_concurrent_goals: Option<usize>,
    #[serde(default)]
    pub poll_seconds: Option<u64>,
    #[serde(default)]
    pub controller: Option<String>,
    #[serde(default)]
    pub review_profile: Option<String>,
    #[serde(default)]
    pub gate: Option<String>,
    #[serde(default)]
    pub auth: Option<String>,
}

fn default_main_branch() -> String {
    "main".into()
}
fn default_shepherd_profile() -> String {
    "coding-unattended".into()
}

pub(super) fn validate(shepherd: &ShepherdConfig) -> Result<()> {
    if shepherd.review.enabled
        && (shepherd.review.delivery != "poll"
            || shepherd.review.poll_seconds == 0
            || shepherd.review.max_pages == 0
            || !(1..=100).contains(&shepherd.review.page_size))
    {
        return Err(Error::Config(
            "shepherd.review polling bounds are invalid".into(),
        ));
    }
    let names: std::collections::BTreeSet<_> = shepherd
        .auth
        .iter()
        .map(|auth| auth.name.as_str())
        .collect();
    if names.len() != shepherd.auth.len() {
        return Err(Error::Config("shepherd auth names must be unique".into()));
    }
    for auth in &shepherd.auth {
        if auth.kind != "token"
            || !env_ref(&auth.token_ref)
            || auth
                .webhook_secret_ref
                .as_deref()
                .is_some_and(|value| !env_ref(value))
        {
            return Err(Error::Config(format!(
                "shepherd auth '{}' must use environment references",
                auth.name
            )));
        }
    }
    for project in &shepherd.projects {
        if project.controller.as_deref() == Some("liberado-shepherd")
            && (project.cold_reviews != Some(0)
                || project.check_names.is_empty()
                || project.review_profile.as_deref().is_none_or(str::is_empty)
                || project.gate.as_deref() != Some("ready_and_green_tip")
                || project
                    .auth
                    .as_deref()
                    .is_none_or(|name| !names.contains(name)))
        {
            return Err(Error::Config(format!(
                "shepherd project '{}' has invalid native-review policy",
                project.name
            )));
        }
    }
    Ok(())
}

fn env_ref(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}
