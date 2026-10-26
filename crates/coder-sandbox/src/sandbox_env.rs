//! Environment variables that must not reach a sandboxed command.

use std::{collections::BTreeMap, time::Duration};

use tokio::time::timeout;

use crate::SandboxError;

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

pub(crate) fn apply_sandbox_env(command: &mut tokio::process::Command, env: &BTreeMap<String, String>) {
    for (key, value) in env {
        if !sandbox_env_is_denied(key) {
            command.env(key, value);
        }
    }
}

pub(crate) async fn run_host_command(
    mut command: tokio::process::Command,
    timeout_secs: u64,
) -> Result<(Option<i32>, Vec<u8>, Vec<u8>, bool), SandboxError> {
    let output_result = timeout(Duration::from_secs(timeout_secs), command.output()).await;
    match output_result {
        Ok(Ok(output)) => Ok((output.status.code(), output.stdout, output.stderr, false)),
        Ok(Err(e)) => Err(SandboxError::Spawn(e.to_string())),
        Err(_) => Ok((None, Vec::new(), Vec::new(), true)),
    }
}
