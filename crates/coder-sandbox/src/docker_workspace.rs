//! Docker-backed workspace: `docker run` isolation for coding commands.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use liberado_coder_core::{CommandPolicy, DockerSandboxSpec};
use tokio::time::timeout;

use crate::docker_paths::{docker_volume_arg, sandbox_volume_arg};
use crate::docker_run::{push_docker_env_passes, push_docker_network};
use crate::{
    CommandGrantSet, CommandOutput, CommandRequest, CommandRunner, HostWorkspace, SandboxError,
    Workspace, ensure_granted_or_allowed, offload_id, preview_or_offload,
};

pub struct DockerWorkspace {
    host: HostWorkspace,
    spec: DockerSandboxSpec,
}

impl DockerWorkspace {
    pub fn new(
        root: impl Into<PathBuf>,
        spec: DockerSandboxSpec,
        command_policy: CommandPolicy,
    ) -> Result<Self, SandboxError> {
        Ok(Self {
            host: HostWorkspace::new(root, command_policy)?,
            spec,
        })
    }

    #[cfg(test)]
    #[cfg(test)]
    pub(crate) fn from_host(host: HostWorkspace, spec: DockerSandboxSpec) -> Self {
        Self { host, spec }
    }

    pub fn docker_run_args(&self, request: &CommandRequest) -> Result<Vec<String>, SandboxError> {
        ensure_granted_or_allowed(
            &self.host.command_grants(),
            self.host.command_policy(),
            request,
        )?;
        if self.spec.image.trim().is_empty() {
            return Err(SandboxError::InvalidDockerConfig(
                "docker image must not be empty".to_string(),
            ));
        }

        let mut args = vec![
            "run".to_string(),
            "--rm".to_string(),
            "-i".to_string(),
            "-v".to_string(),
            docker_volume_arg(self.host.root(), "/workspace", false),
            "-w".to_string(),
            "/workspace".to_string(),
        ];
        push_docker_network(&mut args, self.spec.network.as_deref());
        if let Some(user) = &self.spec.user {
            args.push("--user".to_string());
            args.push(user.clone());
        }
        push_docker_env_passes(&mut args, &self.spec.env_allowlist, &request.env);
        for volume in &self.spec.volumes {
            args.push("-v".to_string());
            args.push(sandbox_volume_arg(volume));
        }
        args.push(self.spec.image.clone());
        args.push(request.program.clone());
        args.extend(request.args.clone());
        Ok(args)
    }
}

impl Workspace for DockerWorkspace {
    fn root(&self) -> &Path {
        self.host.root()
    }

    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf, SandboxError> {
        self.host.resolve_path(rel_path)
    }

    fn command_grants(&self) -> CommandGrantSet {
        self.host.command_grants()
    }
}

#[async_trait]
impl CommandRunner for DockerWorkspace {
    async fn run_command(&self, request: CommandRequest) -> Result<CommandOutput, SandboxError> {
        let docker_args = self.docker_run_args(&request)?;
        let mut command = liberado_common::process::command("docker");
        command.args(&docker_args);
        command.kill_on_drop(true);

        let timeout_secs = request
            .timeout_secs
            .unwrap_or(self.host.command_policy().timeout_secs);
        let output_result = timeout(Duration::from_secs(timeout_secs), command.output()).await;
        let (exit_code, stdout, stderr, timed_out) = match output_result {
            Ok(Ok(output)) => (output.status.code(), output.stdout, output.stderr, false),
            Ok(Err(e)) => return Err(SandboxError::Spawn(e.to_string())),
            Err(_) => (None, Vec::new(), Vec::new(), true),
        };

        let max = request
            .output_max_bytes
            .unwrap_or(self.host.command_policy().output_max_bytes);
        let id = offload_id();
        let (stdout, stdout_offload) = preview_or_offload(
            stdout,
            max,
            request.offload_dir.as_deref(),
            &format!("cmd-{id}-stdout.txt"),
        );
        let (stderr, stderr_offload) = preview_or_offload(
            stderr,
            max,
            request.offload_dir.as_deref(),
            &format!("cmd-{id}-stderr.txt"),
        );

        Ok(CommandOutput {
            exit_code,
            stdout,
            stderr,
            timed_out,
            stdout_offload,
            stderr_offload,
        })
    }
}
