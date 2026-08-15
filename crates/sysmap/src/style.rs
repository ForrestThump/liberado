//! Color tokens for the map. This is the single source of truth for how a node/edge is colored;
//! the GUI consumes these and the legend renders them, so the two can never drift.
//!
//! Colors are chosen for distinction between the ten layers plus runtime kinds, and are tuned so
//! the dark "shade" / light "tint" faces of an isometric box read as one building.

use crate::model::{EdgeKind, Layer, NodeKind};

/// An sRGB color, 0-255 per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Scale each channel toward white by `t` in `[0,1]` (1 = white).
    pub fn tint(self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let mix = |c: u8| (c as f32 + (255.0 - c as f32) * t).round() as u8;
        Self::new(mix(self.r), mix(self.g), mix(self.b))
    }

    /// Scale each channel toward black by `t` in `[0,1]` (1 = black).
    pub fn shade(self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let mix = |c: u8| (c as f32 * (1.0 - t)).round() as u8;
        Self::new(mix(self.r), mix(self.g), mix(self.b))
    }

    pub fn to_array(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }

    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

/// Base color for a layer.
pub fn layer_color(layer: Layer) -> Rgb {
    match layer {
        Layer::Foundation => Rgb::new(0x8a, 0x94, 0xa6), // slate
        Layer::Client => Rgb::new(0x2f, 0xb8, 0xa0),     // teal
        Layer::Kernel => Rgb::new(0x4f, 0x7c, 0xe0),     // blue
        Layer::Store => Rgb::new(0xd9, 0x9a, 0x3d),      // amber
        Layer::Pack => Rgb::new(0x4c, 0xaf, 0x50),       // green
        Layer::Service => Rgb::new(0x8e, 0x5b, 0xc0),    // purple
        Layer::Surface => Rgb::new(0xd0, 0x5a, 0x8a),    // magenta
        Layer::Root => Rgb::new(0xc0, 0x45, 0x3a),       // crimson
        Layer::Tooling => Rgb::new(0x9a, 0xb8, 0x2f),    // lime
        Layer::Testing => Rgb::new(0xa0, 0x7a, 0x50),    // tan
        Layer::Unknown => Rgb::new(0x5a, 0x5f, 0x68),    // neutral gray
    }
}

/// Base color for a runtime node kind. Runtime nodes share a steel-blue family so they read as
/// "infrastructure" rather than crates, with per-kind accents.
pub fn kind_color(kind: NodeKind) -> Rgb {
    match kind {
        NodeKind::Crate => Rgb::new(0x55, 0x5f, 0x6e),
        NodeKind::Provider => Rgb::new(0x39, 0x9c, 0xb8), // cyan
        NodeKind::Mcp => Rgb::new(0x6a, 0x8f, 0xd0),      // steel blue
        NodeKind::Pool => Rgb::new(0x74, 0x68, 0xc9),     // indigo
        NodeKind::Profile => Rgb::new(0x3d, 0xa8, 0x86),  // sea green
        NodeKind::Project => Rgb::new(0xc4, 0x84, 0x2f),  // ochre
        NodeKind::Schedule => Rgb::new(0xb0, 0x6a, 0x4a), // rust
        NodeKind::Hook => Rgb::new(0xa6, 0x4f, 0x6b),     // rose
        NodeKind::Vault => Rgb::new(0x8a, 0x76, 0x3a),    // bronze
        NodeKind::Notifier => Rgb::new(0x5b, 0x9c, 0x8f), // sage
    }
}

/// The color used to draw a node's building. Crates use their layer color; runtime nodes use their
/// kind color.
pub fn node_color(layer: Layer, kind: NodeKind) -> Rgb {
    if kind == NodeKind::Crate {
        layer_color(layer)
    } else {
        kind_color(kind)
    }
}

/// Edge stroke color by kind.
pub fn edge_color(kind: EdgeKind) -> Rgb {
    match kind {
        EdgeKind::Dependency => Rgb::new(0x6b, 0x74, 0x82), // neutral gray
        EdgeKind::Control => Rgb::new(0xe0, 0x8a, 0x3c),    // orange
        EdgeKind::Data => Rgb::new(0x3d, 0xb0, 0x6b),       // green
    }
}

/// Background color for the isometric scene.
pub const SCENE_BACKGROUND: Rgb = Rgb::new(0x10, 0x13, 0x18);
/// Grid-line color under the buildings.
pub const GRID_LINE: Rgb = Rgb::new(0x2a, 0x30, 0x3a);
/// Label color.
pub const LABEL: Rgb = Rgb::new(0xd6, 0xdc, 0xe6);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tint_and_shade_are_extreme_continuous() {
        let c = Rgb::new(0x00, 0x80, 0xff);
        assert_eq!(c.tint(0.0), c);
        assert_eq!(c.tint(1.0), Rgb::new(255, 255, 255));
        assert_eq!(c.shade(0.0), c);
        assert_eq!(c.shade(1.0), Rgb::new(0, 0, 0));
    }

    #[test]
    fn every_layer_has_a_distinct_color() {
        let mut seen = std::collections::BTreeSet::new();
        for layer in Layer::ALL {
            assert!(
                seen.insert(layer_color(layer).to_array()),
                "{layer:?} shares a color"
            );
        }
    }
}
