//! Matrix environment. Secrets stay in the environment. Nothing here reads a config file.
//!
//! The listener starts only when homeserver, both tokens, the owner, and the main room are
//! all set. None of them set means the daemon stays as it is. Some of them set is a warning
//! and Matrix stays off.

use std::net::SocketAddr;

/// The five variables that turn Matrix on.
pub const REQUIRED_VARS: [&str; 5] = [
    "LIBERADO_MATRIX_HOMESERVER",
    "LIBERADO_MATRIX_AS_TOKEN",
    "LIBERADO_MATRIX_HS_TOKEN",
    "LIBERADO_MATRIX_OWNER",
    "LIBERADO_MATRIX_MAIN_ROOM",
];

const DEFAULT_LISTEN: &str = "0.0.0.0:4202";
const DEFAULT_MAIN_USER: &str = "liberado";
const DEFAULT_AS_ID: &str = "liberado";
/// Application-service bot localpart. The registration uses the same name.
pub const DEFAULT_SENDER_LOCALPART: &str = "liberado_bridge";

/// Values read from the environment, or supplied by a test.
#[derive(Debug, Clone, Default)]
pub struct EnvSnapshot {
    pub homeserver: Option<String>,
    pub server_name: Option<String>,
    pub as_token: Option<String>,
    pub hs_token: Option<String>,
    pub owner: Option<String>,
    pub main_user: Option<String>,
    pub main_room: Option<String>,
    pub listen: Option<String>,
    pub as_id: Option<String>,
    pub sender_localpart: Option<String>,
}

/// What the process should do with the snapshot.
#[derive(Debug)]
pub enum Activation {
    /// None of the required variables are set.
    Off,
    /// At least one required variable is set, and at least one is missing.
    Partial { missing: Vec<&'static str> },
    /// The required variables are set, but a value cannot be used.
    Invalid { reason: String },
    /// Matrix can try its startup check.
    On(Box<MatrixSettings>),
}

/// Non-secret view plus the two tokens. `Debug` omits the tokens.
#[derive(Clone)]
pub struct MatrixSettings {
    pub homeserver: String,
    pub server_name: String,
    pub as_token: String,
    pub hs_token: String,
    pub owner: String,
    pub main_user: String,
    pub main_room: String,
    pub listen: SocketAddr,
    pub as_id: String,
    pub sender_localpart: String,
}

impl std::fmt::Debug for MatrixSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MatrixSettings")
            .field("homeserver", &self.homeserver)
            .field("server_name", &self.server_name)
            .field("owner", &self.owner)
            .field("main_user", &self.main_user)
            .field("main_room", &self.main_room)
            .field("listen", &self.listen)
            .field("as_id", &self.as_id)
            .field("sender_localpart", &self.sender_localpart)
            .finish()
    }
}

impl EnvSnapshot {
    /// Read the Matrix variables. Empty values count as unset.
    pub fn from_env() -> Self {
        Self {
            homeserver: var("LIBERADO_MATRIX_HOMESERVER"),
            server_name: var("LIBERADO_MATRIX_SERVER_NAME"),
            as_token: var("LIBERADO_MATRIX_AS_TOKEN"),
            hs_token: var("LIBERADO_MATRIX_HS_TOKEN"),
            owner: var("LIBERADO_MATRIX_OWNER"),
            main_user: var("LIBERADO_MATRIX_MAIN_USER"),
            main_room: var("LIBERADO_MATRIX_MAIN_ROOM"),
            listen: var("LIBERADO_MATRIX_AS_LISTEN"),
            as_id: var("LIBERADO_MATRIX_AS_ID"),
            sender_localpart: var("LIBERADO_MATRIX_SENDER_LOCALPART"),
        }
    }
}

fn var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Decide whether Matrix starts. Missing names are variable names, never values.
pub fn activation(env: &EnvSnapshot) -> Activation {
    let missing = missing_required(env);
    if missing.len() == REQUIRED_VARS.len() {
        return Activation::Off;
    }
    if !missing.is_empty() {
        return Activation::Partial { missing };
    }
    match MatrixSettings::from_snapshot(env) {
        Ok(settings) => Activation::On(Box::new(settings)),
        Err(reason) => Activation::Invalid { reason },
    }
}

/// Comma-separated variable names for the one warning line.
pub fn missing_names(missing: &[&str]) -> String {
    missing.join(", ")
}

fn missing_required(env: &EnvSnapshot) -> Vec<&'static str> {
    let mut missing = Vec::new();
    push_if_blank(&mut missing, REQUIRED_VARS[0], env.homeserver.as_deref());
    push_if_blank(&mut missing, REQUIRED_VARS[1], env.as_token.as_deref());
    push_if_blank(&mut missing, REQUIRED_VARS[2], env.hs_token.as_deref());
    push_if_blank(&mut missing, REQUIRED_VARS[3], env.owner.as_deref());
    push_if_blank(&mut missing, REQUIRED_VARS[4], env.main_room.as_deref());
    missing
}

fn push_if_blank(missing: &mut Vec<&'static str>, name: &'static str, value: Option<&str>) {
    if value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .is_none()
    {
        missing.push(name);
    }
}

impl MatrixSettings {
    fn from_snapshot(env: &EnvSnapshot) -> Result<Self, String> {
        let owner = required_text(env.owner.as_deref(), "LIBERADO_MATRIX_OWNER")?;
        let server_name = resolve_server_name(env.server_name.as_deref(), &owner)?;
        let localpart = env
            .main_user
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(DEFAULT_MAIN_USER);
        Ok(Self {
            homeserver: trim_slash(required_text(
                env.homeserver.as_deref(),
                "LIBERADO_MATRIX_HOMESERVER",
            )?),
            server_name: server_name.clone(),
            as_token: required_text(env.as_token.as_deref(), "LIBERADO_MATRIX_AS_TOKEN")?,
            hs_token: required_text(env.hs_token.as_deref(), "LIBERADO_MATRIX_HS_TOKEN")?,
            owner,
            main_user: main_mxid(localpart, &server_name),
            main_room: required_text(env.main_room.as_deref(), "LIBERADO_MATRIX_MAIN_ROOM")?,
            listen: resolve_listen(env.listen.as_deref())?,
            as_id: or_default(env.as_id.as_deref(), DEFAULT_AS_ID),
            sender_localpart: or_default(env.sender_localpart.as_deref(), DEFAULT_SENDER_LOCALPART),
        })
    }
}

fn required_text(value: Option<&str>, name: &str) -> Result<String, String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{name} is unset"))
}

fn or_default(value: Option<&str>, default: &str) -> String {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn trim_slash(url: String) -> String {
    url.trim_end_matches('/').to_string()
}

fn resolve_server_name(explicit: Option<&str>, owner: &str) -> Result<String, String> {
    if let Some(name) = explicit.map(str::trim).filter(|text| !text.is_empty()) {
        return Ok(name.to_string());
    }
    owner
        .split_once(':')
        .map(|(_, domain)| domain.trim().to_string())
        .filter(|domain| !domain.is_empty())
        .ok_or_else(|| {
            "LIBERADO_MATRIX_SERVER_NAME is unset and LIBERADO_MATRIX_OWNER has no server name"
                .to_string()
        })
}

fn resolve_listen(value: Option<&str>) -> Result<SocketAddr, String> {
    let text = value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(DEFAULT_LISTEN);
    text.parse()
        .map_err(|_| "LIBERADO_MATRIX_AS_LISTEN is not host:port".to_string())
}

/// `@localpart:server`, or the value unchanged when it is already a full user id.
pub fn main_mxid(configured: &str, server_name: &str) -> String {
    let configured = configured.trim();
    if configured.starts_with('@') && configured.contains(':') {
        return configured.to_string();
    }
    let localpart = configured.trim_start_matches('@');
    format!("@{localpart}:{server_name}")
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
