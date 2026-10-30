//! Argument dispatch for the repository CI commands.

use super::{CiLog, USAGE, check, crap_check, repository_root, with_log};

pub fn run(args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let collected: Vec<String> = args.collect();
    let (base, verb) = split_args(&collected)?;
    let command = parse_command(verb.as_deref())?;
    execute(command, base)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CiCommand {
    Local,
    Check,
    Crap,
    CrapLinux,
    Modules,
    ModulesRatchet,
    Complexity,
    Unwraps,
    UnwrapsRatchet,
    Ready,
    VerifyReady,
}

use std::path::Path;

const CI_COMMANDS: &[(&str, CiCommand)] = &[
    ("check", CiCommand::Check),
    ("crap", CiCommand::Crap),
    ("crap-linux", CiCommand::CrapLinux),
    ("modules", CiCommand::Modules),
    ("modules-ratchet", CiCommand::ModulesRatchet),
    ("complexity", CiCommand::Complexity),
    ("unwraps", CiCommand::Unwraps),
    ("unwraps-ratchet", CiCommand::UnwrapsRatchet),
    ("ready", CiCommand::Ready),
    ("verify-ready", CiCommand::VerifyReady),
];

fn split_args(
    args: &[String],
) -> Result<(Option<String>, Option<String>), Box<dyn std::error::Error>> {
    let mut base = None;
    let mut verb = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--base" {
            let Some(rev) = args.get(index + 1) else {
                return Err(USAGE.into());
            };
            if base.is_some() || rev.starts_with('-') {
                return Err(USAGE.into());
            }
            base = Some(rev.clone());
            index += 2;
            continue;
        }
        if verb.is_some() || args[index].starts_with('-') {
            return Err(USAGE.into());
        }
        verb = Some(args[index].clone());
        index += 1;
    }
    Ok((base, verb))
}

fn parse_command(verb: Option<&str>) -> Result<CiCommand, Box<dyn std::error::Error>> {
    match verb {
        None => Ok(CiCommand::Local),
        Some(v) => CI_COMMANDS
            .iter()
            .find(|(name, _)| *name == v)
            .map(|(_, cmd)| *cmd)
            .ok_or_else(|| USAGE.into()),
    }
}

fn execute(command: CiCommand, base: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    // Deliberately tiered: every tier stays under the complexity ceiling even though none of
    // these arms is unit-testable without running the real tool behind it (`parse_command`
    // above carries the testable half of this dispatch).
    match command {
        CiCommand::Local => with_log(|log| local_run(log, base.as_deref())),
        CiCommand::Check => with_log(check),
        CiCommand::Crap => with_log(|log| crap_check(log, base.as_deref())),
        command => execute_health(command, base),
    }
}

fn execute_health(
    command: CiCommand,
    base: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        CiCommand::Modules => crate::module_health_cmd::check(&repository_root()?),
        CiCommand::ModulesRatchet => crate::module_health_cmd::ratchet(&repository_root()?),
        command => execute_unwraps(command, base),
    }
}

fn execute_unwraps(
    command: CiCommand,
    base: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        CiCommand::Unwraps => unwraps_check(&repository_root()?),
        CiCommand::UnwrapsRatchet => unwraps_ratchet(&repository_root()?),
        command => execute_readiness(command, base),
    }
}

#[cfg(feature = "ci-unwraps")]
fn unwraps_check(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crate::unwrap_classification_cmd::check(root)
}

#[cfg(feature = "ci-unwraps")]
fn unwraps_ratchet(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crate::unwrap_classification_cmd::ratchet(root)
}

#[cfg(not(feature = "ci-unwraps"))]
fn unwraps_check(_root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "liberado ci unwraps requires `--features ci-unwraps` (just unwrap-classification / just ci enable it)"
            .into(),
    )
}

#[cfg(not(feature = "ci-unwraps"))]
fn unwraps_ratchet(_root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "liberado ci unwraps-ratchet requires `--features ci-unwraps` (just unwrap-ratchet / just ci enable it)"
            .into(),
    )
}

fn execute_readiness(
    command: CiCommand,
    base: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(action) = readiness_action(command) {
        action(&repository_root()?)
    } else {
        execute_complexity(command, base)
    }
}

type ReadinessAction = fn(&std::path::Path) -> Result<(), Box<dyn std::error::Error>>;

fn readiness_action(command: CiCommand) -> Option<ReadinessAction> {
    match command {
        CiCommand::CrapLinux => Some(crate::readiness_cmd::crap_linux),
        CiCommand::Ready => Some(crate::readiness_cmd::ready),
        CiCommand::VerifyReady => Some(crate::readiness_cmd::verify),
        _ => None,
    }
}

fn execute_complexity(
    command: CiCommand,
    base: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        CiCommand::Complexity => {
            crate::function_complexity_cmd::check_against(&repository_root()?, base.as_deref())
        }
        _ => unreachable!("core CI commands are handled by execute"),
    }
}

fn local_run(log: &CiLog, base: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    check(log)?;
    finish_local_run(log, base)
}

fn finish_local_run(log: &CiLog, base: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    local_audits(log, base)?;
    crate::readiness_cmd::record_full_ci(&log.root)
}

fn local_audits(log: &CiLog, base: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    crate::readiness_cmd::audit_docs(&log.root)?;
    ratchet_quality(log, base)
}

fn ratchet_quality(log: &CiLog, base: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    run_quality_checks(&log.root)?;
    crap_check(log, base)?;
    run_quality_ratchets(&log.root)
}

fn run_quality_checks(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crate::module_health_cmd::check(root)?;
    unwraps_check(root)
}

/// Linux `just ci` records a new best for module-health and unwraps. Other hosts compare only.
/// A Windows rewrite dirties the tree and blocks `just ready` (Debian CRAP requires a clean HEAD).
fn run_quality_ratchets(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(target_os = "linux") {
        write_quality_baselines(root)
    } else {
        eprintln!(
            "[liberado ci] module-health and unwrap baseline writes are Linux-only \
             (GitHub's Ubuntu job is the host of truth). Compared only."
        );
        Ok(())
    }
}

fn write_quality_baselines(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crate::module_health_cmd::ratchet(root)?;
    unwraps_ratchet(root)
}

#[cfg(test)]
mod tests {
    use super::{CiCommand, parse_command, split_args};

    #[test]
    fn quality_baseline_auto_write_is_linux_only() {
        let src = include_str!("dispatch.rs").replace("\r\n", "\n");
        assert!(
            src.contains(
                "if cfg!(target_os = \"linux\") {\n        write_quality_baselines(root)\n    } else {"
            ),
            "module-health and unwrap writes must stay a compile-time Linux gate"
        );
        assert!(
            src.contains("module-health and unwrap baseline writes are Linux-only"),
            "the deferral banner is the operator-visible rule; do not drop it"
        );
    }

    #[test]
    fn every_ci_verb_parses_without_running_an_external_tool() {
        for (verb, expected) in [
            (None, CiCommand::Local),
            (Some("check"), CiCommand::Check),
            (Some("crap"), CiCommand::Crap),
            (Some("crap-linux"), CiCommand::CrapLinux),
            (Some("modules"), CiCommand::Modules),
            (Some("modules-ratchet"), CiCommand::ModulesRatchet),
            (Some("complexity"), CiCommand::Complexity),
            (Some("ready"), CiCommand::Ready),
            (Some("verify-ready"), CiCommand::VerifyReady),
        ] {
            assert_eq!(parse_command(verb).unwrap(), expected);
        }
        assert!(parse_command(Some("ratchet")).is_err());
        assert!(parse_command(Some("complexity-ratchet")).is_err());
        assert!(parse_command(Some("unknown")).is_err());
    }

    #[test]
    fn base_may_precede_or_follow_the_verb() {
        let (base, verb) = split_args(&["--base".into(), "abc".into(), "crap".into()]).unwrap();
        assert_eq!(base.as_deref(), Some("abc"));
        assert_eq!(verb.as_deref(), Some("crap"));
        let (base, verb) =
            split_args(&["complexity".into(), "--base".into(), "def".into()]).unwrap();
        assert_eq!(base.as_deref(), Some("def"));
        assert_eq!(verb.as_deref(), Some("complexity"));
        assert!(split_args(&["crap".into(), "--nope".into()]).is_err());
    }
}
