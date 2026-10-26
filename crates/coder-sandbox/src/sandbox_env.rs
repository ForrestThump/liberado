//! Environment variables that must not reach a sandboxed command.

/// API and owner credentials must not reach a sandboxed shell — a coding run could exfiltrate them.
pub fn sandbox_env_is_denied(name: &str) -> bool {
    let key = name.trim();
    if key.is_empty() {
        return true;
    }
    let upper = key.to_uppercase();
    if upper == "LIBERADO_API_TOKEN" {
        return true;
    }
    if upper.starts_with("LIBERADO_") && upper.contains("TOKEN") {
        return true;
    }
    upper == "AUTHORIZATION"
}
