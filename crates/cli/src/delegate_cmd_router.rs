//! Central routing for `liberado delegate <verb>` (submit excepted — it owns its
//! grammar in the parent). One arm per verb keeps adding subcommands free; the verbs
//! themselves live in sibling modules.

use std::error::Error;

use super::answer_cmd;
use super::review_cmd;
use super::watch_cmd;
use super::{cmd_cancel, cmd_health, cmd_status};

/// The delegator's own provider profile (plan §4): LIBERADO_CONFIG_DIR +
/// LIBERADO_CODER_PROVIDER name one `[[providers]]` entry. Missing config dir means
/// the default topology; a named-but-undeclared provider is an error, never a fallback.
pub(super) fn resolve_provider_profile() -> Result<liberado_config_loader::ProviderProfile, String>
{
    use liberado_config_loader::Topology;
    let dir = std::env::var("LIBERADO_CONFIG_DIR")
        .ok()
        .map(std::path::PathBuf::from);
    let topology = match &dir {
        Some(dir) => {
            let raw = std::fs::read_to_string(dir.join("topology.toml")).map_err(|error| {
                format!("read {}: {error}", dir.join("topology.toml").display())
            })?;
            toml::from_str::<Topology>(&raw).map_err(|error| format!("parse topology: {error}"))?
        }
        None => Topology::default(),
    };
    let name =
        std::env::var("LIBERADO_CODER_PROVIDER").unwrap_or_else(|_| topology.provider.clone());
    topology
        .providers
        .into_iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| format!("provider '{name}' is not declared in topology.providers"))
}

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
        // The review family routes as one so growing it costs the dispatcher nothing.
        other => review_cmd::route(other, args).await,
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
