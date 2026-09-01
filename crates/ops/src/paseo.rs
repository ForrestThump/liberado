use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::PaseoConfig;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaseoInstallOptions {
    pub skip_build: bool,
    pub skip_smoke: bool,
}

pub fn install_paseo(
    repository: &Path,
    config: &PaseoConfig,
    options: &PaseoInstallOptions,
) -> Result<PathBuf, String> {
    maybe_install_bridge(repository, options.skip_build)?;
    let binary = require_binary(config)?;
    let config_path = write_provider_config(repository, config, &binary)?;
    if !options.skip_smoke {
        smoke_initialize(&binary)?;
    }
    Ok(config_path)
}

fn maybe_install_bridge(repository: &Path, skip: bool) -> Result<(), String> {
    if skip {
        return Ok(());
    }
    run_status(
        repository,
        "cargo",
        &["install", "--path", "crates/acp-bridge", "--force"],
    )
}

fn require_binary(config: &PaseoConfig) -> Result<PathBuf, String> {
    let binary = resolve_binary(config)?;
    if binary.is_file() {
        Ok(binary)
    } else {
        Err(format!("ACP binary not found: {}", binary.display()))
    }
}

fn write_provider_config(
    repository: &Path,
    config: &PaseoConfig,
    binary: &Path,
) -> Result<PathBuf, String> {
    let paseo_home = resolve_paseo_home(config)?;
    std::fs::create_dir_all(&paseo_home)
        .map_err(|error| format!("create {}: {error}", paseo_home.display()))?;
    let config_path = paseo_home.join("config.json");
    let mut root = read_json_object(&config_path)?;
    backup_existing_config(&config_path, &paseo_home)?;
    let liberado_config_dir = config
        .config_dir
        .clone()
        .unwrap_or_else(|| repository.join("config"));
    let provider = json!({
        "extends": "acp",
        "label": config.label,
        "description": config.description,
        "command": [binary],
        "env": {
            "LIBERADO_ACP_MODEL": config.model,
            "LIBERADO_CONFIG_DIR": liberado_config_dir,
        },
        "params": { "supportsMcpServers": false },
    });
    providers_mut(&mut root)?.insert(config.provider_name.clone(), provider);
    let encoded = serde_json::to_vec_pretty(&Value::Object(root))
        .map_err(|error| format!("serialize Paseo config: {error}"))?;
    std::fs::write(&config_path, encoded)
        .map_err(|error| format!("write {}: {error}", config_path.display()))?;
    println!(
        "Configured Paseo provider '{}' in {}",
        config.provider_name,
        config_path.display()
    );
    Ok(config_path)
}

fn backup_existing_config(config_path: &Path, paseo_home: &Path) -> Result<(), String> {
    if !config_path.is_file() {
        return Ok(());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup = paseo_home.join(format!("config.json.{stamp}.bak"));
    std::fs::copy(config_path, &backup)
        .map_err(|error| format!("backup {}: {error}", config_path.display()))?;
    println!(
        "Backed up {} to {}",
        config_path.display(),
        backup.display()
    );
    Ok(())
}

fn resolve_binary(config: &PaseoConfig) -> Result<PathBuf, String> {
    if let Some(path) = &config.binary {
        return Ok(path.clone());
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".cargo")))
        .ok_or("cannot resolve Cargo home; set paseo.binary in ops.toml")?;
    Ok(cargo_home
        .join("bin")
        .join(format!("liberado-acp{}", std::env::consts::EXE_SUFFIX)))
}

fn resolve_paseo_home(config: &PaseoConfig) -> Result<PathBuf, String> {
    config
        .home
        .clone()
        .or_else(|| std::env::var_os("PASEO_HOME").map(PathBuf::from))
        .or_else(|| home_dir().map(|home| home.join(".paseo")))
        .ok_or_else(|| "cannot resolve Paseo home; set paseo.home in ops.toml".into())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>, String> {
    if !path.is_file() {
        return Ok(Map::new());
    }
    let bytes = std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{} must contain a JSON object", path.display()))
}

fn providers_mut(root: &mut Map<String, Value>) -> Result<&mut Map<String, Value>, String> {
    let agents = root
        .entry("agents")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("Paseo config field 'agents' must be an object")?;
    agents
        .entry("providers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| "Paseo config field 'agents.providers' must be an object".into())
}

fn smoke_initialize(binary: &Path) -> Result<(), String> {
    let mut command = liberado_common::process::std_command(binary);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("start ACP smoke test: {error}"))?;
    let request = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":1,\"clientInfo\":{\"name\":\"liberado-ops\",\"version\":\"0\"},\"clientCapabilities\":{}}}\n";
    child
        .stdin
        .take()
        .ok_or("ACP smoke stdin unavailable")?
        .write_all(request)
        .map_err(|error| format!("write ACP smoke request: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait for ACP smoke test: {error}"))?;
    validate_smoke_output(output)
}

fn validate_smoke_output(output: std::process::Output) -> Result<(), String> {
    if !output.status.success() {
        return Err(format!(
            "ACP smoke failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next().unwrap_or("");
    let response: Value = serde_json::from_str(first)
        .map_err(|error| format!("ACP smoke returned invalid JSON: {error}; response={first:?}"))?;
    if response.pointer("/result/protocolVersion").is_none() {
        return Err(format!("ACP smoke returned no protocolVersion: {response}"));
    }
    println!("ACP initialize smoke passed");
    Ok(())
}

fn run_status(repository: &Path, program: &str, args: &[&str]) -> Result<(), String> {
    let status = liberado_common::process::std_command(program)
        .args(args)
        .current_dir(repository)
        .status()
        .map_err(|error| format!("run {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} failed with {status}", args.join(" ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn provider_merge_preserves_unrelated_configuration() {
        let mut root = serde_json::from_value::<Map<String, Value>>(json!({
            "theme": "dark",
            "agents": { "providers": { "other": { "command": ["other"] } } }
        }))
        .unwrap();
        providers_mut(&mut root)
            .unwrap()
            .insert("liberado".into(), json!({"extends":"acp"}));
        let value = Value::Object(root);
        assert_eq!(value.get("theme"), Some(&json!("dark")));
        assert!(value.pointer("/agents/providers/other").is_some());
        assert!(value.pointer("/agents/providers/liberado").is_some());
    }

    #[test]
    fn invalid_provider_container_is_rejected() {
        let mut root = serde_json::from_value::<Map<String, Value>>(json!({
            "agents": { "providers": [] }
        }))
        .unwrap();
        assert!(providers_mut(&mut root).is_err());
    }

    #[test]
    fn provider_config_write_preserves_existing_fields_and_creates_backup() {
        let root = tempdir().unwrap();
        let home = root.path().join("paseo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.json"), r#"{"theme":"dark"}"#).unwrap();
        let config = PaseoConfig {
            home: Some(home.clone()),
            binary: Some(root.path().join("liberado-acp")),
            ..PaseoConfig::default()
        };
        let binary = config.binary.as_deref().unwrap();

        let path = write_provider_config(root.path(), &config, binary).unwrap();
        let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert!(value.pointer("/agents/providers/liberado").is_some());
        assert!(
            std::fs::read_dir(&home)
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".bak"))
        );
    }
}
