//! The `liberado` binary — a three-line shell over the [`liberado_cli`] library, which owns the
//! argument grammar and every sub-command module. The split exists so the coverage/CRAP gates can
//! attribute per-function results (bin targets never register); see the library's crate docs for
//! usage.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    liberado_cli::run(&mut std::env::args().skip(1)).await
}
