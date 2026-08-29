//! Argument dispatch for the repository CI commands.

use super::{CiLog, USAGE, check, crap_check, crap_ratchet, repository_root, with_log};

pub fn run(args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args.peekable();
    let command = parse_command(args.next().as_deref(), args.peek().is_none())?;
    execute(command)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CiCommand {
    Local,
    Check,
    Crap,
    CrapLinux,
    Ratchet,
    Modules,
    ModulesRatchet,
    Complexity,
    ComplexityRatchet,
    Ready,
    VerifyReady,
}

fn parse_command(
    verb: Option<&str>,
    has_no_extra_args: bool,
) -> Result<CiCommand, Box<dyn std::error::Error>> {
    let command = match verb {
        None => CiCommand::Local,
        Some("check") if has_no_extra_args => CiCommand::Check,
        Some("crap") if has_no_extra_args => CiCommand::Crap,
        Some("crap-linux") if has_no_extra_args => CiCommand::CrapLinux,
        Some("ratchet") if has_no_extra_args => CiCommand::Ratchet,
        Some("modules") if has_no_extra_args => CiCommand::Modules,
        Some("modules-ratchet") if has_no_extra_args => CiCommand::ModulesRatchet,
        Some("complexity") if has_no_extra_args => CiCommand::Complexity,
        Some("complexity-ratchet") if has_no_extra_args => CiCommand::ComplexityRatchet,
        Some("ready") if has_no_extra_args => CiCommand::Ready,
        Some("verify-ready") if has_no_extra_args => CiCommand::VerifyReady,
        _ => return Err(USAGE.into()),
    };
    Ok(command)
}

fn execute(command: CiCommand) -> Result<(), Box<dyn std::error::Error>> {
    // Deliberately tiered: every tier stays under the complexity ceiling even though none of
    // these arms is unit-testable without running the real tool behind it (`parse_command`
    // above carries the testable half of this dispatch).
    match command {
        CiCommand::Local => with_log(local_run),
        CiCommand::Check => with_log(check),
        CiCommand::Crap => with_log(crap_check),
        CiCommand::Ratchet => with_log(crap_ratchet),
        command => execute_health(command),
    }
}

fn execute_health(command: CiCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        CiCommand::Modules => crate::module_health_cmd::check(&repository_root()?),
        CiCommand::ModulesRatchet => crate::module_health_cmd::ratchet(&repository_root()?),
        command => execute_readiness(command),
    }
}

fn execute_readiness(command: CiCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        CiCommand::CrapLinux => crate::readiness_cmd::crap_linux(&repository_root()?),
        CiCommand::Ready => crate::readiness_cmd::ready(&repository_root()?),
        CiCommand::VerifyReady => crate::readiness_cmd::verify(&repository_root()?),
        command => execute_complexity(command),
    }
}

fn execute_complexity(command: CiCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        CiCommand::Complexity => crate::function_complexity_cmd::check(&repository_root()?),
        CiCommand::ComplexityRatchet => {
            crate::function_complexity_cmd::ratchet(&repository_root()?)
        }
        _ => unreachable!("core CI commands are handled by execute"),
    }
}

fn local_run(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    check(log)?;
    local_audits(log)
}

fn local_audits(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    crate::readiness_cmd::audit_docs(&log.root)?;
    ratchet_quality(log)
}

fn ratchet_quality(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    crate::module_health_cmd::check(&log.root)?;
    crap_ratchet(log)?;
    crate::module_health_cmd::ratchet(&log.root)
}

#[cfg(test)]
mod tests {
    use super::{CiCommand, parse_command};

    #[test]
    fn every_ci_verb_parses_without_running_an_external_tool() {
        for (verb, expected) in [
            (None, CiCommand::Local),
            (Some("check"), CiCommand::Check),
            (Some("crap"), CiCommand::Crap),
            (Some("crap-linux"), CiCommand::CrapLinux),
            (Some("ratchet"), CiCommand::Ratchet),
            (Some("modules"), CiCommand::Modules),
            (Some("modules-ratchet"), CiCommand::ModulesRatchet),
            (Some("complexity"), CiCommand::Complexity),
            (Some("complexity-ratchet"), CiCommand::ComplexityRatchet),
            (Some("ready"), CiCommand::Ready),
            (Some("verify-ready"), CiCommand::VerifyReady),
        ] {
            assert_eq!(parse_command(verb, true).unwrap(), expected);
        }
        assert!(parse_command(Some("unknown"), true).is_err());
        assert!(parse_command(Some("modules"), false).is_err());
    }
}
