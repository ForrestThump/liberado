//! Central routing for `liberado delegate <verb>` (submit excepted — it owns its
//! grammar in the parent). One arm per verb keeps adding subcommands free; the verbs
//! themselves live in sibling modules.

use std::error::Error;

use super::answer_cmd;
use super::review_cmd;
use super::watch_cmd;
use super::{cmd_cancel, cmd_health, cmd_status, usage};

pub(super) async fn dispatch(
    name: &str,
    args: &mut impl Iterator<Item = String>,
) -> Result<(), Box<dyn Error>> {
    match name {
        "status" => cmd_status(args).await,
        "cancel" => cmd_cancel(args).await,
        "health" => cmd_health(args).await,
        "watch" => watch_cmd::run(args).await,
        "answer" => answer_cmd::run(args).await,
        "kickback" => review_cmd::run(args).await,
        "merge" => review_cmd::run_merge(args).await,
        other => Err(usage(&format!("unknown or missing subcommand: {other}")).into()),
    }
}

#[cfg(test)]
mod tests {
    /// Unknown verbs are a usage error, not a panic — the CLI boundary contract.
    #[tokio::test]
    async fn unknown_verbs_are_usage_errors() {
        let err = super::dispatch("frobnicate", &mut std::iter::empty())
            .await
            .expect_err("must refuse");
        assert!(err.to_string().contains("frobnicate"), "{err}");
    }
}
