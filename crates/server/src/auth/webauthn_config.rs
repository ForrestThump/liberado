//! Build a configured [`webauthn_rs::prelude::Webauthn`] from `[api.webauthn]`.

use liberado_config::WebAuthnConfig;
use url::Url;
use webauthn_rs::prelude::{Webauthn, WebauthnBuilder};

pub struct WebAuthnState {
    pub webauthn: Webauthn,
    pub origins: Vec<String>,
}

pub fn build_webauthn(cfg: &WebAuthnConfig) -> Result<WebAuthnState, String> {
    if let Some(problem) = cfg.problems().first() {
        return Err(problem.clone());
    }
    let primary = cfg
        .origins
        .first()
        .ok_or_else(|| "api.webauthn origins is empty".to_string())?;
    let rp_origin = Url::parse(primary).map_err(|_| format!("invalid origin '{primary}'"))?;
    let mut builder = WebauthnBuilder::new(cfg.rp_id.trim(), &rp_origin)
        .map_err(|_| "webauthn builder rejected rp_id and primary origin".to_string())?
        .rp_name(&cfg.rp_name);
    for origin in cfg.origins.iter().skip(1) {
        let url = Url::parse(origin).map_err(|_| format!("invalid origin '{origin}'"))?;
        builder = builder.append_allowed_origin(&url);
    }
    let webauthn = builder
        .build()
        .map_err(|_| "webauthn build failed".to_string())?;
    Ok(WebAuthnState {
        webauthn,
        origins: cfg.origins.clone(),
    })
}

/// Whether an `Origin` header or request authority matches configured WebAuthn origins or loopback.
pub fn origin_allowed(origin_or_host: &str, configured: &[String]) -> bool {
    if configured.iter().any(|o| o == origin_or_host) {
        return true;
    }
    let host = authority_from_origin(origin_or_host);
    if is_loopback_host(&host) {
        return true;
    }
    configured.iter().any(|o| authority_from_origin(o) == host)
}

pub fn authority_from_origin(origin: &str) -> String {
    let rest = origin.split("://").nth(1).unwrap_or(origin);
    rest.split('/').next().unwrap_or(rest).to_string()
}

pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
        || host.starts_with("127.0.0.1:")
        || host.starts_with("[::1]:")
        || host.starts_with("localhost:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_are_allowed_without_listing() {
        assert!(origin_allowed("http://127.0.0.1:4201", &[]));
    }
}
