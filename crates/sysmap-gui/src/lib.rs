//! # liberado-sysmap-gui
//!
//! The three-d system-map renderer: a native window with an orbit/pan/zoom camera, building and
//! edge geometry, click-to-select, and egui panels (legend, explainer, detail). This crate depends
//! only on `sysmap-core` (plus three-d/egui) — it renders any [`SystemMap`], not just Liberado's.
//!
//! [`SystemMap`]: sysmap_core::model::SystemMap

pub mod app;

pub use app::launch;
