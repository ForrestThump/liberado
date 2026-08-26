//! `liberado delegate kickback | merge | checks` — the delegator's review verdicts
//! (plan §10). Split from the parent router for module health, same as the other
//! task-addressed subcommands.
//!
//! A kickback is one action, two records: the instruction travels to the worker via
//! the answers endpoint, and — when forge flags are given — a review comment lands on
//! the PR for the human-visible audit trail. Merge is delegator-only and verifies the
//! spec's required checks first: the forge claims green, the delegator confirms.

use std::error::Error;

use liberado_delegate_contract::{Answer, AnswerAck, TaskRecord};
use liberado_forge::{CheckState, ForgeClient, MergeMethod, PrRef, RepoPath};

use super::{Connection, checked, connection, emit, fetch_task, request, routes};

/// `liberado delegate kickback <task-id> --body TEXT [--comment] [--forge-url URL]
/// [--forge-token-env VAR]` — send the run back with instructions.
pub(super) async fn run(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    let (positional, flags) =
        parse_kickback_args(&mut args).map_err(|error| super::usage(&error))?;
    let [task_id] = positional.as_slice() else {
        return Err(super::usage("kickback needs exactly one <task-id>").into());
    };
    if flags.body.trim().is_empty() {
        return Err(super::usage("kickback needs a non-empty --body").into());
    }
    let connection = connection(&flags.base)?;

    // The instruction is the action; the comment is audit. Order matters: record on
    // the forge first so the review trail exists even if the worker is unreachable.
    if flags.comment {
        let record = fetch_task(&connection, task_id).await?;
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
struct KickbackFlags {
    body: String,
    comment: bool,
    forge_url: Option<String>,
    forge_token_env: Option<String>,
    base: super::Flags,
}

fn parse_kickback_args(
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
fn pr_ref_from_url(url: &str, repo: RepoPath) -> Result<PrRef, String> {
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
async fn verify_required_checks(
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
    let (positional, flags) = parse_merge_args(&mut args).map_err(|error| super::usage(&error))?;
    let [task_id] = positional.as_slice() else {
        return Err(super::usage("merge needs exactly one <task-id>").into());
    };
    let connection = connection(&flags.base)?;

    let record = fetch_task(&connection, task_id).await?;
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

struct MergeFlags {
    base: super::Flags,
    kickback: KickbackFlags,
    method: MergeMethod,
}

impl Default for MergeFlags {
    fn default() -> Self {
        Self {
            base: super::Flags::default(),
            kickback: KickbackFlags::default(),
            method: MergeMethod::Squash,
        }
    }
}

fn parse_merge_args(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kickback_grammar_splits_flags_from_positionals() {
        let (positionals, flags) = parse_kickback_args(
            ["01T", "--body", "fix it", "--comment"]
                .iter()
                .map(|s| s.to_string()),
        )
        .expect("parse");
        assert_eq!(positionals, vec!["01T".to_string()]);
        assert_eq!(flags.body, "fix it");
        assert!(flags.comment);
    }

    #[test]
    fn merge_method_defaults_to_squash_and_parses_the_three_verbs() {
        let (_, flags) = parse_merge_args(["01T"].iter().map(|s| s.to_string())).expect("parse");
        assert_eq!(flags.method, MergeMethod::Squash);
        let (_, flags) =
            parse_merge_args(["01T", "--method", "rebase"].iter().map(|s| s.to_string()))
                .expect("parse");
        assert_eq!(flags.method, MergeMethod::Rebase);
        assert!(parse_merge_args(["--method", "wizard"].iter().map(|s| s.to_string())).is_err());
    }

    #[test]
    fn pr_urls_parse_into_references_like_the_worker_does() {
        let pr = pr_ref_from_url("https://gitea.example/o/r/pulls/9", RepoPath("o/r".into()))
            .expect("parses");
        assert_eq!(pr.number, 9);
        assert!(pr_ref_from_url("https://gitea.example/o/r", RepoPath("o/r".into())).is_err());
    }
}

#[cfg(test)]
mod merge_guard_tests {
    use super::verify_required_checks;
    use liberado_forge::{
        CheckState, CheckStates, ForgeClient, ForgeError, MergeCommit, MergeMethod, OpenPr, PrRef,
        RepoPath,
    };

    struct StubForge {
        overall: CheckState,
    }

    #[async_trait::async_trait]
    impl ForgeClient for StubForge {
        async fn open_pr(&self, _req: &OpenPr) -> Result<PrRef, ForgeError> {
            Err(ForgeError::Shape("unused".into()))
        }
        async fn comment(&self, _pr: &PrRef, _body: &str) -> Result<(), ForgeError> {
            Ok(())
        }
        async fn checks(&self, _pr: &PrRef, names: &[String]) -> Result<CheckStates, ForgeError> {
            Ok(CheckStates {
                overall: self.overall,
                named: names.iter().map(|n| (n.clone(), self.overall)).collect(),
            })
        }
        async fn merge(&self, _pr: &PrRef, _m: MergeMethod) -> Result<MergeCommit, ForgeError> {
            Ok(MergeCommit { sha: "abc".into() })
        }
    }

    fn pr() -> PrRef {
        PrRef {
            repo: RepoPath("o/r".into()),
            number: 1,
            url: "http://f/o/r/pulls/1".into(),
        }
    }

    /// The D3 gate: a delegator-side refusal when required checks are red.
    #[tokio::test]
    async fn failing_required_checks_block_the_merge_with_names() {
        let forge = StubForge {
            overall: CheckState::Failure,
        };
        let error = verify_required_checks(
            &forge,
            &pr(),
            &["ci/linux".to_string(), "ci/windows".to_string()],
        )
        .await
        .expect_err("red checks must refuse");
        assert!(error.contains("not green"), "{error}");
        assert!(error.contains("ci/linux"), "{error}");
    }

    #[tokio::test]
    async fn green_checks_and_empty_requirements_pass() {
        let green = StubForge {
            overall: CheckState::Success,
        };
        verify_required_checks(&green, &pr(), &["ci".to_string()])
            .await
            .expect("green passes");
        // No requirements at all: nothing to verify, no forge call needed.
        let red = StubForge {
            overall: CheckState::Failure,
        };
        verify_required_checks(&red, &pr(), &[])
            .await
            .expect("empty passes");
    }
}
