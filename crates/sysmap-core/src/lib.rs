//! # sysmap-core
//!
//! Project-agnostic core for system maps: the serializable graph model, a deterministic layout,
//! isometric projection, and color styling. No knowledge of any particular project — the layer and
//! node-kind vocabulary, the color palette, and the runtime wiring all come from outside this crate
//! (see the `sysmap.toml` profile in `docs/future-work/sysmap-generic-core-plan.md`).
//!
//! This crate is the extraction seam: `liberado-sysmap` re-exports these modules so consumers keep
//! working while the scanner and wiring (which *are* project-specific) stay behind.

pub mod iso;
pub mod layout;
pub mod model;
pub mod style;
