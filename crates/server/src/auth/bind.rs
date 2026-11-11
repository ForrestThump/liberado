//! Which address the daemon listens on, and when it refuses to.
//!
//! The daemon used to bind `0.0.0.0` with no option and no authentication, so every host that
//! could route to the box had the control plane. The default is now loopback, reaching further
//! is a decision the operator writes down, and the decision is refused unless a credential
//! exists to go with it.

use std::net::{IpAddr, SocketAddr};

use liberado_config::{ApiConfig, DEFAULT_BIND};

/// Env var that opts in to a non-default listen address, e.g. a tailnet IP.
pub const BIND_ENV: &str = "LIBERADO_BIND";

/// Legacy port-only override. Kept because Compose and the deploy scripts set it; it can only
/// move the port, never the interface.
pub const PORT_ENV: &str = "LIBERADO_PORT";

/// Why the daemon will not listen where it was told to.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("{0}")]
    Invalid(String),
}

/// Where the bind address came from, for the startup log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindSource {
    BindEnv,
    Config,
    PortEnv,
    Default,
}

impl BindSource {
    pub fn describe(self) -> &'static str {
        match self {
            BindSource::BindEnv => BIND_ENV,
            BindSource::Config => "topology.toml [api] bind",
            BindSource::PortEnv => PORT_ENV,
            BindSource::Default => "built-in default",
        }
    }
}

/// The resolved listen address and where the decision came from.
#[derive(Debug, Clone)]
pub struct BindPlan {
    pub addr: SocketAddr,
    pub source: BindSource,
}

/// Resolve the listen address: `LIBERADO_BIND`, then `[api] bind`, then `LIBERADO_PORT` on
/// loopback, then the built-in default.
///
/// `LIBERADO_PORT` deliberately keeps its loopback host. It exists in every Compose file and
/// deploy script in the repo, and the one thing a port setting must not be able to do by
/// accident is widen the interface.
pub fn resolve_bind(
    bind_env: Option<&str>,
    port_env: Option<&str>,
    api: &ApiConfig,
) -> Result<BindPlan, BindError> {
    let (text, source) = match (
        bind_env.map(str::trim).filter(|s| !s.is_empty()),
        api.bind.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        port_env.map(str::trim).filter(|s| !s.is_empty()),
    ) {
        (Some(bind), _, _) => (bind.to_string(), BindSource::BindEnv),
        (None, Some(bind), _) => (bind.to_string(), BindSource::Config),
        (None, None, Some(port)) => (format!("127.0.0.1:{port}"), BindSource::PortEnv),
        (None, None, None) => (DEFAULT_BIND.to_string(), BindSource::Default),
    };
    let addr = parse_bind(&text, source)?;
    Ok(BindPlan { addr, source })
}

fn parse_bind(text: &str, source: BindSource) -> Result<SocketAddr, BindError> {
    // `localhost` is what a human writes and what no IP parser accepts. Everything else must
    // be a literal address, so the refusal below cannot be dodged by a name that resolves
    // somewhere unexpected.
    let normalised = match text.strip_prefix("localhost:") {
        Some(port) => format!("127.0.0.1:{port}"),
        None => text.to_string(),
    };
    normalised.parse::<SocketAddr>().map_err(|_| {
        BindError::Invalid(format!(
            "{} is '{text}', which is not a literal <ip>:<port> (e.g. 127.0.0.1:4201 or \
             100.64.0.3:4201)",
            source.describe()
        ))
    })
}

/// `true` when nothing outside this machine can reach the address.
pub fn is_loopback(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}

/// Refuse a listen address that other hosts can reach while no credential is configured.
///
/// Loud at startup, not quiet at the first request. The failure this prevents — a reachable,
/// unauthenticated control plane — is invisible from the inside: everything works, and that is
/// the symptom.
pub fn ensure_bind_is_credentialed(
    plan: &BindPlan,
    has_credentials: bool,
) -> Result<(), BindError> {
    if is_loopback(&plan.addr) || has_credentials {
        return Ok(());
    }
    Err(BindError::Invalid(format!(
        "refusing to listen on {} ({}): it is reachable from other hosts and no API credential \
         is configured. Either bind a loopback address, or declare [api] human = {{ env = \
         \"<VAR>\" }} in topology.toml and set that variable. Anyone who can route to this \
         address could otherwise resolve approvals and change the chat profile.",
        plan.addr,
        plan.source.describe()
    )))
}

#[cfg(test)]
#[path = "bind_tests.rs"]
mod tests;
