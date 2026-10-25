//! `[api]` — who may reach the HTTP control plane, and from where.
//!
//! Before this table the daemon bound `0.0.0.0:4201` with no authentication at all, so every
//! host on the tailnet, every browser tab (CORS was permissive), and every shell a coding run
//! opened could resolve an approval. This section is the declaration half of closing that:
//! where to listen, which credentials exist, and which browser origins may talk to the API.
//!
//! **No credential value is ever written here.** A credential names an environment variable or
//! a file that holds the token, the same posture `HookConfig::secret_ref` and
//! `ProviderProfile::api_key_env` already take (Decision 10). A config file lands in git far
//! more easily than an env var does.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where the daemon listens when nothing overrides it.
///
/// Loopback, not `0.0.0.0`. Reaching the daemon from another host on the tailnet is a decision
/// the operator makes on purpose, through [`ApiConfig::bind`] or `LIBERADO_BIND`, and it is
/// refused unless a credential is configured.
pub const DEFAULT_BIND: &str = "127.0.0.1:4201";

/// Where a token's value comes from. Exactly one of the two must be set.
///
/// `file` exists for Docker and systemd secrets, which arrive as a mounted path rather than an
/// environment variable; the daemon's own environment is visible to every child process it
/// spawns, and a coding run is one of those children.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CredentialRef {
    /// Name of the environment variable holding the token. Never the token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Path to a file whose entire contents (trimmed) are the token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
}

impl CredentialRef {
    /// An env-var reference, for tests and for building a config in code.
    pub fn env(name: impl Into<String>) -> Self {
        Self {
            env: Some(name.into()),
            file: None,
        }
    }

    /// A secret-file reference.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            env: None,
            file: Some(path.into()),
        }
    }

    /// `true` when neither source is named — the serde default, meaning "not configured".
    pub fn is_unset(&self) -> bool {
        self.env.is_none() && self.file.is_none()
    }

    /// Why this reference cannot be resolved, as an operator-facing sentence, or `None` when it
    /// names exactly one source.
    ///
    /// Naming both is refused rather than given a precedence rule: a deployment that sets both
    /// has two beliefs about which token is live, and silently picking one is how a rotated
    /// secret keeps letting the old token in.
    pub fn problem(&self) -> Option<String> {
        match (self.env.as_deref(), self.file.as_ref()) {
            (None, None) => Some("names neither `env` nor `file`".to_string()),
            (Some(_), Some(_)) => Some("names both `env` and `file`; set exactly one".to_string()),
            (Some(name), None) if name.trim().is_empty() => Some("`env` is empty".to_string()),
            (None, Some(path)) if path.as_os_str().is_empty() => {
                Some("`file` is empty".to_string())
            }
            _ => None,
        }
    }
}

/// One non-human API client: an agent, a tool, an MCP server, the CLI.
///
/// Per-client rather than one shared token so a leaked credential names the thing that leaked
/// it and can be rotated alone. A client token authenticates; it does not make its holder the
/// owner, and the owner-only routes refuse it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiClientConfig {
    /// Stable identifier for this client, used in logs and refusal messages. Not a secret.
    pub name: String,
    /// Where this client's token comes from.
    pub token: CredentialRef,
}

/// `[api]` — bind address, credentials, and the browser origins allowed to call the API.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    /// `host:port` to listen on. `None` → [`DEFAULT_BIND`], unless `LIBERADO_BIND` or the
    /// legacy `LIBERADO_PORT` says otherwise. A non-loopback value needs a credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
    /// The owner's credential. It alone may call the owner-only routes, and it alone can be
    /// exchanged for a browser session at `POST /api/auth/login`.
    #[serde(skip_serializing_if = "CredentialRef::is_unset")]
    pub human: CredentialRef,
    /// Per-client credentials. Each authenticates; none is the owner.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clients: Vec<ApiClientConfig>,
    /// Browser origins allowed to call the API cross-origin, exact-matched.
    ///
    /// Empty — the default — means no CORS response headers at all, which is what makes a
    /// `fetch` from another origin fail. The list exists for the `dx serve` dev loop, where the
    /// WebUI is served from `:8080` and the API from `:4201`. It is not a wildcard and there is
    /// no setting that makes it one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_origins: Vec<String>,
}

impl ApiConfig {
    /// Whether any credential is declared at all. A daemon with none is loopback-only.
    pub fn has_credentials(&self) -> bool {
        !self.human.is_unset() || !self.clients.is_empty()
    }

    /// Operator-facing problems with this table, as full sentences. Empty means valid.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if let Some(why) = self.human.problem()
            && !self.human.is_unset()
        {
            problems.push(format!("api.human {why}"));
        }
        let mut seen: Vec<&str> = Vec::new();
        for client in &self.clients {
            let name = client.name.trim();
            if name.is_empty() {
                problems.push("api.clients has an entry with an empty `name`".to_string());
            } else if seen.contains(&name) {
                problems.push(format!("api.clients has two entries named '{name}'"));
            } else {
                seen.push(name);
            }
            if let Some(why) = client.token.problem() {
                problems.push(format!("api.clients['{}'].token {why}", client.name));
            }
        }
        for origin in &self.allowed_origins {
            if origin.trim() == "*" {
                problems.push(
                    "api.allowed_origins contains '*'; list exact origins instead".to_string(),
                );
            }
        }
        problems
    }
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
