//! Native HTTP clients: bearer token from the environment.
//!
//! Loopback daemons with no `[api]` table accept unauthenticated callers; when credentials are
//! configured, set `LIBERADO_API_TOKEN` to a per-client token from topology (`[[api.clients]]`).

/// Env var every native Liberado HTTP client reads for `Authorization: Bearer …`.
pub const API_TOKEN_ENV: &str = "LIBERADO_API_TOKEN";

/// Token value for outbound API calls, if the operator configured one.
pub fn bearer_token_from_env() -> Option<String> {
    std::env::var(API_TOKEN_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}
