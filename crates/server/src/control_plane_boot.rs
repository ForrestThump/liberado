//! Resolve API credentials and the listen address before any component starts.

use std::sync::Arc;

use liberado_config::ApiConfig;
use tracing::info;

use crate::auth;

/// Resolve `[api]` credentials into the live guard. Separated so the error says `API auth`
/// before the loader's own message, which names a setting but not the subsystem.
pub fn resolve_api_guard(api: &ApiConfig) -> Result<auth::ApiGuard, Box<dyn std::error::Error>> {
    let resolved = auth::ApiAuth::resolve(api).map_err(|e| format!("API auth: {e}"))?;
    Ok(auth::ApiGuard::new(resolved, api.allowed_origins.clone()))
}

/// One startup line saying how closed the control plane actually is.
pub fn log_api_posture(guard: &auth::ApiGuard, plan: &auth::BindPlan) {
    info!(
        addr = %plan.addr,
        source = plan.source.describe(),
        loopback = auth::bind::is_loopback(&plan.addr),
        authenticated = !guard.auth.is_open(),
        human_login = guard.auth.has_human_credential(),
        cross_origin_allowed = guard.allowed_origins.len(),
        "API control plane"
    );
}

/// Credentials, bind plan, and the non-loopback refusal — everything [`run`] needs before assembly.
pub fn prepare_control_plane(
    api: &ApiConfig,
) -> Result<(Arc<auth::ApiGuard>, auth::BindPlan), Box<dyn std::error::Error>> {
    let guard = Arc::new(resolve_api_guard(api)?);
    let plan = auth::resolve_bind(
        std::env::var(auth::bind::BIND_ENV).ok().as_deref(),
        std::env::var(auth::bind::PORT_ENV).ok().as_deref(),
        api,
    )?;
    auth::ensure_bind_is_credentialed(&plan, !guard.auth.is_open())?;
    log_api_posture(&guard, &plan);
    Ok((guard, plan))
}
