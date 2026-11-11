//! Volume mount spelling for `docker run -v`.

use std::path::Path;

use liberado_coder_core::SandboxVolume;

pub(crate) fn docker_volume_arg(host: &Path, container: &str, read_only: bool) -> String {
    let mut arg = format!("{}:{container}", docker_path(host));
    if read_only {
        arg.push_str(":ro");
    }
    arg
}

pub(crate) fn sandbox_volume_arg(volume: &SandboxVolume) -> String {
    let mut arg = format!(
        "{}:{}",
        normalize_docker_path(&volume.host),
        normalize_docker_path(&volume.container)
    );
    if volume.read_only {
        arg.push_str(":ro");
    }
    arg
}

fn docker_path(path: &Path) -> String {
    normalize_docker_path(&path.to_string_lossy())
}

fn normalize_docker_path(path: &str) -> String {
    path.replace('\\', "/")
}
