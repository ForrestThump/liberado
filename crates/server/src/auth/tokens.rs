//! Resolving `[api]` credential references into live tokens, and matching a presented one.

use liberado_config::{ApiConfig, CredentialRef};

/// Shortest token the daemon will accept from config.
///
/// Not a strength estimate — it is a typo guard. A one-character token in a secret file is
/// almost always a truncated write or an echo that lost its content, and the failure it causes
/// (an open control plane that looks closed) is the one this whole module exists to prevent.
const MIN_TOKEN_LEN: usize = 16;

/// A token value, held so it cannot be printed by accident.
///
/// `Debug` is the realistic leak path: a `tracing` field, a `dbg!`, an error that derives
/// `Debug` over a struct that happens to contain one. The value is reachable only through
/// [`SecretToken::matches`], which is a comparison, not a getter.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretToken(String);

impl std::fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretToken(<redacted>)")
    }
}

impl SecretToken {
    /// Constant-time comparison against a presented token.
    ///
    /// Length is compared first and is not treated as secret, which is the usual scope of a
    /// constant-time check for a shared secret. Matches `hooks::constant_time_eq`.
    pub fn matches(&self, presented: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), presented.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }
}

/// Who is calling. Established once per request by the auth middleware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// The owner: the human token, or a browser session minted from it.
    Human {
        /// `true` when the credential was the session cookie rather than a bearer token. Only
        /// a cookie can be replayed by a browser the owner did not intend, so only a cookie
        /// needs the CSRF check.
        via_cookie: bool,
    },
    /// A declared non-human client, named by its `[[api.clients]]` entry.
    Client { name: String },
}

impl Principal {
    /// `true` for the owner, by either credential. The owner-only routes ask exactly this.
    pub fn is_human(&self) -> bool {
        matches!(self, Principal::Human { .. })
    }

    /// The name to put in a log line. Never a token, and never a session id.
    pub fn label(&self) -> &str {
        match self {
            Principal::Human { .. } => "human",
            Principal::Client { name } => name,
        }
    }
}

/// Why the `[api]` credentials could not be resolved at boot.
#[derive(Debug, thiserror::Error)]
pub enum AuthConfigError {
    /// The message names the setting and the source, never the value.
    #[error("{0}")]
    Invalid(String),
}

/// Every credential the API accepts, resolved from the environment and from secret files once
/// at boot.
///
/// Resolution happens at startup rather than per request so a missing secret is a daemon that
/// refuses to start, not a daemon that serves 401 to its own WebUI for a week.
#[derive(Debug, Default)]
pub struct ApiAuth {
    human: Option<SecretToken>,
    clients: Vec<(String, SecretToken)>,
}

impl ApiAuth {
    /// Resolve `[api]` into live tokens.
    pub fn resolve(api: &ApiConfig) -> Result<Self, AuthConfigError> {
        let human = match api.human.is_unset() {
            true => None,
            false => Some(read_credential(&api.human, "api.human")?),
        };
        let mut clients = Vec::new();
        for client in &api.clients {
            let label = format!("api.clients['{}'].token", client.name);
            let token = read_credential(&client.token, &label)?;
            if human.as_ref().is_some_and(|h| h.matches(&token.0)) {
                return Err(AuthConfigError::Invalid(format!(
                    "{label} resolves to the same token as api.human; a client token must not \
                     carry owner authority"
                )));
            }
            if clients
                .iter()
                .any(|(_, t): &(String, SecretToken)| t.matches(&token.0))
            {
                return Err(AuthConfigError::Invalid(format!(
                    "{label} resolves to the same token as another client; per-client tokens \
                     exist so a leak names its holder"
                )));
            }
            clients.push((client.name.clone(), token));
        }
        Ok(Self { human, clients })
    }

    /// Construct directly from already-known token values. Tests only — production goes through
    /// [`ApiAuth::resolve`] so no code path can hard-code a credential.
    #[cfg(test)]
    pub fn from_values(human: Option<&str>, clients: &[(&str, &str)]) -> Self {
        Self {
            human: human.map(|t| SecretToken(t.to_string())),
            clients: clients
                .iter()
                .map(|(n, t)| (n.to_string(), SecretToken(t.to_string())))
                .collect(),
        }
    }

    /// `true` when no credential is configured at all. Such a daemon is loopback-only.
    pub fn is_open(&self) -> bool {
        self.human.is_none() && self.clients.is_empty()
    }

    /// `true` when an owner credential exists, so a browser can sign in.
    pub fn has_human_credential(&self) -> bool {
        self.human.is_some()
    }

    /// Match a presented bearer token against every configured credential.
    ///
    /// The owner is checked first so a client entry can never shadow it. `None` means the token
    /// matched nothing; the caller answers 401 and does not say which part was wrong.
    pub fn authenticate_bearer(&self, presented: &str) -> Option<Principal> {
        if self.human.as_ref().is_some_and(|t| t.matches(presented)) {
            return Some(Principal::Human { via_cookie: false });
        }
        self.clients
            .iter()
            .find(|(_, token)| token.matches(presented))
            .map(|(name, _)| Principal::Client { name: name.clone() })
    }

    /// Whether `presented` is the owner token. Used by `POST /api/auth/login`.
    pub fn is_human_token(&self, presented: &str) -> bool {
        self.human.as_ref().is_some_and(|t| t.matches(presented))
    }
}

/// Read one credential reference. The error names the setting and the source, never the value.
fn read_credential(cred: &CredentialRef, label: &str) -> Result<SecretToken, AuthConfigError> {
    if let Some(why) = cred.problem() {
        return Err(AuthConfigError::Invalid(format!("{label} {why}")));
    }
    let raw = match (&cred.env, &cred.file) {
        (Some(name), _) => std::env::var(name).map_err(|_| {
            AuthConfigError::Invalid(format!(
                "{label} names environment variable '{name}', which is not set"
            ))
        })?,
        (_, Some(path)) => std::fs::read_to_string(path).map_err(|e| {
            AuthConfigError::Invalid(format!(
                "{label} names file '{}', which could not be read: {e}",
                path.display()
            ))
        })?,
        // `problem()` already refused this shape.
        (None, None) => unreachable!("credential with no source passed problem()"),
    };
    let token = raw.trim().to_string();
    if token.len() < MIN_TOKEN_LEN {
        return Err(AuthConfigError::Invalid(format!(
            "{label} resolves to fewer than {MIN_TOKEN_LEN} characters; generate one with \
             `openssl rand -hex 32`"
        )));
    }
    Ok(SecretToken(token))
}

#[cfg(test)]
#[path = "tokens_tests.rs"]
mod tests;
