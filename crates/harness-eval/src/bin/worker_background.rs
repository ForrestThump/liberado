#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    if let Err(error) = liberado_harness_eval::worker::run_command(std::env::args().skip(1)) {
        liberado_harness_eval::worker::record_bootstrap_failure(
            std::env::args().skip(1),
            &error.to_string(),
        );
        std::process::exit(1);
    }
}
