//! Pure CLI parsing for `liberado shepherd`. Split from `shepherd_cmd.rs` for module-health.

use std::path::PathBuf;

/// One parsed shepherd invocation: which mode was asked for and with what modifiers.
///
/// Parsing is pure so the usage rules (a mode is required; `config` demands `check`; `--project`
/// takes the next argument) are testable without a repository or a daemon behind them.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Invocation {
    SelfTest,
    ConfigCheck {
        project: Option<String>,
    },
    Drive {
        once: bool,
        watch: bool,
    },
    ReviewDryRun {
        project: String,
        pr: u64,
        sha: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ParsedInvocation {
    pub(super) mode: Invocation,
    pub(super) dry_run: bool,
    pub(super) project: Option<String>,
    pub(super) seed: Option<PathBuf>,
    pub(super) reset_baselines: bool,
}

pub(super) fn parse_invocation(args: &[String]) -> Result<ParsedInvocation, String> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let project = args
        .windows(2)
        .find(|a| a[0] == "--project")
        .map(|a| a[1].clone());
    let seed = args
        .windows(2)
        .find(|a| a[0] == "--seed")
        .map(|a| PathBuf::from(&a[1]));
    let reset_baselines = args.iter().any(|a| a == "--reset-baselines");

    if args.first().is_some_and(|arg| arg == "review") {
        return parse_review(args, project, seed, dry_run, reset_baselines);
    }

    if args.iter().any(|a| a == "--self-test") {
        return Ok(ParsedInvocation {
            mode: Invocation::SelfTest,
            dry_run,
            project,
            seed,
            reset_baselines,
        });
    }
    if args.first().is_some_and(|arg| arg == "config") {
        if args.get(1).is_none_or(|arg| arg != "check") {
            return Err("usage: liberado shepherd config check [--project <name>]".into());
        }
        return Ok(ParsedInvocation {
            mode: Invocation::ConfigCheck {
                project: project.clone(),
            },
            dry_run,
            project,
            seed,
            reset_baselines,
        });
    }
    let once = args.iter().any(|a| a == "--once");
    let watch = args.iter().any(|a| a == "--watch");
    if !(once || watch || seed.is_some()) {
        return Err(
            "usage: liberado shepherd <--once|--watch|--seed FILE> [--project <name>] [--dry-run]\n       liberado shepherd config check [--project <name>]\n       liberado shepherd --self-test"
                .into(),
        );
    }
    Ok(ParsedInvocation {
        mode: Invocation::Drive { once, watch },
        dry_run,
        project,
        seed,
        reset_baselines,
    })
}

fn parse_review(
    args: &[String],
    project: Option<String>,
    seed: Option<PathBuf>,
    dry_run: bool,
    reset_baselines: bool,
) -> Result<ParsedInvocation, String> {
    let project = project.ok_or("review requires --project <name>")?;
    let pr = option_value(args, "--pr")
        .ok_or("review requires --pr <number>")?
        .parse()
        .map_err(|_| "--pr must be a number")?;
    let sha = option_value(args, "--sha")
        .ok_or("review requires --sha <full-sha>")?
        .to_string();
    if !dry_run {
        return Err("Slices 0/1 permit only `shepherd review --dry-run`".into());
    }
    Ok(ParsedInvocation {
        mode: Invocation::ReviewDryRun { project, pr, sha },
        dry_run,
        project: None,
        seed,
        reset_baselines,
    })
}

fn option_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}
