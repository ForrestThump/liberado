//! `liberado-cost` — query token cost over the latency journal at read time.
//!
//! ```text
//! liberado-cost [--data-dir PATH] [--topology PATH] [--prices PATH]
//! ```
//!
//! - `--data-dir` defaults to `$LIBERADO_DATA_DIR` or `.liberado`.
//! - Prices load from `--topology` (a topology.toml with `[[models]]` rates) or `--prices`
//!   (same TOML shape). When neither is given, the tool still reports tokens and marks every
//!   model unpriced.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use liberado_cost::{
    PriceTable, default_data_dir, format_report, price_table_from_topology_path,
    report_from_data_dir,
};

fn main() -> ExitCode {
    let mut data_dir: Option<PathBuf> = None;
    let mut topology: Option<PathBuf> = None;
    let mut prices_path: Option<PathBuf> = None;
    let mut json = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let v = match args.next() {
                    Some(v) => v,
                    None => {
                        eprintln!("liberado-cost: --data-dir requires a path");
                        return ExitCode::from(2);
                    }
                };
                data_dir = Some(PathBuf::from(v));
            }
            "--topology" | "--prices" => {
                let flag = arg.clone();
                let v = match args.next() {
                    Some(v) => v,
                    None => {
                        eprintln!("liberado-cost: {flag} requires a path");
                        return ExitCode::from(2);
                    }
                };
                let p = PathBuf::from(v);
                if flag == "--prices" {
                    prices_path = Some(p);
                } else {
                    topology = Some(p);
                }
            }
            "-h" | "--help" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            "--json" => {
                json = true;
            }
            other => {
                eprintln!("liberado-cost: unknown argument {other}");
                print_help();
                return ExitCode::from(2);
            }
        }
    }

    let data_dir = data_dir.unwrap_or_else(default_data_dir);
    let prices = match load_prices(topology.as_ref(), prices_path.as_ref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("liberado-cost: {e}");
            return ExitCode::from(1);
        }
    };

    match report_from_data_dir(&data_dir, &prices) {
        Ok(report) => {
            if json {
                match serde_json::to_string_pretty(&report) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        eprintln!("liberado-cost: failed to serialize report as JSON: {e}");
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
    // No price file: empty table → every model unpriced (tokens still report).
    Ok(PriceTable::new())
}

fn print_help() {
    eprintln!(
        "\
liberado-cost — token cost over the latency journal (read-time pricing)

USAGE:
    liberado-cost [OPTIONS]

OPTIONS:
    --data-dir PATH   Liberado data dir (default: $LIBERADO_DATA_DIR or .liberado)
    --topology PATH   topology.toml with [[models]] input/output/cached_input rates
    --prices PATH     same as --topology (alias for a prices-only TOML)
    --json            output the report as JSON instead of plain-text tables
    -h, --help        Show this help

Reads <data-dir>/latency/events.jsonl and <data-dir>/dispatches/*.jsonl.
Child dispatch correlations are rolled into parent_conversation from the dispatch
start record. Money is never written to the journal.
"
    );
}
