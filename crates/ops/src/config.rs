use std::path::{Path, PathBuf};

use serde::Deserialize;

const OPS_CONFIG_ENV: &str = "LIBERADO_OPS_CONFIG";

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct OpsConfig {
    pub homelab: Option<HomelabConfig>,
    pub development: DevelopmentConfig,
    pub paseo: PaseoConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct HomelabConfig {
    pub ssh_target: String,
    pub build_dir: String,
    pub compose_file: String,
    pub container: String,
    pub container_binary: String,
    pub image: String,
    pub api_url: String,
    pub webui_remote_dir: String,
    pub webui_local_dir: PathBuf,
    pub latency_journal: String,
    pub connect_timeout_secs: u64,
    pub deploy_lock_timeout_secs: u64,
    pub allow_invalid_tls: bool,
}

impl Default for HomelabConfig {
    fn default() -> Self {
        Self {
            ssh_target: String::new(),
            build_dir: "liberado-build".into(),
            compose_file: "homelab/services/liberado/docker-compose.yml".into(),
            container: "liberado".into(),
            container_binary: "/usr/local/bin/liberado".into(),
            image: "liberado:dev".into(),
            api_url: String::new(),
            webui_remote_dir: "homelab/services/liberado/webui-dist".into(),
            webui_local_dir: PathBuf::from("target/dx/liberado-webui/release/web/public"),
            latency_journal: "/data/latency/events.jsonl".into(),
            connect_timeout_secs: 15,
            deploy_lock_timeout_secs: 1_800,
            allow_invalid_tls: false,
        }
    }
}

impl HomelabConfig {
    pub fn validate(&self) -> Result<(), OpsConfigError> {
        for (name, value) in [
            ("homelab.ssh_target", self.ssh_target.as_str()),
            ("homelab.build_dir", self.build_dir.as_str()),
            ("homelab.compose_file", self.compose_file.as_str()),
            ("homelab.container", self.container.as_str()),
            ("homelab.container_binary", self.container_binary.as_str()),
            ("homelab.image", self.image.as_str()),
            ("homelab.api_url", self.api_url.as_str()),
            ("homelab.webui_remote_dir", self.webui_remote_dir.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(OpsConfigError::Invalid(format!("{name} is required")));
            }
            if value.contains('\n') || value.contains('\r') || value.contains('\0') {
                return Err(OpsConfigError::Invalid(format!(
                    "{name} contains a control character"
                )));
            }
        }
        if !self.api_url.starts_with("http://") && !self.api_url.starts_with("https://") {
            return Err(OpsConfigError::Invalid(
                "homelab.api_url must start with http:// or https://".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct DevelopmentConfig {
    pub daemon_url: String,
    pub daemon_port: u16,
    pub webui_dev_port: u16,
    pub readiness_timeout_secs: u64,
}

impl Default for DevelopmentConfig {
    fn default() -> Self {
        Self {
            daemon_url: "http://127.0.0.1:4201".into(),
            daemon_port: 4201,
            webui_dev_port: 8080,
            readiness_timeout_secs: 90,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PaseoConfig {
    pub home: Option<PathBuf>,
    pub provider_name: String,
    pub label: String,
    pub description: String,
    pub binary: Option<PathBuf>,
    pub model: String,
    pub config_dir: Option<PathBuf>,
}

impl Default for PaseoConfig {
    fn default() -> Self {
        Self {
            home: None,
            provider_name: "liberado".into(),
            label: "Liberado".into(),
            description: "Liberado multi-mode agent over ACP".into(),
            binary: None,
            model: "deepseek/deepseek-v4-pro".into(),
            config_dir: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpsConfigError {
    #[error(
        "no ops configuration found; pass --config, set {OPS_CONFIG_ENV}, or create .liberado/ops.toml"
    )]
    Missing,
    #[error("read ops config {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse ops config {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("invalid ops config: {0}")]
    Invalid(String),
}

impl OpsConfig {
    pub fn load(
        explicit: Option<&Path>,
        repository: &Path,
    ) -> Result<(Self, PathBuf), OpsConfigError> {
        let path = resolve_path(explicit, repository).ok_or(OpsConfigError::Missing)?;
        let text = std::fs::read_to_string(&path).map_err(|source| OpsConfigError::Read {
            path: path.clone(),
            source,
        })?;
        let parsed = toml::from_str(&text).map_err(|source| OpsConfigError::Parse {
            path: path.clone(),
            source: Box::new(source),
        })?;
        Ok((parsed, path))
    }

    pub fn homelab(&self) -> Result<&HomelabConfig, OpsConfigError> {
        let config = self.homelab.as_ref().ok_or_else(|| {
            OpsConfigError::Invalid("[homelab] is required for deployment".into())
        })?;
        config.validate()?;
        Ok(config)
    }
}

fn resolve_path(explicit: Option<&Path>, repository: &Path) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os(OPS_CONFIG_ENV).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let local = repository.join(".liberado").join("ops.toml");
    if local.is_file() {
        return Some(local);
    }
    liberado_config::config_dir()
        .map(|directory| directory.join("ops.toml"))
        .filter(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_defaults_contain_no_host_identity() {
        let config = HomelabConfig::default();
        assert!(config.ssh_target.is_empty());
        assert!(config.api_url.is_empty());
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("192.168."));
        assert!(!rendered.contains("Shiloh"));
    }

    #[test]
    fn explicit_file_loads_and_unknown_fields_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ops.toml");
        std::fs::write(
            &path,
            "[homelab]\nssh_target='ops@example'\napi_url='https://example.test'\n",
        )
        .unwrap();
        let (config, loaded) = OpsConfig::load(Some(&path), dir.path()).unwrap();
        assert_eq!(loaded, path);
        assert_eq!(config.homelab().unwrap().ssh_target, "ops@example");

        std::fs::write(&path, "unexpected=true\n").unwrap();
        assert!(matches!(
            OpsConfig::load(Some(&path), dir.path()),
            Err(OpsConfigError::Parse { .. })
        ));
    }

    #[test]
    fn homelab_requires_endpoint_values() {
        let config = HomelabConfig::default();
        assert!(config.validate().is_err());
    }
}
