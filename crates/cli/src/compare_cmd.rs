//! Thin CLI compatibility adapter for the Rust harness-evaluation engine.

use std::error::Error;

pub fn prepare(args: &[String]) -> Result<(), Box<dyn Error>> {
    liberado_harness_eval::legacy::prepare(args)
}

pub fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    liberado_harness_eval::legacy::run(args)
}

pub fn save(args: &[String]) -> Result<(), Box<dyn Error>> {
    liberado_harness_eval::legacy::save(args)
}
