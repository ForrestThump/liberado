//! Isometric projection and building geometry — pure math, no UI dependency.
//!
//! World coordinates: `x` east-west (renders down-right), `y` north-south (renders down-left),
//! `z` up. Projection is the classic 30° isometric:
//!
//! ```text
//! sx = ox + (x - y) * cos30 * scale
//! sy = oy + (x + y) * sin30 * scale - z * scale
//! ```
//!
//! This keeps the model renderer-agnostic: the GUI only supplies a scale and a screen origin.

use crate::layout::PlacedNode;

/// A 2D point in screen space.
pub type Pt = [f32; 2];

const COS30: f32 = 0.866_025_4;
const SIN30: f32 = 0.5;

/// A viewport: the scale (pixels per world unit) and screen-space origin (where world (0,0,0)
/// lands).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub scale: f32,
    pub origin_x: f32,
    pub origin_y: f32,
}

impl Default for View {
    fn default() -> Self {
        Self {
            scale: 34.0,
            origin_x: 400.0,
            origin_y: 300.0,
        }
    }
}

/// Project a world point to screen space.
pub fn project(wx: f32, wy: f32, wz: f32, view: &View) -> Pt {
    let sx = view.origin_x + (wx - wy) * COS30 * view.scale;
    let sy = view.origin_y + (wx + wy) * SIN30 * view.scale - wz * view.scale;
    [sx, sy]
}

/// The projected base (ground) and top (roof) diamonds of a building, in order
/// `[north, east, south, west]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuildingGeometry {
    pub base: [Pt; 4],
    pub top: [Pt; 4],
}

/// Compute a building's projected faces.
pub fn building_geometry(p: &PlacedNode, view: &View) -> BuildingGeometry {
    let h = p.half;
    let z = p.height;
    let base = [
        project(p.wx - h, p.wy - h, 0.0, view), // north
        project(p.wx + h, p.wy - h, 0.0, view), // east
        project(p.wx + h, p.wy + h, 0.0, view), // south
        project(p.wx - h, p.wy + h, 0.0, view), // west
    ];
    let top = [
        project(p.wx - h, p.wy - h, z, view), // north
        project(p.wx + h, p.wy - h, z, view), // east
        project(p.wx + h, p.wy + h, z, view), // south
        project(p.wx - h, p.wy + h, z, view), // west
    ];
    BuildingGeometry { base, top }
}

/// The roof polygon (a closed convex diamond), indices `[north, east, south, west]`.
pub fn roof_poly(g: &BuildingGeometry) -> [Pt; 4] {
    g.top
}

/// The west-facing (left) wall: `[west_bottom, south_bottom, south_top, west_top]`.
pub fn left_wall_poly(g: &BuildingGeometry) -> [Pt; 4] {
    [g.base[3], g.base[2], g.top[2], g.top[3]]
}

/// The east-facing (right) wall: `[south_bottom, east_bottom, east_top, south_top]`.
pub fn right_wall_poly(g: &BuildingGeometry) -> [Pt; 4] {
    [g.base[2], g.base[1], g.top[1], g.top[2]]
}

/// The ground-center of a building (for edge endpoints).
pub fn base_center(p: &PlacedNode, view: &View) -> Pt {
    project(p.wx, p.wy, 0.0, view)
}

/// Whether `pt` is inside a convex polygon given in order.
pub fn point_in_convex_polygon(pt: Pt, poly: &[Pt]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    // Cross-product sign must stay consistent for a convex polygon.
    let mut sign = 0.0f32;
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let cross = (b[0] - a[0]) * (pt[1] - a[1]) - (b[1] - a[1]) * (pt[0] - a[0]);
        if cross.abs() < 1e-6 {
            continue;
        }
        let s = cross.signum();
        if sign == 0.0 {
            sign = s;
        } else if s != sign {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(id: &str, wx: f32, wy: f32, height: f32) -> PlacedNode {
        PlacedNode {
            id: id.to_string(),
            wx,
            wy,
            height,
            half: 0.6,
        }
    }

    #[test]
    fn north_renders_above_south() {
        let view = View::default();
        // Same x, north (smaller y) vs south (larger y): north must have a smaller sy.
        let north = project(0.0, -1.0, 0.0, &view);
        let south = project(0.0, 1.0, 0.0, &view);
        assert!(north[1] < south[1]);
    }

    #[test]
    fn height_lifts_off_the_ground() {
        let view = View::default();
        let base = project(0.0, 0.0, 0.0, &view);
        let top = project(0.0, 0.0, 2.0, &view);
        assert!(top[1] < base[1]);
    }

    #[test]
    fn building_geometry_is_consistent() {
        let view = View::default();
        let g = building_geometry(&placed("x", 0.0, 0.0, 1.0), &view);
        // Roof is above base for every corner.
        for (b, t) in g.base.iter().zip(g.top.iter()) {
            assert!(t[1] < b[1]);
        }
    }

    #[test]
    fn point_in_polygon_hits_roof_center() {
        let view = View::default();
        let g = building_geometry(&placed("x", 0.0, 0.0, 1.0), &view);
        let center = g.top.iter().fold([0.0f32; 2], |acc, p| {
            [acc[0] + p[0] / 4.0, acc[1] + p[1] / 4.0]
        });
        assert!(point_in_convex_polygon(center, &roof_poly(&g)));
        // A point far away is not inside.
        assert!(!point_in_convex_polygon([10000.0, 10000.0], &roof_poly(&g)));
    }
}
