//! `docker run` argv helpers — network default and env passthrough.

use std::collections::BTreeMap;

use crate::sandbox_env::sandbox_env_is_denied;

pub fn push_docker_network(args: &mut Vec<String>, network: Option<&str>) {
    let mode = network.filter(|n| !n.trim().is_empty()).unwrap_or("none");
    args.push("--network".to_string());
    args.push(mode.to_string());
}

pub fn push_docker_env_passes(
    args: &mut Vec<String>,
    allowlist: &[String],
    request_env: &BTreeMap<String, String>,
) {
    for key in allowlist {
        if sandbox_env_is_denied(key) {
            continue;
        }
        args.push("-e".to_string());
        args.push(key.clone());
    }
    for (key, value) in request_env {
        if sandbox_env_is_denied(key) {
            continue;
        }
        args.push("-e".to_string());
        args.push(format!("{key}={value}"));
    }
}
