//! `liberado-cost` — query token cost, provenance ratios, and delegation costs.
//!
//! ```text
//! liberado-cost [--data-dir PATH] [--topology PATH] [--prices PATH] [--json]
//! liberado-cost provenance-ratio [--data-dir PATH] [--threshold RATIO] [--json]
//! liberado-cost delegation-cost [--data-dir PATH] [--json]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use liberado_cost::{
    PriceTable, default_data_dir, format_report, price_table_from_topology_path,
    report_from_data_dir, run_delegation_cost, run_provenance_ratio,
};

const DEFAULT_RATIO_THRESHOLD: f64 = 3.0;

#[derive(Parser)]
#[command(name = "liberado-cost", about = "Token cost accounting and analysis")]
struct Cli {
    /// Liberado data dir (default: $LIBERADO_DATA_DIR or .liberado)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// topology.toml with [[models]] rates
    #[arg(long, global = true)]
    topology: Option<PathBuf>,

    /// Same as --topology (alias for prices-only TOML)
    #[arg(long, global = true)]
    prices: Option<PathBuf>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

// Every flag above is `global`, which is not a tidiness choice: the no-subcommand form is the
// documented one, and `liberado-cost --json` is what the backlog tells people to script against.
// Hanging these off the `report` subcommand instead makes the bare invocation an argument error,
// so the first thing any existing caller sees is "unexpected argument '--json'".

#[derive(Subcommand)]
enum Command {
    /// Run the standard token-cost report (default when no subcommand is given)
    Report,
    /// Ratio of delegation output to input — flag transcripts worth reading
    ProvenanceRatio {
        /// Flag delegations whose ratio meets or exceeds this threshold
        #[arg(long, default_value_t = DEFAULT_RATIO_THRESHOLD)]
        threshold: f64,
    },
    /// Compare prompt sizes after delegating vs non-delegating turns
    DelegationCost,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(default_data_dir);
    let json = cli.json;

    match cli.command.unwrap_or(Command::Report) {
        Command::Report => {
            let prices_table = match load_prices(cli.topology.as_ref(), cli.prices.as_ref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("liberado-cost: {e}");
                    return ExitCode::from(1);
                }
            };
            match report_from_data_dir(&data_dir, &prices_table) {
                Ok(report) => {
                    if json {
                        match serde_json::to_string_pretty(&report) {
                            Ok(j) => println!("{j}"),
                            Err(e) => {
                                eprintln!("liberado-cost: JSON: {e}");
                                return ExitCode::from(1);
                            }
                        }
                    } else {
                        print!("{}", format_report(&report));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("liberado-cost: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Command::ProvenanceRatio { threshold } => {
            let rows = run_provenance_ratio(&data_dir);
            if json {
                match serde_json::to_string_pretty(&rows) {
                    Ok(j) => println!("{j}"),
                    Err(e) => {
                        eprintln!("liberado-cost: JSON: {e}");
                        return ExitCode::from(1);
                    }
                }
            } else {
                print_provenance_ratio(&rows, threshold);
            }
            ExitCode::SUCCESS
        }
        Command::DelegationCost => match run_delegation_cost(&data_dir) {
            Ok(samples) => {
                if json {
                    match serde_json::to_string_pretty(&samples) {
                        Ok(j) => println!("{j}"),
                        Err(e) => {
                            eprintln!("liberado-cost: JSON: {e}");
                            return ExitCode::from(1);
                        }
                    }
                } else {
                    print_delegation_cost(&samples);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("liberado-cost: {e}");
                ExitCode::from(1)
            }
        },
    }
}

fn load_prices(
    topology: Option<&PathBuf>,
    prices_path: Option<&PathBuf>,
) -> Result<PriceTable, String> {
    if let Some(p) = prices_path {
        return price_table_from_topology_path(p);
    }
    if let Some(p) = topology {
        return price_table_from_topology_path(p);
    }
    Ok(PriceTable::new())
}

fn print_provenance_ratio(rows: &[liberado_cost::ProvenanceRow], flag_at: f64) {
    if rows.is_empty() {
        println!("no delegation followed by an answer — nothing to report");
        return;
    }
    let flagged: Vec<_> = rows.iter().filter(|r| r.ratio >= flag_at).collect();
    println!(
        "delegations: {}   flagged at ratio >= {flag_at:.1}: {}\n",
        rows.len(),
        flagged.len()
    );
    println!(
        "{:<30} {:>9} {:>9} {:>8}",
        "conversation", "received", "written", "ratio"
    );
    for r in rows.iter().take(20) {
        let mark = if r.ratio >= flag_at { " <-" } else { "" };
        println!(
            "{:<30} {:>9} {:>9} {:>8.1}{mark}",
            r.conversation, r.received, r.written, r.ratio
        );
    }
    let mut ratios: Vec<f64> = rows
        .iter()
        .map(|r| r.ratio)
        .filter(|r| r.is_finite())
        .collect();
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if !ratios.is_empty() {
        println!("\nmedian ratio {:.1}", ratios[ratios.len() / 2]);
    }
}

fn print_delegation_cost(samples: &[liberado_cost::DelegationCostSample]) {
    let after_delegating: Vec<_> = samples.iter().filter(|s| s.after_delegating).collect();
    let after_plain: Vec<_> = samples.iter().filter(|s| !s.after_delegating).collect();
    println!(
        "total samples: {}  after-delegating: {}  after-plain: {}\n",
        samples.len(),
        after_delegating.len(),
        after_plain.len()
    );
    summarize("turn AFTER a delegating turn    ", &after_delegating);
    summarize("turn AFTER a non-delegating turn", &after_plain);
}

fn summarize(label: &str, rows: &[&liberado_cost::DelegationCostSample]) {
    if rows.is_empty() {
        println!("{label}: no samples");
        return;
    }
    let n = rows.len();
    let mean = rows.iter().map(|s| u64::from(s.prompt_tokens)).sum::<u64>() as f64 / n as f64;
    let mut sorted: Vec<u32> = rows.iter().map(|s| s.prompt_tokens).collect();
    sorted.sort_unstable();
    let median = sorted[n / 2];
    let reported: Vec<_> = rows
        .iter()
        .filter(|s| s.cached_prompt_tokens.is_some())
        .collect();
    let hit = if reported.is_empty() {
        "n/a".to_string()
    } else {
        let cached: u64 = reported
            .iter()
            .map(|s| u64::from(s.cached_prompt_tokens.unwrap()))
            .sum();
        let prompt: u64 = reported.iter().map(|s| u64::from(s.prompt_tokens)).sum();
        format!("{:.1}%", cached as f64 / prompt as f64 * 100.0)
    };
    println!("{label}: n={n}  mean_prompt={mean:.0}  median_prompt={median}  cache_hit={hit}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// The bare form is the documented one and the one the backlog scripts against. Hanging
    /// `--json`, `--topology` or `--prices` off the `report` subcommand turns every existing
    /// invocation into `error: unexpected argument`, which is a silent break for anyone whose
    /// only feedback is an exit code.
    #[test]
    fn the_flags_that_worked_before_subcommands_existed_still_work_without_one() {
        for argv in [
            vec!["liberado-cost", "--json"],
            vec!["liberado-cost", "--topology", "t.toml"],
            vec!["liberado-cost", "--prices", "p.toml"],
            vec!["liberado-cost", "--data-dir", "d", "--json"],
        ] {
            let cli = Cli::try_parse_from(&argv)
                .unwrap_or_else(|e| panic!("{argv:?} must still parse:\n{e}"));
            assert!(
                cli.command.is_none(),
                "{argv:?} should fall through to the report"
            );
        }

        let cli = Cli::try_parse_from(["liberado-cost", "--json"]).unwrap();
        assert!(cli.json, "--json must reach the report path");
        let cli = Cli::try_parse_from(["liberado-cost", "--topology", "t.toml"]).unwrap();
        assert_eq!(cli.topology, Some(PathBuf::from("t.toml")));
    }

    #[test]
    fn the_shared_flags_are_also_accepted_after_a_subcommand() {
        let cli = Cli::try_parse_from(["liberado-cost", "provenance-ratio", "--json"]).unwrap();
        assert!(cli.json);
        assert!(matches!(cli.command, Some(Command::ProvenanceRatio { .. })));

        let cli =
            Cli::try_parse_from(["liberado-cost", "delegation-cost", "--data-dir", "d"]).unwrap();
        assert_eq!(cli.data_dir, Some(PathBuf::from("d")));
        assert!(matches!(cli.command, Some(Command::DelegationCost)));
    }

    #[test]
    fn provenance_ratio_threshold_has_a_default_and_takes_an_override() {
        let cli = Cli::try_parse_from(["liberado-cost", "provenance-ratio"]).unwrap();
        let Some(Command::ProvenanceRatio { threshold }) = cli.command else {
            panic!("expected provenance-ratio");
        };
        assert_eq!(threshold, DEFAULT_RATIO_THRESHOLD);

        let cli = Cli::try_parse_from(["liberado-cost", "provenance-ratio", "--threshold", "7.5"])
            .unwrap();
        let Some(Command::ProvenanceRatio { threshold }) = cli.command else {
            panic!("expected provenance-ratio");
        };
        assert_eq!(threshold, 7.5);
    }
}
