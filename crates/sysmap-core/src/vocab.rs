//! The open vocabulary a map is rendered with: layers and node kinds as *data*, not enum variants.
//!
//! This is what makes `sysmap-core` project-agnostic. A project supplies a [`Vocabulary`] — the
//! layer ids (with colors, blurbs, and stack order) and the runtime node kinds — and the layout,
//! colors, and legend all read it. An id absent from the vocabulary still renders, with a
//! deterministic fallback color, so a new role or kind never breaks the map.

use serde::{Deserialize, Serialize};

/// A declared architectural layer: its id, human label, hex color, explainer blurb, and whether it
/// renders in the main bottom-up stack (versus the side "meta" district).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerSpec {
    pub id: String,
    /// Display name (may equal `id`).
    pub label: String,
    /// `#rrggbb`.
    pub color: String,
    pub blurb: String,
    /// True = main district, rendered bottom-up in `Vocabulary::layers` order; false = side meta
    /// district (tooling/testing/unknown and anything else out of the main stack).
    pub main: bool,
}

/// A declared node kind for non-crate (runtime) nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KindSpec {
    pub id: String,
    /// Display name.
    pub label: String,
    /// `#rrggbb`.
    pub color: String,
    pub blurb: String,
    /// Building height in world units.
    pub height: f32,
}

/// The open layer/kind vocabulary a map is rendered with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vocabulary {
    /// All layers, main-stack layers first (bottom-up), then meta layers.
    pub layers: Vec<LayerSpec>,
    /// Runtime node kinds, in the order the layout groups them in the runtime district.
    pub kinds: Vec<KindSpec>,
}

impl Vocabulary {
    pub fn layer(&self, id: &str) -> Option<&LayerSpec> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn kind(&self, id: &str) -> Option<&KindSpec> {
        self.kinds.iter().find(|k| k.id == id)
    }

    /// Main-stack layers, in bottom-up order.
    pub fn main_stack(&self) -> impl Iterator<Item = &LayerSpec> {
        self.layers.iter().filter(|l| l.main)
    }
}
