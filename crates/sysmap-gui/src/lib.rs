//! # liberado-sysmap-gui
//!
//! A project-agnostic 2D system-map renderer with pan, zoom, directed edges, click selection, and
//! detail panels. It depends only on `sysmap-core` plus eframe and can render any [`SystemMap`].
//!
//! [`SystemMap`]: sysmap_core::model::SystemMap

pub mod app;
mod insights;
mod interaction;

pub use app::launch;
