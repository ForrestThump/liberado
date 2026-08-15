//! The eframe/egui application: renders the isometric scene, legend, explainer, and detail panels,
//! and handles pan/zoom/click/hover interaction.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui;
use liberado_sysmap::iso::{self, BuildingGeometry, Pt, View};
use liberado_sysmap::layout::{self, PlacedNode};
use liberado_sysmap::model::{EdgeKind, Layer, MapNode, NodeKind, SystemMap};
use liberado_sysmap::style::{self, Rgb};

const BG: egui::Color32 = egui::Color32::from_rgb(0x10, 0x13, 0x18);
const PANEL_BG: egui::Color32 = egui::Color32::from_rgb(0x1a, 0x1e, 0x26);

pub fn launch(map: SystemMap, repo: PathBuf) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1480.0, 900.0])
            .with_min_inner_size([960.0, 620.0])
            .with_title("Liberado — isometric system map"),
        ..Default::default()
    };
    eframe::run_native(
        "liberado-sysmap",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, map, repo)))),
    )
    .map_err(|e| e.to_string())
}

struct App {
    map: SystemMap,
    repo: PathBuf,
    placed: BTreeMap<String, PlacedNode>,
    view: View,
    selected: Option<String>,
    hovered: Option<String>,
    show_deps: bool,
    show_runtime: bool,
    show_labels: bool,
    fit_requested: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, map: SystemMap, repo: PathBuf) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let layout = layout::layout(&map);
        let placed: BTreeMap<String, PlacedNode> = layout
            .placed
            .iter()
            .map(|p| (p.id.clone(), p.clone()))
            .collect();
        Self {
            map,
            repo,
            placed,
            view: View::default(),
            selected: None,
            hovered: None,
            show_deps: true,
            show_runtime: true,
            show_labels: true,
            fit_requested: true,
        }
    }

    fn node(&self, id: &str) -> Option<&MapNode> {
        self.map.node(id)
    }

    // ── panels ────────────────────────────────────────────────────────────

    fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Liberado system map").strong());
            ui.separator();
            ui.toggle_value(&mut self.show_deps, "dependencies");
            ui.toggle_value(&mut self.show_runtime, "runtime paths");
            ui.toggle_value(&mut self.show_labels, "labels");
            ui.separator();
            if ui.button("Fit view").clicked() {
                self.fit_requested = true;
            }
            if ui.button("Clear selection").clicked() {
                self.selected = None;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} nodes · {} edges · {}",
                        self.map.nodes.len(),
                        self.map.edges.len(),
                        self.repo.display()
                    ))
                    .weak()
                    .small(),
                );
            });
        });
    }

    fn legend_ui(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Legend");
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Buildings are colored by architectural layer. Height is hub-ness (dependency fan-in + fan-out).")
                    .small()
                    .weak(),
            );
            ui.add_space(8.0);

            ui.label(egui::RichText::new("Layers (bottom → top)").strong());
            for layer in Layer::ALL.iter().copied() {
                swatch_row(ui, style::layer_color(layer), layer.as_str(), layer.blurb());
            }
            swatch_row(
                ui,
                style::layer_color(Layer::Unknown),
                "unknown",
                Layer::Unknown.blurb(),
            );

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Runtime infrastructure").strong());
            for kind in runtime_kinds() {
                swatch_row(ui, style::kind_color(kind), kind.label(), kind_label_blurb(kind));
            }

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Edges").strong());
            edge_row(ui, EdgeKind::Dependency, "build-time dependency (Cargo.toml)");
            edge_row(ui, EdgeKind::Control, "runtime control flow");
            edge_row(ui, EdgeKind::Data, "runtime data / payload flow");

            ui.add_space(8.0);
            explainer_ui(ui);
        });
    }

    fn details_ui(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| match (&self.selected, &self.hovered) {
            (Some(id), _) | (None, Some(id)) => self.node_detail(ui, id),
            _ => {
                ui.label(
                    egui::RichText::new(
                        "Click a building to inspect it. Drag to pan, scroll to zoom.",
                    )
                    .weak(),
                );
            }
        });
    }

    fn node_detail(&self, ui: &mut egui::Ui, id: &str) {
        let Some(node) = self.node(id) else { return };
        let color = style::node_color(node.layer, node.kind);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, color32(color));
            ui.heading(&node.label);
        });
        ui.label(
            egui::RichText::new(format!("{} · layer: {}", node.kind.label(), node.layer)).weak(),
        );
        if !node.description.is_empty() {
            ui.add_space(4.0);
            ui.label(&node.description);
        }
        if !node.deps.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Depends on").strong());
            ui.label(node.deps.join(", "));
        }
        if !node.meta.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Runtime metadata").strong());
            for (k, v) in &node.meta {
                ui.label(format!("{k}: {v}"));
            }
        }
        if !node.enabled {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("disabled (declared but not enabled)")
                    .italics()
                    .weak(),
            );
        }
        // Neighbor summary.
        let neighbors = self.map.neighbors(id);
        if !neighbors.is_empty() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("{} connected components", neighbors.len())).strong(),
            );
        }
    }

    // ── scene ─────────────────────────────────────────────────────────────

    fn scene_ui(&mut self, ui: &mut egui::Ui) {
        let (response, painter) =
            ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
        let rect = response.rect;
        painter.rect_filled(rect, 0.0, BG);

        if self.fit_requested {
            self.fit_view(rect);
            self.fit_requested = false;
        }

        // Pan with primary drag.
        if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            self.view.origin_x += d.x;
            self.view.origin_y += d.y;
        }

        // Zoom with the scroll wheel, centered on the view center.
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            let factor = (scroll * 0.0015).exp();
            let center = rect.center();
            self.view.origin_x = center.x - (center.x - self.view.origin_x) * factor;
            self.view.origin_y = center.y - (center.y - self.view.origin_y) * factor;
            self.view.scale = (self.view.scale * factor).clamp(6.0, 240.0);
        }

        // Geometry for this frame, sorted back-to-front (smallest base sy first).
        let mut geoms: Vec<(String, BuildingGeometry, PlacedNode)> = self
            .placed
            .values()
            .map(|p| {
                let g = iso::building_geometry(p, &self.view);
                (p.id.clone(), g, p.clone())
            })
            .collect();
        geoms.sort_by(|a, b| a.2.id.cmp(&b.2.id));
        // Draw order uses screen depth, not id; re-sort by depth for rendering.
        geoms.sort_by(|a, b| {
            let a_sy = iso::base_center(&a.2, &self.view)[1];
            let b_sy = iso::base_center(&b.2, &self.view)[1];
            a_sy.partial_cmp(&b_sy).unwrap_or(std::cmp::Ordering::Equal)
        });

        self.draw_grid(&painter, rect);
        self.draw_edges(&painter);
        for (id, g, _p) in &geoms {
            self.draw_building(&painter, id, g);
        }
        if self.show_labels {
            for (_id, g, p) in &geoms {
                self.draw_label(&painter, p, g);
            }
        }

        // Hover + click hit-testing (front-most hit wins).
        let pointer = response.hover_pos().or_else(|| {
            if response.clicked() {
                response.interact_pointer_pos()
            } else {
                None
            }
        });
        self.hovered = pointer.and_then(|pos| self.hit_test([pos.x, pos.y], &geoms));
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            self.selected = self.hit_test([pos.x, pos.y], &geoms);
        }

        if self.hovered.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if let Some(id) = &self.hovered
            && let Some(node) = self.node(id)
        {
            self.tooltip(ui.ctx(), node.summary());
        }
    }

    fn draw_grid(&self, painter: &egui::Painter, rect: egui::Rect) {
        let line_color = egui::Color32::from_rgb(0x24, 0x2a, 0x34);
        let stroke = egui::Stroke::new(1.0, line_color);
        // Draw world-space grid lines across the visible area.
        let step = 1.7f32;
        let range = 120i32;
        for i in -range..=range {
            let c = i as f32 * step;
            let a = iso::project(c, -range as f32 * step, 0.0, &self.view);
            let b = iso::project(c, range as f32 * step, 0.0, &self.view);
            painter.line_segment([pos2(a), pos2(b)], stroke);
            let a = iso::project(-range as f32 * step, c, 0.0, &self.view);
            let b = iso::project(range as f32 * step, c, 0.0, &self.view);
            painter.line_segment([pos2(a), pos2(b)], stroke);
        }
        let _ = rect; // grid covers the whole scene; no need to clip
    }

    fn draw_edges(&self, painter: &egui::Painter) {
        for edge in &self.map.edges {
            match edge.kind {
                EdgeKind::Dependency if !self.show_deps => continue,
                EdgeKind::Control | EdgeKind::Data if !self.show_runtime => continue,
                _ => {}
            }
            let (Some(from), Some(to)) = (self.placed.get(&edge.from), self.placed.get(&edge.to))
            else {
                continue;
            };
            let base_color = style::edge_color(edge.kind);
            let width = if edge.kind == EdgeKind::Dependency {
                1.0
            } else {
                2.0
            };
            let alpha = if edge.kind == EdgeKind::Dependency {
                110
            } else {
                200
            };
            let color = egui::Color32::from_rgba_unmultiplied(
                base_color.r,
                base_color.g,
                base_color.b,
                alpha,
            );

            if edge.from == edge.to {
                self.draw_self_loop(painter, from, color, width);
                continue;
            }

            let (start, end) = trimmed_endpoints(from, to, &self.view);
            let stroke = egui::Stroke::new(width, color);
            painter.line_segment([pos2(start), pos2(end)], stroke);

            // Arrowheads on directed runtime edges.
            if edge.kind != EdgeKind::Dependency {
                let tip = pos2(end);
                let dir = normalize([end[0] - start[0], end[1] - start[1]]);
                let perp = [-dir[1], dir[0]];
                let size = 7.0;
                let base = [tip.x - dir[0] * size, tip.y - dir[1] * size];
                let a = [
                    base[0] + perp[0] * size * 0.5,
                    base[1] + perp[1] * size * 0.5,
                ];
                let b = [
                    base[0] - perp[0] * size * 0.5,
                    base[1] - perp[1] * size * 0.5,
                ];
                painter.add(egui::Shape::convex_polygon(
                    vec![pos2(a), pos2(b), tip],
                    color,
                    egui::Stroke::NONE,
                ));
            }

            // Payload label near the midpoint.
            if !edge.label.is_empty() && edge.kind != EdgeKind::Dependency {
                let mid = [(start[0] + end[0]) * 0.5, (start[1] + end[1]) * 0.5];
                painter.text(
                    pos2(mid),
                    egui::Align2::CENTER_BOTTOM,
                    edge.label.clone(),
                    egui::FontId::proportional(10.0),
                    egui::Color32::from_rgb(0xc8, 0xd0, 0xdc),
                );
            }
        }
    }

    fn draw_self_loop(
        &self,
        painter: &egui::Painter,
        p: &PlacedNode,
        color: egui::Color32,
        width: f32,
    ) {
        // A small loop above the roof.
        let roof_center = iso::project(p.wx, p.wy, p.height + 0.35, &self.view);
        let center = pos2(roof_center);
        let radius = 10.0 * (self.view.scale / 34.0).clamp(0.5, 2.0);
        let n = 24;
        let mut points = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let t = (i as f32 / n as f32) * std::f32::consts::TAU;
            points.push(egui::Pos2::new(
                center.x + t.cos() * radius,
                center.y + t.sin() * radius * 0.6,
            ));
        }
        painter.add(egui::Shape::line(points, egui::Stroke::new(width, color)));
    }

    fn draw_building(&self, painter: &egui::Painter, id: &str, g: &BuildingGeometry) {
        let Some(node) = self.node(id) else { return };
        let base = style::node_color(node.layer, node.kind);
        let dimmed = !node.enabled;

        let fill = |c: Rgb, f: f32| {
            let c = if dimmed { c.shade(0.45) } else { c };
            color32_alpha(c, f)
        };

        let left = fill(base, 1.0);
        let right = fill(base.shade(0.35), 1.0);
        let roof = fill(base.tint(0.22), 1.0);
        let outline = egui::Stroke::new(1.0, color32_alpha(Rgb::new(0, 0, 0), 0.5));

        let left_poly = iso::left_wall_poly(g).iter().copied().map(pos2).collect();
        let right_poly = iso::right_wall_poly(g).iter().copied().map(pos2).collect();
        let roof_poly: Vec<egui::Pos2> = iso::roof_poly(g).iter().copied().map(pos2).collect();

        painter.add(egui::Shape::convex_polygon(left_poly, left, outline));
        painter.add(egui::Shape::convex_polygon(right_poly, right, outline));
        painter.add(egui::Shape::convex_polygon(
            roof_poly.clone(),
            roof,
            outline,
        ));

        // Selection / hover / neighbor highlight ring around the roof.
        let highlight = self.selected.as_deref() == Some(id)
            || self.hovered.as_deref() == Some(id)
            || self
                .selected
                .as_deref()
                .is_some_and(|s| self.map.neighbors(s).contains(&id));
        if highlight {
            let ring = if self.selected.as_deref() == Some(id) {
                egui::Color32::from_rgb(0xff, 0xd8, 0x5a)
            } else {
                egui::Color32::from_rgb(0x8f, 0xd0, 0xff)
            };
            let ring_stroke = egui::Stroke::new(2.0, ring);
            painter.add(egui::Shape::line(
                roof_poly
                    .iter()
                    .chain(std::iter::once(&roof_poly[0]))
                    .copied()
                    .collect(),
                ring_stroke,
            ));
        }
    }

    fn draw_label(&self, painter: &egui::Painter, p: &PlacedNode, g: &BuildingGeometry) {
        // Label under the building's south corner.
        let base_south = g.base[2];
        let pos = egui::Pos2::new(base_south[0], base_south[1] + 4.0);
        let text = self
            .node(&p.id)
            .map(|n| n.label.clone())
            .unwrap_or_else(|| p.id.clone());
        painter.text(
            pos,
            egui::Align2::CENTER_TOP,
            text,
            egui::FontId::proportional(11.0),
            color32(style::LABEL),
        );
    }

    fn hit_test(&self, pt: Pt, geoms: &[(String, BuildingGeometry, PlacedNode)]) -> Option<String> {
        // Front-most (largest base sy) hit wins.
        let mut best: Option<(String, f32)> = None;
        for (id, g, p) in geoms {
            let hit = iso::point_in_convex_polygon(pt, &iso::roof_poly(g))
                || iso::point_in_convex_polygon(pt, &iso::left_wall_poly(g))
                || iso::point_in_convex_polygon(pt, &iso::right_wall_poly(g));
            if hit {
                let depth = iso::base_center(p, &self.view)[1];
                if best.as_ref().is_none_or(|(_, d)| depth > *d) {
                    best = Some((id.clone(), depth));
                }
            }
        }
        best.map(|(id, _)| id)
    }

    fn tooltip(&self, ctx: &egui::Context, text: String) {
        let pos = ctx
            .input(|i| i.pointer.latest_pos())
            .unwrap_or(egui::Pos2::ZERO);
        egui::Area::new(egui::Id::new("sysmap-tooltip"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos + egui::vec2(14.0, 14.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new(text).small());
                });
            });
    }

    fn fit_view(&mut self, rect: egui::Rect) {
        // Project every building corner at scale 1 to find the world's screen extent.
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let probe = View {
            scale: 1.0,
            origin_x: 0.0,
            origin_y: 0.0,
        };
        let mut corners = Vec::new();
        for p in self.placed.values() {
            let g = iso::building_geometry(p, &probe);
            corners.extend(g.base);
            corners.extend(g.top);
        }
        for c in corners {
            min_x = min_x.min(c[0]);
            min_y = min_y.min(c[1]);
            max_x = max_x.max(c[0]);
            max_y = max_y.max(c[1]);
        }
        if !min_x.is_finite() {
            return;
        }
        let w = (max_x - min_x).max(1.0);
        let h = (max_y - min_y).max(1.0);
        let pad = 0.92;
        let scale = ((rect.width() / w).min(rect.height() / h) * pad).clamp(4.0, 240.0);
        let cx = (min_x + max_x) * 0.5;
        let cy = (min_y + max_y) * 0.5;
        self.view = View {
            scale,
            origin_x: rect.center().x - cx * scale,
            origin_y: rect.center().y - cy * scale,
        };
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("toolbar")
            .exact_height(36.0)
            .frame(
                egui::Frame::NONE
                    .fill(PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ctx, |ui| self.toolbar_ui(ui));

        egui::TopBottomPanel::bottom("details")
            .resizable(true)
            .default_height(180.0)
            .min_height(80.0)
            .frame(
                egui::Frame::NONE
                    .fill(PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ctx, |ui| self.details_ui(ui));

        egui::SidePanel::right("legend")
            .resizable(true)
            .default_width(400.0)
            .min_width(260.0)
            .frame(
                egui::Frame::NONE
                    .fill(PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ctx, |ui| self.legend_ui(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ctx, |ui| self.scene_ui(ui));
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn color32(c: Rgb) -> egui::Color32 {
    egui::Color32::from_rgb(c.r, c.g, c.b)
}

fn color32_alpha(c: Rgb, f: f32) -> egui::Color32 {
    let a = (f.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(c.r, c.g, c.b, a)
}

fn pos2(p: Pt) -> egui::Pos2 {
    egui::Pos2::new(p[0], p[1])
}

fn normalize(v: Pt) -> Pt {
    let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if len < 1e-6 {
        [1.0, 0.0]
    } else {
        [v[0] / len, v[1] / len]
    }
}

/// Shorten an edge so it stops at each building's footprint edge instead of its center.
fn trimmed_endpoints(from: &PlacedNode, to: &PlacedNode, view: &View) -> (Pt, Pt) {
    let start = iso::base_center(from, view);
    let end = iso::base_center(to, view);
    let dir = normalize([end[0] - start[0], end[1] - start[1]]);
    let from_inset = from.half * view.scale * 1.1;
    let to_inset = to.half * view.scale * 1.1;
    let s = [
        start[0] + dir[0] * from_inset,
        start[1] + dir[1] * from_inset,
    ];
    let e = [end[0] - dir[0] * to_inset, end[1] - dir[1] * to_inset];
    (s, e)
}

fn swatch_row(ui: &mut egui::Ui, color: Rgb, name: &str, blurb: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 2.0, color32(color));
        ui.label(egui::RichText::new(name).strong().small());
        ui.label(egui::RichText::new(blurb).weak().small());
    });
}

fn edge_row(ui: &mut egui::Ui, kind: EdgeKind, label: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 3.0), egui::Sense::hover());
        let c = style::edge_color(kind);
        ui.painter().rect_filled(rect, 1.0, color32(c));
        ui.label(egui::RichText::new(label).weak().small());
    });
}

fn runtime_kinds() -> [NodeKind; 9] {
    [
        NodeKind::Vault,
        NodeKind::Provider,
        NodeKind::Mcp,
        NodeKind::Pool,
        NodeKind::Profile,
        NodeKind::Project,
        NodeKind::Schedule,
        NodeKind::Hook,
        NodeKind::Notifier,
    ]
}

fn kind_label_blurb(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Vault => "Obsidian vault (source of truth)",
        NodeKind::Provider => "inference backend",
        NodeKind::Mcp => "MCP server (agent tools)",
        NodeKind::Pool => "authority-segregated pool",
        NodeKind::Profile => "session profile (pack + hat)",
        NodeKind::Project => "authorized coding root",
        NodeKind::Schedule => "cron schedule",
        NodeKind::Hook => "external webhook",
        NodeKind::Notifier => "notification channel (Telegram)",
        NodeKind::Crate => "crate",
    }
}

/// An explainer panel rendered as a visible section at the bottom of the legend panel.
pub fn explainer_ui(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.separator();
    ui.heading("About this map");
    ui.label(
        "Liberado is a Rust-native personal AI life OS: a daemon that watches an Obsidian vault, \
         reasons with an LLM, and acts through tools — safely. This map is generated from source, \
         not hand-drawn:",
    );
    ui.label("• every building is a workspace crate (colored by architectural layer), or a runtime component declared in topology.toml.");
    ui.label("• gray edges are build-time dependencies from Cargo.toml.");
    ui.label("• orange/green arrows are runtime control and data paths — the perceive → decide → act loop, surfaces, inference, and notification.");
    ui.label(
        "• building height is hub-ness: how many crates depend on it plus how many it depends on.",
    );
    ui.add_space(4.0);
    ui.label(
        "The map is rebuilt from crates/*/Cargo.toml and topology.toml on every launch, so a \
         dependency change appears on the next run — no re-examination. Run \
         `liberado-sysmap --write-json out.json` for the serialized graph.",
    );
}
