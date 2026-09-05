//! Coding-pack composition, split from the server root for module-health boundaries.

use std::sync::Arc;

use liberado_provider::Provider;

/// Build the coding pack for the goal hub when a provider is attached.
///
/// Invalid coding tuning is a startup error. Silently keeping defaults can route work to the wrong
/// worker or model.
pub(super) fn build_coding_pack(
    provider: Option<&Arc<dyn Provider>>,
    config: &liberado_bootstrap::Config,
) -> Result<Option<Arc<liberado_coder_agent::CodingSessionPack>>, Box<dyn std::error::Error>> {
    let Some(provider) = provider else {
        return Ok(None);
    };
    let coder_tuning = liberado_coder_core::CoderTuning::from_value(config.tuning.coder.as_ref())?;
    let work_parent = liberado_bootstrap::data_dir().join("goal-workspaces");
    let _ = std::fs::create_dir_all(&work_parent);
    let mut pack = liberado_coder_agent::CodingSessionPack::new(provider.clone(), work_parent)
        .with_max_concurrent_coding_subagents(
            config.tuning.dispatch.max_concurrent_coding_subagents,
        )
        .with_tuning(coder_tuning);
    if let Some(factory) = liberado_bootstrap::CoderRoleProviderFactory::for_config(config) {
        pack = pack.with_provider_factory(Arc::new(factory));
    }
    Ok(Some(Arc::new(pack)))
}
