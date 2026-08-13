//! Path-based MVL conformance CLI for fixture files and foreign harnesses.
//!
//! ```text
//! cargo run -p liberado-test-support --bin mvl-conformance -- \
//!   --mvl $OUT/run.mvl.jsonl \
//!   [--execution $OUT/run.execution.jsonl] \
//!   [--expected-content-shown <call_id>=<path>] \
//!   [--kill-after-seq <n>]
//! ```

use std::process::ExitCode;

use liberado_test_support::mvl_oracle::{
    VerdictStatus, oracle_usage, parse_oracle_args, run_mvl_conformance,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{}\n", oracle_usage());
        return ExitCode::from(2);
    }
    let (mvl, opts) = match parse_oracle_args(&args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let report = match run_mvl_conformance(&mvl, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("oracle error: {e}");
            return ExitCode::from(2);
        }
    };
    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("serialize report: {e}");
            return ExitCode::from(2);
        }
    }
    if report
        .verdicts
        .iter()
        .any(|v| v.status == VerdictStatus::Fail)
    {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
