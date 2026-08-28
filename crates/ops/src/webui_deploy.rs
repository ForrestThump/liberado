//! Build, validate, pack, and refresh the separately mounted WebUI bundle.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::archive::pack_directory;
use crate::deploy::{ssh_options, validate_remote_config};
use crate::{CommandPlan, CommandSpec, DeployOptions, HomelabConfig};

const REMOTE_WEBUI_DEPLOY: &str = r#"set -euo pipefail
remote_dir="$1"
archive="$HOME/liberado-webui-dist.tar.gz"
incoming="${remote_dir}.incoming"
mkdir -p "$HOME/$remote_dir"
rm -rf "$HOME/$incoming"
mkdir -p "$HOME/$incoming"
tar xzf "$archive" -C "$HOME/$incoming"
test -f "$HOME/$incoming/index.html"
rsync -a --delete "$HOME/$incoming/" "$HOME/$remote_dir/"
rm -rf "$HOME/$incoming" "$archive"
"#;

pub fn deploy_webui(
    repository: &Path,
    config: &HomelabConfig,
    options: &DeployOptions,
) -> Result<(), String> {
    config.validate().map_err(|error| error.to_string())?;
    validate_remote_config(config)?;
    let dist = safe_repository_path(repository, &config.webui_local_dir, "webui_local_dir")?;
    if options.dry_run {
        webui_plan(
            repository,
            config,
            &dist,
            Path::new("<temporary>/liberado-webui-dist.tar.gz"),
            options.skip_build,
        )
        .print();
        return Ok(());
    }
    prepare_bundle(repository, &dist, options.skip_build)?;
    ship_bundle(repository, config, &dist)
}

fn prepare_bundle(repository: &Path, dist: &Path, skip_build: bool) -> Result<(), String> {
    if !skip_build {
        remove_stale_bundle(dist)?;
        webui_build_step(repository).run()?;
    }
    validate_webui_bundle(dist)
}

fn remove_stale_bundle(dist: &Path) -> Result<(), String> {
    if !dist.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(dist)
        .map_err(|error| format!("remove stale WebUI bundle {}: {error}", dist.display()))
}

fn ship_bundle(repository: &Path, config: &HomelabConfig, dist: &Path) -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|error| format!("create WebUI temp dir: {error}"))?;
    let archive = temp.path().join("liberado-webui-dist.tar.gz");
    pack_directory(dist, &archive)?;
    webui_plan(repository, config, dist, &archive, true).execute()?;
    verify_http(config)
}

pub fn webui_plan(
    repository: &Path,
    config: &HomelabConfig,
    dist: &Path,
    archive: &Path,
    skip_build: bool,
) -> CommandPlan {
    let mut steps = Vec::new();
    if !skip_build {
        steps.push(webui_build_step(repository));
    }
    steps.extend([
        CommandSpec {
            label: format!("Pack verified bundle from {}", dist.display()),
            program: "<rust-tar>".into(),
            args: vec![archive.display().to_string()],
            cwd: Some(repository.to_path_buf()),
            stdin: None,
        },
        CommandSpec {
            label: "Upload WebUI bundle".into(),
            program: "scp".into(),
            args: ssh_options(config)
                .into_iter()
                .chain([
                    archive.display().to_string(),
                    format!("{}:~/liberado-webui-dist.tar.gz", config.ssh_target),
                ])
                .collect(),
            cwd: Some(repository.to_path_buf()),
            stdin: None,
        },
        CommandSpec {
            label: "Refresh mounted WebUI contents".into(),
            program: "ssh".into(),
            args: ssh_options(config)
                .into_iter()
                .chain([
                    config.ssh_target.clone(),
                    "bash".into(),
                    "-s".into(),
                    "--".into(),
                    config.webui_remote_dir.clone(),
                ])
                .collect(),
            cwd: Some(repository.to_path_buf()),
            stdin: Some(REMOTE_WEBUI_DEPLOY.into()),
        },
    ]);
    CommandPlan { steps }
}

fn webui_build_step(repository: &Path) -> CommandSpec {
    CommandSpec {
        label: "Build release WebUI".into(),
        program: "dx".into(),
        args: vec![
            "build".into(),
            "-r".into(),
            "-p".into(),
            "liberado-webui".into(),
            "--web".into(),
        ],
        cwd: Some(repository.to_path_buf()),
        stdin: None,
    }
}

fn validate_webui_bundle(dist: &Path) -> Result<(), String> {
    if !dist.join("index.html").is_file() {
        return Err(format!("no WebUI bundle at {}", dist.display()));
    }
    let assets = dist.join("assets");
    let mut wasm_count = 0;
    for entry in std::fs::read_dir(&assets)
        .map_err(|error| format!("read WebUI assets {}: {error}", assets.display()))?
    {
        let path = entry
            .map_err(|error| format!("read WebUI asset: {error}"))?
            .path();
        if path.extension().and_then(|value| value.to_str()) != Some("wasm") {
            continue;
        }
        wasm_count += 1;
        let mut bytes = Vec::new();
        File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if bytes
            .windows(b".debug_".len())
            .any(|window| window == b".debug_")
        {
            return Err(format!("{} contains DWARF debug sections", path.display()));
        }
    }
    if wasm_count == 0 {
        return Err(format!("no .wasm files in {}", assets.display()));
    }
    Ok(())
}

fn verify_http(config: &HomelabConfig) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(config.allow_invalid_tls)
        .build()
        .map_err(|error| format!("build HTTP client: {error}"))?;
    let url = format!("{}/api/status", config.api_url.trim_end_matches('/'));
    let response = client
        .get(&url)
        .send()
        .map_err(|error| format!("verify {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("verify {url}: HTTP {}", response.status()));
    }
    println!("Verified {url}");
    Ok(())
}

fn safe_repository_path(repository: &Path, relative: &Path, name: &str) -> Result<PathBuf, String> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("homelab.{name} must be a repository-relative path"));
    }
    let path = repository.join(relative);
    if !path.starts_with(repository.join("target")) {
        return Err(format!("homelab.{name} must stay under target/"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> HomelabConfig {
        HomelabConfig {
            ssh_target: "operator@host.example".into(),
            api_url: "https://liberado.example".into(),
            ..HomelabConfig::default()
        }
    }

    #[test]
    fn plan_preserves_bind_mount_directory() {
        let plan = webui_plan(
            Path::new("repo"),
            &config(),
            Path::new("repo/target/dist"),
            Path::new("bundle.tar.gz"),
            true,
        );
        let remote = plan.steps[2].stdin.as_deref().unwrap();
        assert!(remote.contains("rsync -a --delete"));
        assert!(!remote.contains("mv \"$HOME/$incoming\""));
    }

    #[test]
    fn path_cannot_escape_target() {
        assert!(safe_repository_path(Path::new("repo"), Path::new("../outside"), "x").is_err());
        assert!(safe_repository_path(Path::new("repo"), Path::new("docs"), "x").is_err());
        assert!(safe_repository_path(Path::new("repo"), Path::new("target/dist"), "x").is_ok());
    }
}
