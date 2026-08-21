//! Interactive 2D renderer for any [`sysmap_core::model::SystemMap`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use sysmap_core::layout::{PlacedNode, layout};
use sysmap_core::model::{EdgeKind, MapEdge, SystemMap};
use sysmap_core::style::{self, Rgb};

const WORLD_X_SCALE: f32 = 190.0;
const WORLD_Y_SCALE: f32 = 65.0;
const GRID_PIXEL_STEP: f32 = 65.0;
const MIN_NODE_WIDTH: f32 = 110.0;
const MIN_NODE_HEIGHT: f32 = 58.0;
const BASE_LABEL_FONT: f32 = 14.0;
const MIN_READABLE_LABEL_FONT: f32 = 11.0;
const MIN_ZOOM: f32 = 0.16;
const MAX_ZOOM: f32 = 4.0;
const PANEL_BG: Color32 = Color32::from_rgb(0x1a, 0x1e, 0x26);

pub fn launch(map: SystemMap, repo: PathBuf) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Liberado - system map",
        options,
        Box::new(move |_cc| Ok(Box::new(App::new(map, repo)))),
    )
    .map_err(|error| error.to_string())
}

struct App {
    map: SystemMap,
    repo: PathBuf,
    placed: BTreeMap<String, PlacedNode>,
    selected: Option<String>,
    show_deps: bool,
    show_runtime: bool,
    include_second_hop: bool,
    zoom: f32,
    pan: Vec2,
    needs_fit: bool,
}

impl App {
    fn new(map: SystemMap, repo: PathBuf) -> Self {
        let placed = layout(&map, &map.vocabulary)
            .placed
            .into_iter()
            .map(|node| (node.id.clone(), node))
            .collect();
        Self {
            map,
            repo,
            placed,
            selected: None,
            show_deps: true,
            show_runtime: true,
            include_second_hop: false,
            zoom: 1.0,
            pan: Vec2::ZERO,
            needs_fit: true,
        }
    }

    fn toolbar(&mut self, root: &mut egui::Ui) {
        egui::Panel::top("toolbar")
            .frame(egui::Frame::NONE.fill(PANEL_BG).inner_margin(8.0))
            .show_inside(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Liberado system map (2D)").strong());
                    ui.separator();
                    ui.toggle_value(&mut self.show_deps, "dependencies");
                    ui.toggle_value(&mut self.show_runtime, "runtime paths");
                    ui.add_enabled_ui(self.selected.is_some(), |ui| {
                        ui.toggle_value(&mut self.include_second_hop, "include second hop");
                    });
                    if ui.button("fit").clicked() {
                        self.needs_fit = true;
                    }
                    if ui.button("clear selection").clicked() {
                        self.selected = None;
                    }
                    ui.label(
                        egui::RichText::new("drag to pan - wheel to zoom - click to inspect")
                            .weak()
                            .small(),
                    );
                });
            });
    }

    fn side_panel(&self, root: &mut egui::Ui) {
        egui::Panel::right("legend")
            .resizable(true)
            .default_size(350.0)
            .min_size(260.0)
            .frame(egui::Frame::NONE.fill(PANEL_BG).inner_margin(10.0))
            .show_inside(root, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("Legend");
                    ui.label(
                        egui::RichText::new(
                            "Every edge has an arrow. It points from the dependent or sender to the dependency or receiver.",
                        )
                        .weak(),
                    );
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Layers").strong());
                    for layer in &self.map.vocabulary.layers {
                        swatch_row(
                            ui,
                            style::layer_color(&self.map.vocabulary, &layer.id),
                            &layer.label,
                            &layer.blurb,
                        );
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Runtime infrastructure").strong());
                    for kind in &self.map.vocabulary.kinds {
                        swatch_row(
                            ui,
                            style::kind_color(&self.map.vocabulary, &kind.id),
                            &kind.label,
                            &kind.blurb,
                        );
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Edges").strong());
                    edge_row(ui, EdgeKind::Dependency, "depends on");
                    edge_row(ui, EdgeKind::Control, "control flow");
                    edge_row(ui, EdgeKind::Data, "data flow");
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!(
                            "{} nodes - {} edges",
                            self.map.nodes.len(),
                            self.map.edges.len()
                        ))
                        .weak(),
                    );
                    ui.label(
                        egui::RichText::new(self.repo.display().to_string())
                            .weak()
                            .small(),
                    );
                });
            });
    }

    fn details(&self, root: &mut egui::Ui) {
        egui::Panel::bottom("details")
            .resizable(true)
            .default_size(180.0)
            .min_size(80.0)
            .frame(egui::Frame::NONE.fill(PANEL_BG).inner_margin(10.0))
            .show_inside(root, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if let Some(id) = self.selected.as_deref() {
                        self.node_detail(ui, id);
                    } else {
                        ui.label(
                            egui::RichText::new(
                                "Click a node to isolate its directed relationships and inspect it.",
                            )
                            .weak(),
                        );
                    }
                });
            });
    }

    fn canvas(&mut self, root: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(color32(style::SCENE_BACKGROUND)))
            .show_inside(root, |ui| {
                let (response, painter) =
                    ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
                if self.needs_fit {
                    self.fit(response.rect);
                    self.needs_fit = false;
                }
                if response.dragged() {
                    self.pan += ui.input(|input| input.pointer.delta());
                }
                if response.hovered() {
                    let scroll = ui.input(|input| input.smooth_scroll_delta.y);
                    if scroll.abs() > f32::EPSILON {
                        let pointer = ui
                            .input(|input| input.pointer.hover_pos())
                            .unwrap_or(response.rect.center());
                        self.zoom_at(pointer, (scroll * 0.0015).exp());
                    }
                }

                self.draw_grid(&painter, response.rect);
                let scope =
                    visible_scope(&self.map, self.selected.as_deref(), self.include_second_hop);
                for edge in &self.map.edges {
                    if self.edge_visible(edge, &scope) {
                        self.draw_edge(&painter, edge);
                    }
                }
                for node in self.placed.values() {
                    self.draw_node(&painter, node, scope.as_ref());
                }

                if response.clicked() {
                    self.selected = response
                        .interact_pointer_pos()
                        .and_then(|pos| self.pick(pos));
                }
            });
    }

    fn fit(&mut self, rect: Rect) {
        let Some((min, max)) = world_bounds(&self.map, &self.placed) else {
            return;
        };
        let size = max - min;
        let usable = rect.size() * 0.84;
        self.zoom = (usable.x / size.x.max(1.0))
            .min(usable.y / size.y.max(1.0))
            .clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = rect.center().to_vec2() - (min + size * 0.5) * self.zoom;
    }

    fn zoom_at(&mut self, pointer: Pos2, factor: f32) {
        let old = self.zoom;
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let world = (pointer.to_vec2() - self.pan) / old;
        self.pan = pointer.to_vec2() - world * self.zoom;
    }

    fn screen(&self, node: &PlacedNode) -> Pos2 {
        Pos2::new(node.wx * WORLD_X_SCALE, -node.wy * WORLD_Y_SCALE) * self.zoom + self.pan
    }

    fn node_rect(&self, node: &PlacedNode) -> Rect {
        Rect::from_center_size(self.screen(node), self.node_size(node) * self.zoom)
    }

    fn node_size(&self, placed: &PlacedNode) -> Vec2 {
        let Some(node) = self.map.node(&placed.id) else {
            return Vec2::new(MIN_NODE_WIDTH, MIN_NODE_HEIGHT);
        };
        node_world_size(&self.map, &node.id, &node.label)
    }

    fn pick(&self, pos: Pos2) -> Option<String> {
        self.placed
            .values()
            .find(|node| self.node_rect(node).contains(pos))
            .map(|node| node.id.clone())
    }

    fn edge_visible(&self, edge: &MapEdge, scope: &Option<BTreeSet<String>>) -> bool {
        let kind_visible = match edge.kind {
            EdgeKind::Dependency => self.show_deps,
            EdgeKind::Control | EdgeKind::Data => self.show_runtime,
        };
        kind_visible
            && edge_in_selection(
                edge,
                self.selected.as_deref(),
                self.include_second_hop,
                scope.as_ref(),
            )
    }

    fn draw_grid(&self, painter: &egui::Painter, rect: Rect) {
        let step = GRID_PIXEL_STEP * self.zoom;
        if step < 12.0 {
            return;
        }
        let color = color32(style::GRID_LINE);
        let mut x = self.pan.x.rem_euclid(step) + rect.left();
        while x < rect.right() {
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0, color),
            );
            x += step;
        }
        let mut y = self.pan.y.rem_euclid(step) + rect.top();
        while y < rect.bottom() {
            painter.line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                Stroke::new(1.0, color),
            );
            y += step;
        }
    }

    fn draw_edge(&self, painter: &egui::Painter, edge: &MapEdge) {
        let (Some(from), Some(to)) = (self.placed.get(&edge.from), self.placed.get(&edge.to))
        else {
            return;
        };
        let a = self.screen(from);
        let b = self.screen(to);
        let delta = b - a;
        let length = delta.length();
        if length < 1.0 {
            return;
        }
        let direction = delta / length;
        let start =
            a + direction * ray_rect_distance(direction, self.node_size(from) * self.zoom * 0.5);
        let tip =
            b - direction * ray_rect_distance(-direction, self.node_size(to) * self.zoom * 0.5);
        let color = color32(style::edge_color(edge.kind));
        let width = if edge.kind == EdgeKind::Dependency {
            1.4
        } else {
            2.2
        };
        painter.line_segment([start, tip], Stroke::new(width, color));

        // Direction is always visible. Selection filters edges but never removes arrowheads.
        painter.add(egui::Shape::convex_polygon(
            arrow_points(tip, direction, self.zoom).to_vec(),
            color32(style::arrow_color(edge.kind)),
            Stroke::NONE,
        ));
    }

    fn draw_node(
        &self,
        painter: &egui::Painter,
        placed: &PlacedNode,
        scope: Option<&BTreeSet<String>>,
    ) {
        let Some(node) = self.map.node(&placed.id) else {
            return;
        };
        let base = style::node_color(
            &self.map.vocabulary,
            node.layer.as_str(),
            node.kind.as_str(),
        );
        let fill = if self.selected.as_deref() == Some(&placed.id) {
            Color32::from_rgb(0xff, 0xd8, 0x5a)
        } else if scope.is_some_and(|nodes| nodes.contains(&placed.id)) {
            color32(base.tint(0.2))
        } else if self.selected.is_some() {
            color32(base.shade(0.65))
        } else {
            color32(base)
        };
        let rect = self.node_rect(placed);
        painter.rect(
            rect,
            5.0 * self.zoom,
            fill,
            Stroke::new((1.2 * self.zoom).max(0.5), color32(base.tint(0.35))),
            StrokeKind::Outside,
        );

        let label_rect = rect.shrink(6.0 * self.zoom);
        painter.rect_filled(label_rect, 3.0 * self.zoom, Color32::from_black_alpha(215));

        // The dark inset gives every label the same contrast. Font size follows node size and is
        // capped by the available inset, so it cannot spill across the box.
        let font_size = fitted_label_font(&node.label, self.node_size(placed)) * self.zoom;
        if font_size >= 6.0 {
            painter.text(
                label_rect.center(),
                egui::Align2::CENTER_CENTER,
                &node.label,
                egui::FontId::proportional(font_size),
                Color32::WHITE,
            );
        }
    }

    fn node_detail(&self, ui: &mut egui::Ui, id: &str) {
        let Some(node) = self.map.node(id) else {
            return;
        };
        ui.heading(&node.label);
        ui.label(egui::RichText::new(format!("{} - layer: {}", node.kind, node.layer)).weak());
        if !node.description.is_empty() {
            ui.label(&node.description);
        }
        let outgoing: Vec<_> = self
            .map
            .edges
            .iter()
            .filter(|edge| edge.from == id)
            .collect();
        let incoming: Vec<_> = self.map.edges.iter().filter(|edge| edge.to == id).collect();
        ui.columns(2, |columns| {
            relationship_list(
                &mut columns[0],
                "Outgoing - this node depends on or sends to",
                &outgoing,
                true,
            );
            relationship_list(
                &mut columns[1],
                "Incoming - these nodes depend on or send to this",
                &incoming,
                false,
            );
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.toolbar(ui);
        self.side_panel(ui);
        self.details(ui);
        self.canvas(ui);
    }
}

fn visible_scope(
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

fn edge_in_selection(
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

fn world_bounds(map: &SystemMap, placed: &BTreeMap<String, PlacedNode>) -> Option<(Vec2, Vec2)> {
    let mut bounds = placed.values().filter_map(|placed_node| {
        let node = map.node(&placed_node.id)?;
        let center = Vec2::new(
            placed_node.wx * WORLD_X_SCALE,
            -placed_node.wy * WORLD_Y_SCALE,
        );
        let half = node_world_size(map, &node.id, &node.label) * 0.5;
        Some((center - half, center + half))
    });
    let (mut min, mut max) = bounds.next()?;
    for (node_min, node_max) in bounds {
        min = min.min(node_min);
        max = max.max(node_max);
    }
    Some((min, max))
}

fn arrow_points(tip: Pos2, direction: Vec2, zoom: f32) -> [Pos2; 3] {
    let size = (8.0 * zoom.sqrt()).clamp(5.0, 13.0);
    let normal = Vec2::new(-direction.y, direction.x);
    [
        tip,
        tip - direction * size + normal * size * 0.55,
        tip - direction * size - normal * size * 0.55,
    ]
}

fn node_world_size(map: &SystemMap, id: &str, label: &str) -> Vec2 {
    let label_width = label.chars().count() as f32 * MIN_READABLE_LABEL_FONT * 0.58 + 24.0;
    let load_scale = 1.0 + (map.dependency_fan_in(id) as f32).ln_1p() * 0.16;
    Vec2::new(
        MIN_NODE_WIDTH.max(label_width) * load_scale,
        MIN_NODE_HEIGHT * load_scale,
    )
}

fn fitted_label_font(label: &str, node_size: Vec2) -> f32 {
    let character_units = label.chars().count().max(1) as f32 * 0.58;
    let width_limit = (node_size.x - 24.0) / character_units;
    let height_limit = (node_size.y - 16.0) * 0.55;
    let growth = (node_size.y / MIN_NODE_HEIGHT).sqrt();
    (BASE_LABEL_FONT * growth)
        .min(width_limit)
        .min(height_limit)
        .max(MIN_READABLE_LABEL_FONT)
}

fn ray_rect_distance(direction: Vec2, half_size: Vec2) -> f32 {
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

fn relationship_list(ui: &mut egui::Ui, heading: &str, edges: &[&MapEdge], outgoing: bool) {
    ui.label(egui::RichText::new(heading).strong());
    if edges.is_empty() {
        ui.label(egui::RichText::new("None").weak());
    }
    for edge in edges {
        let other = if outgoing { &edge.to } else { &edge.from };
        let text = if edge.label.is_empty() {
            format!("{} -> {other}", edge.kind)
        } else {
            format!("{} -> {other}: {}", edge.kind, edge.label)
        };
        ui.label(text);
    }
}

fn color32(color: Rgb) -> Color32 {
    Color32::from_rgb(color.r, color.g, color.b)
}

fn swatch_row(ui: &mut egui::Ui, color: Rgb, name: &str, blurb: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
        ui.painter().rect_filled(rect, 2.0, color32(color));
        ui.label(egui::RichText::new(name).strong().small());
        ui.label(egui::RichText::new(blurb).weak().small());
    });
}

fn edge_row(ui: &mut egui::Ui, kind: EdgeKind, label: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(30.0, 8.0), Sense::hover());
        ui.painter().line_segment(
            [rect.left_center(), rect.right_center()],
            Stroke::new(3.0, color32(style::edge_color(kind))),
        );
        let tip = rect.right_center();
        ui.painter().add(egui::Shape::convex_polygon(
            arrow_points(tip, Vec2::X, 0.5).to_vec(),
            color32(style::arrow_color(kind)),
            Stroke::NONE,
        ));
        ui.label(egui::RichText::new(label).weak().small());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sysmap_core::vocab::Vocabulary;

    fn map() -> SystemMap {
        SystemMap {
            generated_at: String::new(),
            repository_root: String::new(),
            config_dir: None,
            vocabulary: Vocabulary {
                layers: vec![],
                kinds: vec![],
            },
            nodes: vec![],
            edges: vec![
                MapEdge {
                    from: "a".into(),
                    to: "b".into(),
                    kind: EdgeKind::Dependency,
                    label: String::new(),
                },
                MapEdge {
                    from: "a".into(),
                    to: "c".into(),
                    kind: EdgeKind::Dependency,
                    label: String::new(),
                },
                MapEdge {
                    from: "b".into(),
                    to: "c".into(),
                    kind: EdgeKind::Dependency,
                    label: String::new(),
                },
                MapEdge {
                    from: "c".into(),
                    to: "d".into(),
                    kind: EdgeKind::Dependency,
                    label: String::new(),
                },
            ],
        }
    }

    #[test]
    fn selection_scope_stops_at_requested_distance() {
        assert_eq!(
            visible_scope(&map(), Some("a"), false).unwrap(),
            BTreeSet::from(["a".into(), "b".into(), "c".into()])
        );
        assert_eq!(
            visible_scope(&map(), Some("a"), true).unwrap(),
            BTreeSet::from(["a".into(), "b".into(), "c".into(), "d".into()])
        );
    }

    #[test]
    fn no_selection_has_no_edge_scope_filter() {
        assert!(visible_scope(&map(), None, false).is_none());
    }

    #[test]
    fn direct_selection_shows_only_incident_edges() {
        let map = map();
        let scope = visible_scope(&map, Some("a"), false);
        assert!(edge_in_selection(
            &map.edges[0],
            Some("a"),
            false,
            scope.as_ref()
        ));
        assert!(!edge_in_selection(
            &map.edges[2],
            Some("a"),
            false,
            scope.as_ref()
        ));
        let two_hop_scope = visible_scope(&map, Some("a"), true);
        assert!(edge_in_selection(
            &map.edges[2],
            Some("a"),
            true,
            two_hop_scope.as_ref()
        ));
    }

    #[test]
    fn arrowhead_points_in_edge_direction_at_every_zoom_level() {
        for zoom in [MIN_ZOOM, 1.0, MAX_ZOOM] {
            let points = arrow_points(Pos2::new(100.0, 50.0), Vec2::X, zoom);
            assert_eq!(points[0], Pos2::new(100.0, 50.0));
            assert!(points[1].x < points[0].x);
            assert!(points[2].x < points[0].x);
        }
    }

    #[test]
    fn only_dependency_fan_in_grows_a_node() {
        let map = map();
        let hub = node_world_size(&map, "c", "same label");
        let outgoing = node_world_size(&map, "a", "same label");
        assert!(hub.x > outgoing.x);
        assert!(hub.y > outgoing.y);
    }

    #[test]
    fn label_font_fits_the_dark_inset() {
        for label in ["short", "liberado-provider-openai-compat"] {
            let size = node_world_size(&map(), "a", label);
            let font = fitted_label_font(label, size);
            let estimated_width = label.chars().count() as f32 * font * 0.58;
            assert!(estimated_width <= size.x - 24.0 + 0.01);
            assert!(font <= (size.y - 16.0) * 0.55 + 0.01);
        }
        let long = "liberado-provider-openai-compat";
        let size = node_world_size(&map(), "a", long);
        assert!(fitted_label_font(long, size) < BASE_LABEL_FONT);
    }
}
