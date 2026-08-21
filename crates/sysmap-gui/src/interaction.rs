//! Pure visibility and geometry rules for the interactive renderer.

use std::collections::BTreeSet;

use eframe::egui::{Pos2, Vec2};
use sysmap_core::model::{EdgeKind, MapEdge, SystemMap};

pub(crate) fn edge_kind_visible(
    kind: EdgeKind,
    show_dependencies: bool,
    show_development: bool,
    show_build: bool,
    show_runtime: bool,
) -> bool {
    match kind {
        EdgeKind::Dependency => show_dependencies,
        EdgeKind::DevelopmentDependency => show_development,
        EdgeKind::BuildDependency => show_build,
        EdgeKind::Control | EdgeKind::Data => show_runtime,
    }
}

pub(crate) fn visible_scope(
    map: &SystemMap,
    selected: Option<&str>,
    second_hop: bool,
) -> Option<BTreeSet<String>> {
    let selected = selected?;
    let mut scope = BTreeSet::from([selected.to_string()]);
    let first: Vec<String> = map
        .neighbors(selected)
        .into_iter()
        .map(str::to_string)
        .collect();
    scope.extend(first.iter().cloned());
    if second_hop {
        for neighbor in first {
            scope.extend(map.neighbors(&neighbor).into_iter().map(str::to_string));
        }
    }
    Some(scope)
}

pub(crate) fn edge_in_selection(
    edge: &MapEdge,
    selected: Option<&str>,
    second_hop: bool,
    scope: Option<&BTreeSet<String>>,
) -> bool {
    let Some(selected) = selected else {
        return true;
    };
    if !second_hop {
        return edge.from == selected || edge.to == selected;
    }
    scope.is_some_and(|nodes| nodes.contains(&edge.from) && nodes.contains(&edge.to))
}

pub(crate) fn arrow_points(tip: Pos2, direction: Vec2, zoom: f32) -> [Pos2; 3] {
    let size = (8.0 * zoom.sqrt()).clamp(5.0, 13.0);
    let normal = Vec2::new(-direction.y, direction.x);
    [
        tip,
        tip - direction * size + normal * size * 0.55,
        tip - direction * size - normal * size * 0.55,
    ]
}

pub(crate) fn ray_rect_distance(direction: Vec2, half_size: Vec2) -> f32 {
    let x = if direction.x.abs() > f32::EPSILON {
        half_size.x / direction.x.abs()
    } else {
        f32::INFINITY
    };
    let y = if direction.y.abs() > f32::EPSILON {
        half_size.y / direction.y.abs()
    } else {
        f32::INFINITY
    };
    x.min(y)
}
