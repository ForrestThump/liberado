use std::path::Path;

use crate::{CommandPlan, CommandSpec, HomelabConfig};

const REMOTE_DAEMON_DEPLOY: &str = r#"set -euo pipefail
sha="$1"
build_dir="$2"
stage="$3"
compose_file="$4"
image="$5"
container="$6"
api_url="$7"
lock_timeout="$8"
archive="$HOME/liberado-src-$sha.tar.gz"
stage_path="$HOME/$stage"
build_path="$HOME/$build_dir"
mkdir -p "$stage_path" "$build_path"
tar xzf "$archive" -C "$stage_path"
(
  flock -w "$lock_timeout" 9
  rsync -a --delete --exclude=target/ "$stage_path/" "$build_path/"
  docker build --build-arg "GIT_SHA=$sha" -t "$image" "$build_path"
  docker compose -f "$HOME/$compose_file" up -d --force-recreate
  state="$(docker inspect -f '{{.State.Status}}' "$container")"
  test "$state" = running
  actual="$(docker exec "$container" cat /etc/liberado-build-sha | tr -d '[:space:]')"
  test "$actual" = "$sha"
  curl -fsS --max-time 15 "$api_url/api/status" >/dev/null
) 9>"$HOME/.liberado-deploy.lock"
rm -rf "$stage_path" "$archive"
"#;

const REMOTE_SMOKE: &str = r#"set -euo pipefail
container="$1"
container_binary="$2"
api_url="$3"
expected_sha="$4"
status="$(curl -fsS --max-time 15 "$api_url/api/status")"
printf '%s' "$status" | grep -Eq '"running"[[:space:]]*:[[:space:]]*true'
actual="$(docker exec "$container" cat /etc/liberado-build-sha | tr -d '[:space:]')"
if [ -n "$expected_sha" ]; then test "$actual" = "$expected_sha"; fi
docker exec "$container" "$container_binary" config check >/dev/null
printf 'running build-sha=%s\n' "$actual"
"#;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployOptions {
    pub git_ref: Option<String>,
    pub dry_run: bool,
    pub skip_build: bool,
}

pub fn deploy_homelab(
    repository: &Path,
    config: &HomelabConfig,
    options: &DeployOptions,
) -> Result<(), String> {
    config.validate().map_err(|error| error.to_string())?;
    validate_remote_config(config)?;
    let git_ref = options.git_ref.as_deref().unwrap_or("HEAD");
    let sha = git_capture(repository, &["rev-parse", &format!("{git_ref}^{{commit}}")])?;
    if options.git_ref.is_none() {
        let dirty = git_capture(repository, &["status", "--porcelain"])?;
        if !dirty.is_empty() {
            return Err(
                "refusing to deploy a dirty working tree; commit the intended artifact or pass --ref"
                    .into(),
            );
        }
    }

    if options.dry_run {
        homelab_plan(
            repository,
            config,
            git_ref,
            &sha,
            Path::new("<temporary>/liberado-src.tar.gz"),
        )
        .print();
        return Ok(());
    }

    let temp = tempfile::tempdir().map_err(|error| format!("create deploy temp dir: {error}"))?;
    let archive = temp.path().join("liberado-src.tar.gz");
    homelab_plan(repository, config, git_ref, &sha, &archive).execute()
}

pub fn homelab_plan(
    repository: &Path,
    config: &HomelabConfig,
    git_ref: &str,
    sha: &str,
    archive: &Path,
) -> CommandPlan {
    let remote_archive = format!("{}:~/liberado-src-{sha}.tar.gz", config.ssh_target);
    let stage = format!(".liberado-stage-{}", &sha[..sha.len().min(12)]);
    CommandPlan {
        steps: vec![
            CommandSpec {
                label: "Archive committed source".into(),
                program: "git".into(),
                args: vec![
                    "archive".into(),
                    "--format=tar.gz".into(),
                    format!("--output={}", archive.display()),
                    git_ref.into(),
                ],
                cwd: Some(repository.to_path_buf()),
                stdin: None,
            },
            CommandSpec {
                label: "Upload source archive".into(),
                program: "scp".into(),
                args: ssh_options(config)
                    .into_iter()
                    .chain([archive.display().to_string(), remote_archive])
                    .collect(),
                cwd: Some(repository.to_path_buf()),
                stdin: None,
            },
            CommandSpec {
                label: "Build, recreate, and verify".into(),
                program: "ssh".into(),
                args: ssh_options(config)
                    .into_iter()
                    .chain([
                        config.ssh_target.clone(),
                        "bash".into(),
                        "-s".into(),
                        "--".into(),
                        sha.into(),
                        config.build_dir.clone(),
                        stage,
                        config.compose_file.clone(),
                        config.image.clone(),
                        config.container.clone(),
                        config.api_url.trim_end_matches('/').into(),
                        config.deploy_lock_timeout_secs.to_string(),
                    ])
                    .collect(),
                cwd: Some(repository.to_path_buf()),
                stdin: Some(REMOTE_DAEMON_DEPLOY.into()),
            },
        ],
    }
}

pub fn smoke_homelab(
    config: &HomelabConfig,
    expected_sha: Option<&str>,
    live_chat: bool,
) -> Result<(), String> {
    config.validate().map_err(|error| error.to_string())?;
    validate_remote_config(config)?;
    CommandSpec {
        label: "Verify live deployment facts".into(),
        program: "ssh".into(),
        args: ssh_options(config)
            .into_iter()
            .chain([
                config.ssh_target.clone(),
                "bash".into(),
                "-s".into(),
                "--".into(),
                config.container.clone(),
                config.container_binary.clone(),
                config.api_url.trim_end_matches('/').into(),
                expected_sha.unwrap_or("").into(),
            ])
            .collect(),
        cwd: None,
        stdin: Some(REMOTE_SMOKE.into()),
    }
    .run()?;
    if live_chat {
        live_chat_smoke(config)?;
    }
    Ok(())
}

pub fn latency_homelab(config: &HomelabConfig, json: bool) -> Result<(), String> {
    config.validate().map_err(|error| error.to_string())?;
    validate_remote_config(config)?;
    let output = liberado_common::process::std_command("ssh")
        .args(ssh_options(config))
        .arg(&config.ssh_target)
        .args([
            "docker",
            "exec",
            &config.container,
            "cat",
            &config.latency_journal,
        ])
        .output()
        .map_err(|error| format!("read remote latency journal: {error}"))?;
    render_latency_output(output, json)
}

fn render_latency_output(output: std::process::Output, json: bool) -> Result<(), String> {
    if !output.status.success() {
        return Err(format!(
            "read remote latency journal: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let source = String::from_utf8(output.stdout)
        .map_err(|error| format!("latency journal is not UTF-8: {error}"))?;
    let events = liberado_cost::load_latency_events_from_str(&source)
        .map_err(|error| format!("parse latency journal: {error}"))?;
    let rows = liberado_cost::latency_summary(&events);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows)
                .map_err(|error| format!("serialize latency report: {error}"))?
        );
    } else {
        print!("{}", liberado_cost::format_latency_report(&rows));
    }
    Ok(())
}

fn live_chat_smoke(config: &HomelabConfig) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(config.allow_invalid_tls)
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|error| format!("build HTTP client: {error}"))?;
    let url = format!("{}/api/chat", config.api_url.trim_end_matches('/'));
    let response = client
        .post(&url)
        .json(&serde_json::json!({"message":"Reply with exactly: liberado is live"}))
        .send()
        .map_err(|error| format!("live chat smoke {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("live chat smoke {url}: HTTP {}", response.status()));
    }
    println!("Live chat smoke passed");
    Ok(())
}

fn git_capture(repository: &Path, args: &[&str]) -> Result<String, String> {
    let output = liberado_common::process::std_command("git")
        .args(args)
        .current_dir(repository)
        .output()
        .map_err(|error| format!("run git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}

pub(crate) fn ssh_options(config: &HomelabConfig) -> Vec<String> {
    vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={}", config.connect_timeout_secs),
    ]
}

pub(crate) fn validate_remote_config(config: &HomelabConfig) -> Result<(), String> {
    for (name, value, extra) in [
        ("build_dir", config.build_dir.as_str(), "-._/"),
        ("compose_file", config.compose_file.as_str(), "-._/"),
        ("container", config.container.as_str(), "-._"),
        ("container_binary", config.container_binary.as_str(), "-._/"),
        ("image", config.image.as_str(), "-._/:@"),
        ("webui_remote_dir", config.webui_remote_dir.as_str(), "-._/"),
        ("latency_journal", config.latency_journal.as_str(), "-._/"),
    ] {
        if !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || extra.contains(character))
        {
            return Err(format!(
                "homelab.{name} contains unsupported shell characters"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::ExitStatus;
    use tempfile::tempdir;

    #[cfg(unix)]
    fn status(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn status(code: i32) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(code as u32)
    }

    fn git(repository: &Path, args: &[&str]) {
        assert!(
            liberado_common::process::std_command("git")
                .args(args)
                .current_dir(repository)
                .status()
                .unwrap()
                .success()
        );
    }

    fn committed_repository() -> tempfile::TempDir {
        let root = tempdir().unwrap();
        git(root.path(), &["init"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "Test"]);
        std::fs::write(root.path().join("tracked.txt"), "one\n").unwrap();
        git(root.path(), &["add", "tracked.txt"]);
        git(root.path(), &["commit", "-m", "base"]);
        root
    }

    fn config() -> HomelabConfig {
        HomelabConfig {
            ssh_target: "operator@host.example".into(),
            api_url: "https://liberado.example".into(),
            ..HomelabConfig::default()
        }
    }

    #[test]
    fn daemon_plan_has_provenance_lock_and_exact_live_verification() {
        let plan = homelab_plan(
            Path::new("repo"),
            &config(),
            "HEAD",
            "0123456789abcdef",
            Path::new("source.tar.gz"),
        );
        assert_eq!(plan.steps.len(), 3);
        let remote = plan.steps[2].stdin.as_deref().unwrap();
        assert!(remote.contains("flock -w"));
        assert!(remote.contains("GIT_SHA=$sha"));
        assert!(remote.contains("test \"$actual\" = \"$sha\""));
        let text = format!("{plan:?}");
        assert!(!text.contains("192.168."));
        assert!(!text.contains("Shiloh"));
    }

    #[test]
    fn remote_shell_arguments_reject_injection() {
        let mut bad = config();
        bad.build_dir = "build; reboot".into();
        assert!(validate_remote_config(&bad).is_err());
    }

    #[test]
    fn dry_run_deploy_resolves_a_clean_commit_and_rejects_dirty_head() {
        let root = committed_repository();
        let options = DeployOptions {
            dry_run: true,
            ..DeployOptions::default()
        };
        deploy_homelab(root.path(), &config(), &options).unwrap();

        std::fs::write(root.path().join("tracked.txt"), "dirty\n").unwrap();
        let error = deploy_homelab(root.path(), &config(), &options).unwrap_err();
        assert!(error.contains("dirty working tree"));
    }

    #[test]
    fn latency_output_rejects_remote_failure_and_accepts_empty_journal() {
        let failed = std::process::Output {
            status: status(1),
            stdout: Vec::new(),
            stderr: b"remote unavailable".to_vec(),
        };
        assert!(
            render_latency_output(failed, false)
                .unwrap_err()
                .contains("remote unavailable")
        );

        let empty = std::process::Output {
            status: status(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        render_latency_output(empty, true).unwrap();
    }
}
