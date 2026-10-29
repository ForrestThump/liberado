//! Native HTTP clients: bearer token from the environment.
//!
//! Loopback daemons with no `[api]` table accept unauthenticated callers; when credentials are
//! configured, set `LIBERADO_API_TOKEN` to a per-client token from topology (`[[api.clients]]`).
//!
//! Do not log [`authorization_value`] or [`bearer_token_from_env`]. The strings are secrets.

/// Env var every native Liberado HTTP client reads for `Authorization: Bearer …`.
pub const API_TOKEN_ENV: &str = "LIBERADO_API_TOKEN";

/// Token value for outbound API calls, if the operator configured one.
pub fn bearer_token_from_env() -> Option<String> {
    normalized_token(std::env::var(API_TOKEN_ENV).ok().as_deref())
}

/// `Authorization` header value for a daemon call, or `None` when the token is missing or blank.
///
/// HTTP stacks that cannot share a request-builder type attach this string themselves.
/// `None`, `""`, and whitespace send no header.
pub fn authorization_value(token: Option<&str>) -> Option<String> {
    normalized_token(token).map(|token| format!("Bearer {token}"))
}

fn normalized_token(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::authorization_value;

    #[test]
    fn authorization_value_is_bearer_when_a_token_is_present() {
        assert_eq!(
            authorization_value(Some("fake-api-token-000")),
            Some("Bearer fake-api-token-000".to_string())
        );
        assert_eq!(
            authorization_value(Some("  fake-api-token-000  ")),
            Some("Bearer fake-api-token-000".to_string())
        );
    }

    #[test]
    fn authorization_value_is_absent_for_a_blank_token() {
        assert_eq!(authorization_value(None), None);
        assert_eq!(authorization_value(Some("")), None);
        assert_eq!(authorization_value(Some("   ")), None);
    }
}
