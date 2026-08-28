use std::path::PathBuf;

use liberado_ops::{
    DeployOptions, DevAction, DevOptions, OpsConfig, PaseoInstallOptions, deploy_homelab,
    deploy_webui, install_paseo, latency_homelab, repository_root, run_dev as run_dev_action,
    smoke_homelab,
};

pub fn run_deploy(
    args: &mut impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let target = DeployTarget::parse(&args.next().ok_or(deploy_usage())?)?;
    let parsed = ParsedArgs::parse(args)?;
    let repository = repository_root(&std::env::current_dir()?)?;
    let (config, path) = OpsConfig::load(parsed.config.as_deref(), &repository)?;
    println!("Operations config: {}", path.display());
    let homelab = config.homelab()?;
    let options = DeployOptions {
        git_ref: parsed.git_ref.clone(),
        dry_run: parsed.dry_run,
        skip_build: parsed.skip_build,
    };
    target.run(&repository, homelab, &options, &parsed)?;
    Ok(())
}

pub fn run_dev(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let command = args.next().ok_or(dev_usage())?;
    let parsed = ParsedArgs::parse(args)?;
    let repository = repository_root(&std::env::current_dir()?)?;
    let (config, path) = OpsConfig::load(parsed.config.as_deref(), &repository)?;
    println!("Operations config: {}", path.display());
    let action = parse_dev_action(&command)?;
    let options = DevOptions {
        vault: parsed.vault,
        build: parsed.build,
    };
    run_dev_action(&repository, &config.development, action, &options)?;
    Ok(())
}

enum DeployTarget {
    Homelab,
    Webui,
    Smoke,
    Latency,
}

impl DeployTarget {
    fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "homelab" => Ok(Self::Homelab),
            "webui" => Ok(Self::Webui),
            "smoke" => Ok(Self::Smoke),
            "latency" => Ok(Self::Latency),
            _ => Err(deploy_usage()),
        }
    }

    fn run(
        self,
        repository: &std::path::Path,
        homelab: &liberado_ops::HomelabConfig,
        options: &DeployOptions,
        parsed: &ParsedArgs,
    ) -> Result<(), String> {
        match self {
            Self::Homelab => deploy_homelab(repository, homelab, options),
            Self::Webui => deploy_webui(repository, homelab, options),
            Self::Smoke => smoke_homelab(homelab, parsed.expected_sha.as_deref(), parsed.live_chat),
            Self::Latency => latency_homelab(homelab, parsed.json),
        }
    }
}

fn parse_dev_action(command: &str) -> Result<DevAction, &'static str> {
    match command {
        "start" => Ok(DevAction::StartDaemon),
        "stop" => Ok(DevAction::StopDaemon),
        "webui-start" => Ok(DevAction::StartWebui),
        "webui-stop" => Ok(DevAction::StopWebui),
        "status" => Ok(DevAction::Status),
        "tui" => Ok(DevAction::Tui),
        _ => Err(dev_usage()),
    }
}

pub fn run_paseo(
    args: &mut impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.next().as_deref() != Some("install") {
        return Err(paseo_usage().into());
    }
    let parsed = ParsedArgs::parse(args)?;
    let repository = repository_root(&std::env::current_dir()?)?;
    let (config, path) = OpsConfig::load(parsed.config.as_deref(), &repository)?;
    println!("Operations config: {}", path.display());
    install_paseo(
        &repository,
        &config.paseo,
        &PaseoInstallOptions {
            skip_build: parsed.skip_build,
            skip_smoke: parsed.skip_smoke,
        },
    )?;
    Ok(())
}

pub fn run_ops(args: &mut impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    if args.next().as_deref() != Some("config") || args.next().as_deref() != Some("check") {
        return Err("usage: liberado ops config check [--config PATH]".into());
    }
    let parsed = ParsedArgs::parse(args)?;
    let repository = repository_root(&std::env::current_dir()?)?;
    let (config, path) = OpsConfig::load(parsed.config.as_deref(), &repository)?;
    if let Some(homelab) = &config.homelab {
        homelab.validate()?;
    }
    println!("Operations config valid: {}", path.display());
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedArgs {
    config: Option<PathBuf>,
    git_ref: Option<String>,
    vault: Option<PathBuf>,
    dry_run: bool,
    skip_build: bool,
    skip_smoke: bool,
    build: bool,
    expected_sha: Option<String>,
    live_chat: bool,
    json: bool,
}

impl ParsedArgs {
    fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, String> {
        let values: Vec<String> = args.collect();
        let mut parsed = Self::default();
        let mut index = 0;
        while index < values.len() {
            match values[index].as_str() {
                "--config" => {
                    parsed.config = Some(PathBuf::from(value_after(&values, &mut index)?))
                }
                "--ref" => parsed.git_ref = Some(value_after(&values, &mut index)?.to_string()),
                "--vault" => parsed.vault = Some(PathBuf::from(value_after(&values, &mut index)?)),
                "--dry-run" => parsed.dry_run = true,
                "--skip-build" => parsed.skip_build = true,
                "--skip-smoke" => parsed.skip_smoke = true,
                "--build" => parsed.build = true,
                "--expected-sha" => {
                    parsed.expected_sha = Some(value_after(&values, &mut index)?.to_string())
                }
                "--live-chat" => parsed.live_chat = true,
                "--json" => parsed.json = true,
                unknown => return Err(format!("unknown operations argument: {unknown}")),
            }
            index += 1;
        }
        Ok(parsed)
    }
}

fn value_after<'a>(values: &'a [String], index: &mut usize) -> Result<&'a str, String> {
    *index += 1;
    values
        .get(*index)
        .map(String::as_str)
        .ok_or_else(|| "option requires a value".into())
}

fn deploy_usage() -> &'static str {
    "usage: liberado deploy <homelab|webui|smoke|latency> [--config PATH] [--dry-run] [--ref REF] [--skip-build] [--expected-sha SHA] [--live-chat] [--json]"
}

fn dev_usage() -> &'static str {
    "usage: liberado dev <start|stop|webui-start|webui-stop|status|tui> [--config PATH] [--vault PATH] [--build]"
}

fn paseo_usage() -> &'static str {
    "usage: liberado paseo install [--config PATH] [--skip-build] [--skip-smoke]"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(values: &[&str]) -> Result<ParsedArgs, String> {
        ParsedArgs::parse(&mut values.iter().map(|value| value.to_string()))
    }

    #[test]
    fn shared_operations_flags_parse_in_any_order() {
        let parsed = parse(&[
            "--dry-run",
            "--config",
            "local.toml",
            "--ref",
            "main",
            "--skip-build",
        ])
        .unwrap();
        assert!(parsed.dry_run);
        assert!(parsed.skip_build);
        assert_eq!(
            parsed.config.as_deref(),
            Some(std::path::Path::new("local.toml"))
        );
        assert_eq!(parsed.git_ref.as_deref(), Some("main"));
    }

    #[test]
    fn unknown_flags_fail_closed() {
        assert!(parse(&["--mystery"]).is_err());
        assert!(parse(&["--config"]).is_err());
    }

    #[test]
    fn deployment_targets_fail_closed() {
        assert!(matches!(
            DeployTarget::parse("homelab"),
            Ok(DeployTarget::Homelab)
        ));
        assert!(matches!(
            DeployTarget::parse("webui"),
            Ok(DeployTarget::Webui)
        ));
        assert!(matches!(
            DeployTarget::parse("smoke"),
            Ok(DeployTarget::Smoke)
        ));
        assert!(matches!(
            DeployTarget::parse("latency"),
            Ok(DeployTarget::Latency)
        ));
        assert!(DeployTarget::parse("unknown").is_err());
    }

    #[test]
    fn development_actions_fail_closed() {
        for command in [
            "start",
            "stop",
            "webui-start",
            "webui-stop",
            "status",
            "tui",
        ] {
            assert!(parse_dev_action(command).is_ok(), "{command}");
        }
        assert!(parse_dev_action("unknown").is_err());
    }
}
