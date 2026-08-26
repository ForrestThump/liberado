//! The cold-review engine behind `liberado delegate review` (plan §10 step 2).
//!
//! Change surface only: the PR diff goes to the reviewer with no goal text and no
//! trace — author context cannot leak into the verdict. Split from the command file
//! for module health.

use std::error::Error;

use liberado_delegate_contract::{Answer, AnswerAck, TaskRecord};
use liberado_forge::{CheckState, ForgeClient, MergeMethod, PrRef, RepoPath};
use liberado_provider::Provider;

use super::{Connection, checked, emit, request, routes};

/// Assemble one completion provider from the delegator's own config stack — same
/// resolution rules the worker uses (topology profile, env key), so a delegator that
/// can submit can also review.
/// Same resolution rules as the worker's `provider_profile`: LIBERADO_CONFIG_DIR +
/// LIBERADO_CODER_PROVIDER name one topology entry. The model is the profile's own
/// default — review is not role-configured on the delegator side.
pub(super) fn review_provider() -> Result<std::sync::Arc<dyn Provider>, String> {
    let profile = super::super::router::resolve_provider_profile()?;
    let api_key = std::env::var(&profile.api_key_env).map_err(|_| {
        format!(
            "{} is required for provider '{}'",
            profile.api_key_env, profile.name
        )
    })?;
    Ok(std::sync::Arc::new(
        liberado_provider_openai_compat::OpenAiCompatibleProvider::new(
            &api_key,
            &profile.default_model,
            &profile.base_url,
        ),
    ))
}

pub(super) async fn report(
    forge: &dyn ForgeClient,
    provider: &dyn Provider,
    record: &TaskRecord,
) -> Result<String, String> {
    use liberado_delegate_contract::TaskStatus;

    let url = match &record.status {
        TaskStatus::PrOpened { url } => url.clone(),
        other => return Err(format!("task is {other:?}; nothing to review")),
    };
    let pr = pr_ref_from_url(&url, RepoPath(record.spec.repository.clone()))?;
    let diff = forge.diff(&pr).await.map_err(|e| e.to_string())?;

    let surface = liberado_coder_agent::ChangeSurface {
        diff,
        file_excerpts: Vec::new(),
    };
    let request = liberado_coder_agent::build_cold_review_request(
        &surface,
        &liberado_coder_agent::ForbiddenAuthorContext::default(),
        None,
        ".",
    )
    .map_err(|e| format!("cold review refused: {e}"))?;

    let response = provider
        .complete(liberado_provider::CompletionRequest {
            messages: vec![
                liberado_provider::Message::system(request.system_prompt),
                liberado_provider::Message::user(request.user_message),
            ],
            tools: Vec::new(),
            response_format: Default::default(),
            temperature: None,
            max_tokens: None,
            model: None,
            reasoning: None,
        })
        .await
        .map_err(|e| format!("reviewer model failed: {e}"))?;
    let content = response.content.unwrap_or_default();

    let findings = parse_findings(&content)?;
    let filter = liberado_coder_agent::filter_findings(&surface, &findings);
    let decision = liberado_coder_agent::decide_after_filter(&filter, 0);
    Ok(render_review(&url, &filter, &decision))
}

pub(super) fn parse_findings(
    content: &str,
) -> Result<Vec<liberado_coder_agent::ColdFinding>, String> {
    #[derive(serde::Deserialize)]
    struct Findings {
        #[serde(default)]
        findings: Vec<liberado_coder_agent::ColdFinding>,
    }
    // Reviewers wrap their JSON in prose; take the outermost braces and parse that.
    let json_start = content
        .find('{')
        .ok_or("reviewer returned no JSON object")?;
    let json_end = content.rfind('}').ok_or("reviewer JSON is not closed")?;
    if json_end < json_start {
        return Err("reviewer JSON braces do not balance".into());
    }
    let parsed: Findings = serde_json::from_str(content[json_start..=json_end].trim())
        .map_err(|error| format!("reviewer findings did not parse: {error}"))?;
    Ok(parsed.findings)
}

fn render_review(
    url: &str,
    filter: &liberado_coder_agent::FilterResult,
    decision: &liberado_coder_agent::StageDecision,
) -> String {
    use liberado_coder_agent::StageDecision as D;
    let mut text = format!("Cold review of {url}\n\n");
    if filter.retained.is_empty() && filter.dropped.is_empty() {
        text.push_str("No findings. Verdict: ready.\n");
        return text;
    }
    for finding in &filter.retained {
        text.push_str(&format!(
            "- [{}] {}{}\n  why: {}\n",
            severity_tag(finding),
            finding.title,
            finding
                .path
                .as_ref()
                .map(|p| format!(" ({p})"))
                .unwrap_or_default(),
            finding.why
        ));
    }
    if !filter.dropped.is_empty() {
        text.push_str(&format!(
            "\n{} finding(s) dropped (off-surface or uncited).\n",
            filter.dropped.len()
        ));
    }
    match decision {
        D::RunFixRound { findings } => text.push_str(&format!(
            "\nVerdict: KICK BACK ({} retained finding(s)).\n",
            findings.len()
        )),
        D::EscalateToHuman { reason } => {
            text.push_str(&format!("\nVerdict: ESCALATE ({reason}).\n"))
        }
        D::NoFixNeeded => text.push_str("\nVerdict: READY (nothing actionable retained).\n"),
    }
    text
}

fn severity_tag(finding: &liberado_coder_agent::ColdFinding) -> &'static str {
    use liberado_coder_agent::Severity::*;
    match finding.severity {
        High => "high",
        Medium => "medium",
        Low => "low",
    }
}

pub(super) fn verdict_summary(report: &str) -> String {
    // The PR comment keeps the verdict + finding lines; the header URL is redundant there.
    report
        .lines()
        .filter(|l| !l.starts_with("Cold review of"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `liberado delegate review <task-id> [--post]` — cold review over the PR diff
/// (plan §10 step 2): the change surface only, no goal, no trace. The verdict prints;
/// `--post` also leaves it on the PR as the human-visible record.
pub(super) async fn run_review(
    mut args: impl Iterator<Item = String>,
) -> Result<(), Box<dyn Error>> {
    let (positional, flags) =
        parse_kickback_args(&mut args).map_err(|error| super::super::usage(&error))?;
    let [task_id] = positional.as_slice() else {
        return Err(super::super::usage("review needs exactly one <task-id>").into());
    };
    let connection = super::connection(&flags.base)?;
    let record = super::fetch_task(&connection, task_id).await?;
    let forge = forge_client(&flags)?;
    let provider = review_provider()?;

    let review_report = report(forge.as_ref(), provider.as_ref(), &record).await?;
    emit(&review_report);
    maybe_post(forge.as_ref(), flags.comment, &record, &review_report).await
}

/// `--post`: leave the verdict on the PR. The comment carries the verdict, not the
/// full report — findings stay in the CLI output where the human is reading.
async fn maybe_post(
    forge: &dyn ForgeClient,
    wanted: bool,
    record: &TaskRecord,
    review_report: &str,
) -> Result<(), Box<dyn Error>> {
    if !wanted {
        return Ok(());
    }
    let url = match &record.status {
        liberado_delegate_contract::TaskStatus::PrOpened { url } => url.clone(),
        other => return Err(format!("task is {other:?}; nothing to comment on").into()),
    };
    let pr = pr_ref_from_url(&url, RepoPath(record.spec.repository.clone()))?;
    forge
        .comment(&pr, &verdict_summary(review_report))
        .await
        .map_err(|e| e.to_string())?;
    emit("verdict posted to the pull request");
    Ok(())
}

pub(super) async fn run(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    let (positional, flags) =
        parse_kickback_args(&mut args).map_err(|error| super::super::usage(&error))?;
    let [task_id] = positional.as_slice() else {
        return Err(super::super::usage("kickback needs exactly one <task-id>").into());
    };
    if flags.body.trim().is_empty() {
        return Err(super::super::usage("kickback needs a non-empty --body").into());
    }
    let connection = super::connection(&flags.base)?;

    // The instruction is the action; the comment is audit. Order matters: record on
    // the forge first so the review trail exists even if the worker is unreachable.
    if flags.comment {
        let record = super::fetch_task(&connection, task_id).await?;
        comment_on_pr(&flags, &record, &flags.body).await?;
    }
    // The round number in the tag is informational; the worker derives the real
    // round from its own journal so restarts cannot desync the two sides.
    let ack = post_instruction(
        &connection,
        &Answer::instruction(0, flags.body.clone()),
        task_id,
    )
    .await?;
    emit(if ack.delivered {
        "kickback accepted; the run resumes on its branch"
    } else {
        "worker refused or deferred the kickback"
    });
    Ok(())
}

#[derive(Debug, Default)]
pub(super) struct KickbackFlags {
    pub(super) body: String,
    pub(super) comment: bool,
    pub(super) forge_url: Option<String>,
    pub(super) forge_token_env: Option<String>,
    pub(super) base: super::super::Flags,
}

pub(super) fn parse_kickback_args(
    mut args: impl Iterator<Item = String>,
) -> Result<(Vec<String>, KickbackFlags), String> {
    let mut positionals = Vec::new();
    let mut flags = KickbackFlags::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--body" => flags.body = args.next().ok_or("--body needs a value")?,
            "--comment" => flags.comment = true,
            "--forge-url" => {
                flags.forge_url = Some(args.next().ok_or("--forge-url needs a value")?)
            }
            "--forge-token-env" => {
                flags.forge_token_env = Some(args.next().ok_or("--forge-token-env needs a value")?)
            }
            "--endpoint" => {
                flags.base.endpoint = Some(args.next().ok_or("--endpoint needs a value")?)
            }
            "--token-env" => {
                flags.base.token_env = Some(args.next().ok_or("--token-env needs a value")?)
            }
            other if other.starts_with('-') => return Err(format!("unknown flag: {other}")),
            other => positionals.push(other.to_string()),
        }
    }
    Ok((positionals, flags))
}

async fn post_instruction(
    connection: &Connection,
    answer: &Answer,
    task_id: &str,
) -> Result<AnswerAck, String> {
    let response = request(
        connection,
        reqwest::Method::POST,
        &routes::task_answers(task_id),
    )
    .json(answer)
    .send()
    .await
    .map_err(|error| format!("post kickback: {error}"))?;
    serde_json::from_str(&checked(response).await?).map_err(|error| error.to_string())
}

/// Post the review comment through the forge abstraction. The PR reference comes
/// from the worker's own record, so the delegator never guesses numbers.
async fn comment_on_pr(
    flags: &KickbackFlags,
    record: &TaskRecord,
    body: &str,
) -> Result<(), Box<dyn Error>> {
    use liberado_delegate_contract::TaskStatus;
    let url = match &record.status {
        TaskStatus::PrOpened { url } => url.clone(),
        other => return Err(format!("task is {other:?}; nothing to comment on").into()),
    };
    let forge = forge_client(flags)?;
    let repo = RepoPath(record.spec.repository.clone());
    let pr = pr_ref_from_url(&url, repo)?;
    forge.comment(&pr, body).await.map_err(|e| e.to_string())?;
    Ok(())
}

fn forge_client(flags: &KickbackFlags) -> Result<std::sync::Arc<dyn ForgeClient>, String> {
    // Insecure TLS is an explicit opt-in mirroring the worker's flag, for LAN forges
    // behind a private CA. Never a default.
    let insecure = matches!(
        std::env::var("LIBERADO_FORGE_INSECURE_TLS").as_deref(),
        Ok("1") | Ok("true")
    );
    let url = flags
        .forge_url
        .clone()
        .or_else(|| std::env::var("LIBERADO_FORGE_URL").ok())
        .ok_or("no forge url: pass --forge-url or set LIBERADO_FORGE_URL")?;
    let var = flags
        .forge_token_env
        .clone()
        .unwrap_or_else(|| "LIBERADO_FORGE_TOKEN".into());
    let token = std::env::var(&var).map_err(|_| format!("{var} is not set"))?;
    liberado_forge::gitea::GiteaForge::with_tls(url.trim_end_matches('/'), &token, insecure)
        .map(std::sync::Arc::new)
        .map(|f| f as std::sync::Arc<dyn ForgeClient>)
        .map_err(|e| e.to_string())
}

/// Same parse rule the worker uses on its own minted urls (`…/pulls/<n>`).
pub(super) fn pr_ref_from_url(url: &str, repo: RepoPath) -> Result<PrRef, String> {
    let number: u64 = url
        .rsplit('/')
        .next()
        .and_then(|tail| tail.parse().ok())
        .ok_or_else(|| format!("cannot parse PR number from {url}"))?;
    Ok(PrRef {
        repo,
        number,
        url: url.to_string(),
    })
}

/// §10 step 1: the delegator verifies what the forge claims instead of trusting it.
/// An empty requirement list passes trivially; any non-success named check fails the
/// merge with the offending names in the message.
pub(super) async fn verify_required_checks(
    forge: &dyn ForgeClient,
    pr: &PrRef,
    required: &[String],
) -> Result<(), String> {
    if required.is_empty() {
        return Ok(());
    }
    let states = forge
        .checks(pr, required)
        .await
        .map_err(|e| e.to_string())?;
    if states.overall == CheckState::Success {
        emit("required checks green");
        return Ok(());
    }
    let failed: Vec<String> = states
        .named
        .iter()
        .filter(|(_, state)| *state != CheckState::Success)
        .map(|(name, _)| name.clone())
        .collect();
    Err(format!(
        "required checks are not green ({failed:?}); refusing to merge"
    ))
}

/// `liberado delegate merge <task-id> [--method squash]` — verify required checks,
/// then merge. Delegator-only by design (§14): the worker never holds merge rights.
pub(super) async fn run_merge(
    mut args: impl Iterator<Item = String>,
) -> Result<(), Box<dyn Error>> {
    let (positional, flags) =
        parse_merge_args(&mut args).map_err(|error| super::super::usage(&error))?;
    let [task_id] = positional.as_slice() else {
        return Err(super::super::usage("merge needs exactly one <task-id>").into());
    };
    let connection = super::connection(&flags.base)?;

    let record = super::fetch_task(&connection, task_id).await?;
    let (pr_url, repository) = match &record.status {
        liberado_delegate_contract::TaskStatus::PrOpened { url } => {
            (url.clone(), record.spec.repository.clone())
        }
        other => return Err(format!("task is {other:?}; only a PR-opened task can merge").into()),
    };

    // Verify what the forge claims before acting on it (§10 step 1).
    let forge = forge_client(&flags.kickback)?;
    let pr = pr_ref_from_url(&pr_url, RepoPath(repository))?;
    verify_required_checks(forge.as_ref(), &pr, &record.spec.acceptance.required_checks)
        .await
        .map_err(|error| -> Box<dyn Error> { error.into() })?;

    let commit = forge
        .merge(&pr, flags.method)
        .await
        .map_err(|e| e.to_string())?;
    emit(&format!("merged as {}", commit.sha));
    Ok(())
}

pub(super) struct MergeFlags {
    pub(super) base: super::super::Flags,
    pub(super) kickback: KickbackFlags,
    pub(super) method: MergeMethod,
}

impl Default for MergeFlags {
    fn default() -> Self {
        Self {
            base: super::super::Flags::default(),
            kickback: KickbackFlags::default(),
            method: MergeMethod::Squash,
        }
    }
}

pub(super) fn parse_merge_args(
    mut args: impl Iterator<Item = String>,
) -> Result<(Vec<String>, MergeFlags), String> {
    let mut positionals = Vec::new();
    let mut flags = MergeFlags::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--method" => {
                let value = args.next().ok_or("--method needs a value")?;
                flags.method = match value.as_str() {
                    "merge" => MergeMethod::Merge,
                    "squash" => MergeMethod::Squash,
                    "rebase" => MergeMethod::Rebase,
                    other => return Err(format!("unknown merge method: {other}")),
                };
            }
            "--forge-url" => {
                flags.kickback.forge_url = Some(args.next().ok_or("--forge-url needs a value")?)
            }
            "--forge-token-env" => {
                flags.kickback.forge_token_env =
                    Some(args.next().ok_or("--forge-token-env needs a value")?)
            }
            "--endpoint" => {
                flags.base.endpoint = Some(args.next().ok_or("--endpoint needs a value")?)
            }
            "--token-env" => {
                flags.base.token_env = Some(args.next().ok_or("--token-env needs a value")?)
            }
            other if other.starts_with('-') => return Err(format!("unknown flag: {other}")),
            other => positionals.push(other.to_string()),
        }
    }
    Ok((positionals, flags))
}
