//! Color styling for the map. The palette for layers and kinds comes from the map's [`Vocabulary`];
//! only the *generic* edge colors, scene constants, and the fallback palette live here. This keeps
//! `sysmap-core` free of project-specific color choices.
//!
//! The GUI consumes these and the legend renders them, so color and legend can never drift.

use crate::model::{EdgeKind, NodeKind};
use crate::vocab::Vocabulary;

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

    /// Parse `#rrggbb` (the leading `#` optional). Returns `None` for malformed input.
    pub fn from_hex(hex: &str) -> Option<Self> {
        let h = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
        if h.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&h[0..2], 16).ok()?;
        let g = u8::from_str_radix(&h[2..4], 16).ok()?;
        let b = u8::from_str_radix(&h[4..6], 16).ok()?;
        Some(Self::new(r, g, b))
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

/// A deterministic, distinct fallback color for an id not declared in the vocabulary.
pub fn fallback_color(id: &str) -> Rgb {
    let h = fnv1a(id);
    FALLBACK_PALETTE[(h % FALLBACK_PALETTE.len() as u64) as usize]
}

fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// 16 visually distinct colors, reused for undeclared layer/kind ids (hash-addressed).
const FALLBACK_PALETTE: [Rgb; 16] = [
    Rgb::new(0xe6, 0x19, 0x4b),
    Rgb::new(0x3c, 0xb4, 0x4b),
    Rgb::new(0x43, 0x63, 0xd8),
    Rgb::new(0xf5, 0x82, 0x31),
    Rgb::new(0x91, 0x1e, 0xb4),
    Rgb::new(0x46, 0xf0, 0xf0),
    Rgb::new(0xf0, 0x32, 0xe6),
    Rgb::new(0xbc, 0xf6, 0x0c),
    Rgb::new(0xfa, 0xbe, 0xbe),
    Rgb::new(0x00, 0x80, 0x80),
    Rgb::new(0xe6, 0xbe, 0xff),
    Rgb::new(0x9a, 0x63, 0x24),
    Rgb::new(0xff, 0xff, 0xff),
    Rgb::new(0x80, 0x00, 0x00),
    Rgb::new(0xaa, 0xff, 0xc3),
    Rgb::new(0x80, 0x80, 0x00),
];

/// Color for a layer id, from the vocabulary, with a fallback for undeclared ids.
pub fn layer_color(vocab: &Vocabulary, layer_id: &str) -> Rgb {
    vocab
        .layer(layer_id)
        .and_then(|l| Rgb::from_hex(&l.color))
        .unwrap_or_else(|| fallback_color(layer_id))
}

/// Color for a node-kind id, from the vocabulary, with a fallback for undeclared ids.
pub fn kind_color(vocab: &Vocabulary, kind_id: &str) -> Rgb {
    vocab
        .kind(kind_id)
        .and_then(|k| Rgb::from_hex(&k.color))
        .unwrap_or_else(|| fallback_color(kind_id))
}

/// The color used to draw a node's building. Crates use their layer color; runtime nodes use their
/// kind color.
pub fn node_color(vocab: &Vocabulary, layer_id: &str, kind_id: &str) -> Rgb {
    if kind_id == NodeKind::CRATE {
        layer_color(vocab, layer_id)
    } else {
        kind_color(vocab, kind_id)
    }
}

/// Edge stroke color by kind (generic semantics — build vs control vs data).
pub fn edge_color(kind: EdgeKind) -> Rgb {
    const COLORS: [Rgb; 5] = [
        Rgb::new(0x6b, 0x74, 0x82), // normal dependency: neutral gray
        Rgb::new(0x56, 0x8a, 0xa6), // development dependency: muted blue
        Rgb::new(0x8a, 0x6a, 0xa6), // build dependency: muted violet
        Rgb::new(0xe0, 0x8a, 0x3c), // control: orange
        Rgb::new(0x3d, 0xb0, 0x6b), // data: green
    ];
    COLORS[kind.index()]
}

/// High-contrast arrowhead color for a directed edge.
///
/// It keeps the edge kind's hue but is intentionally brighter than the shaft.
pub fn arrow_color(kind: EdgeKind) -> Rgb {
    edge_color(kind).tint(0.55)
}

/// Background color for the scene.
pub const SCENE_BACKGROUND: Rgb = Rgb::new(0x10, 0x13, 0x18);
/// Grid-line color under the nodes.
pub const GRID_LINE: Rgb = Rgb::new(0x2a, 0x30, 0x3a);
/// Label color.
pub const LABEL: Rgb = Rgb::new(0xd6, 0xdc, 0xe6);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocab::{KindSpec, LayerSpec};

    #[test]
    fn tint_and_shade_are_extreme_continuous() {
        let c = Rgb::new(0x00, 0x80, 0xff);
        assert_eq!(c.tint(0.0), c);
        assert_eq!(c.tint(1.0), Rgb::new(255, 255, 255));
        assert_eq!(c.shade(0.0), c);
        assert_eq!(c.shade(1.0), Rgb::new(0, 0, 0));
    }

    #[test]
    fn from_hex_parses_with_and_without_hash() {
        assert_eq!(Rgb::from_hex("#4f7ce0"), Some(Rgb::new(0x4f, 0x7c, 0xe0)));
        assert_eq!(Rgb::from_hex("4f7ce0"), Some(Rgb::new(0x4f, 0x7c, 0xe0)));
        assert_eq!(Rgb::from_hex("zzz"), None);
        assert_eq!(Rgb::from_hex("#4f7c"), None);
    }

    #[test]
    fn fallback_color_is_deterministic_and_uses_declared_color_when_present() {
        let vocab = Vocabulary {
            layers: vec![LayerSpec {
                id: "kernel".into(),
                label: "Kernel".into(),
                color: "#4f7ce0".into(),
                blurb: String::new(),
                main: true,
            }],
            kinds: vec![KindSpec {
                id: "vault".into(),
                label: "Vault".into(),
                color: "#8a763a".into(),
                blurb: String::new(),
                height: 1.4,
            }],
        };
        assert_eq!(layer_color(&vocab, "kernel"), Rgb::new(0x4f, 0x7c, 0xe0));
        assert_eq!(kind_color(&vocab, "vault"), Rgb::new(0x8a, 0x76, 0x3a));
        // Undeclared ids get the same fallback color on every call.
        assert_eq!(layer_color(&vocab, "novel"), fallback_color("novel"));
        assert_eq!(layer_color(&vocab, "novel"), layer_color(&vocab, "novel"));
    }

    #[test]
    fn arrowheads_are_brighter_than_their_edge_shafts() {
        for kind in [
            EdgeKind::Dependency,
            EdgeKind::DevelopmentDependency,
            EdgeKind::BuildDependency,
            EdgeKind::Control,
            EdgeKind::Data,
        ] {
            let shaft = edge_color(kind);
            let arrow = arrow_color(kind);
            assert_ne!(arrow, shaft);
            assert!(
                u16::from(arrow.r) + u16::from(arrow.g) + u16::from(arrow.b)
                    > u16::from(shaft.r) + u16::from(shaft.g) + u16::from(shaft.b)
            );
        }
    }
}
