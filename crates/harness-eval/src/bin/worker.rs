use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    liberado_harness_eval::worker::run_command(std::env::args().skip(1))
}
