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
    Ratchet,
    Modules,
    ModulesRatchet,
}

fn parse_command(
    verb: Option<&str>,
    has_no_extra_args: bool,
) -> Result<CiCommand, Box<dyn std::error::Error>> {
    let command = match verb {
        None => CiCommand::Local,
        Some("check") if has_no_extra_args => CiCommand::Check,
        Some("crap") if has_no_extra_args => CiCommand::Crap,
        Some("ratchet") if has_no_extra_args => CiCommand::Ratchet,
        Some("modules") if has_no_extra_args => CiCommand::Modules,
        Some("modules-ratchet") if has_no_extra_args => CiCommand::ModulesRatchet,
        _ => return Err(USAGE.into()),
    };
    Ok(command)
}

fn execute(command: CiCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        CiCommand::Local => with_log(local_run),
        CiCommand::Check => with_log(check),
        CiCommand::Crap => with_log(crap_check),
        CiCommand::Ratchet => with_log(crap_ratchet),
        CiCommand::Modules => crate::module_health_cmd::check(&repository_root()?),
        CiCommand::ModulesRatchet => crate::module_health_cmd::ratchet(&repository_root()?),
    }
}

fn local_run(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    check(log)?;
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
            (Some("ratchet"), CiCommand::Ratchet),
            (Some("modules"), CiCommand::Modules),
            (Some("modules-ratchet"), CiCommand::ModulesRatchet),
        ] {
            assert_eq!(parse_command(verb, true).unwrap(), expected);
        }
        assert!(parse_command(Some("unknown"), true).is_err());
        assert!(parse_command(Some("modules"), false).is_err());
    }
}
