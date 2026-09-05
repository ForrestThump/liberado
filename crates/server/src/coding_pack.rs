//! Coding-pack composition, split from the server root for module-health boundaries.

use std::path::Path;
use std::sync::Arc;

use liberado_provider::Provider;

pub(super) fn load_server_config(
    dir: Option<&Path>,
) -> Result<
    (
        liberado_bootstrap::Config,
        liberado_bootstrap::ConfigProvenance,
        liberado_coder_core::CoderTuning,
    ),
    Box<dyn std::error::Error>,
> {
    let (config, provenance) = liberado_bootstrap::load_config(dir)?;
    let coder_tuning = coder_tuning_from_config(&config)?;
    Ok((config, provenance, coder_tuning))
}

pub(super) fn coder_tuning_from_config(
    config: &liberado_bootstrap::Config,
) -> Result<liberado_coder_core::CoderTuning, Box<dyn std::error::Error>> {
    Ok(liberado_coder_core::CoderTuning::from_value(
        config.tuning.coder.as_ref(),
    )?)
}

/// Build the coding pack for the goal hub when a provider is attached.
///
/// Invalid coding tuning is a startup error. Silently keeping defaults can route work to the wrong
/// worker or model.
pub(super) fn build_coding_pack(
    provider: Option<&Arc<dyn Provider>>,
    config: &liberado_bootstrap::Config,
    coder_tuning: &liberado_coder_core::CoderTuning,
) -> Option<Arc<liberado_coder_agent::CodingSessionPack>> {
    let provider = provider?;
    let work_parent = liberado_bootstrap::data_dir().join("goal-workspaces");
    let _ = std::fs::create_dir_all(&work_parent);
    let mut pack = liberado_coder_agent::CodingSessionPack::new(provider.clone(), work_parent)
        .with_max_concurrent_coding_subagents(
            config.tuning.dispatch.max_concurrent_coding_subagents,
        )
        .with_tuning(coder_tuning.clone());
    if let Some(factory) = liberado_bootstrap::CoderRoleProviderFactory::for_config(config) {
        pack = pack.with_provider_factory(Arc::new(factory));
    }
    Some(Arc::new(pack))
}
