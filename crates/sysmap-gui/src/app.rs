//! The three-d renderer: a true 3D scene (orbit/pan/zoom camera). This crate renders any
//! [`sysmap_core::model::SystemMap`] — it depends only on `sysmap-core` (plus three-d/egui), so it
//! is liftable out of Liberado. The legend, explainer and detail panels are egui, overlaid on the
//! 3D view via three-d's egui `GUI`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use three_d::*;

use sysmap_core::layout::{PlacedNode, layout};
use sysmap_core::model::{EdgeKind, SystemMap};
use sysmap_core::style::{self, Rgb};

pub fn launch(map: SystemMap, repo: PathBuf) -> Result<(), String> {
    let window = Window::new(WindowSettings {
        title: "Liberado — 3D system map".to_string(),
        max_size: Some((1600, 1000)),
        min_size: (960, 600),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;

    let context = window.gl();
    let mut app = App::new(&context, map, repo);
    let (eye, target) = app.camera_pose();

    let mut camera = Camera::new_perspective(
        window.viewport(),
        eye,
        target,
        vec3(0.0, 1.0, 0.0),
        degrees(45.0),
        0.1,
        2000.0,
    );
    let mut control = OrbitControl::new(target, 2.0, 500.0);
    let mut gui = three_d::GUI::new(&context);

    window.render_loop(move |mut frame_input| {
        camera.set_viewport(frame_input.viewport);

        // Rebuild the instanced buffers from current state (selection, toggles). 48 buildings and
        // a few hundred edges is a tiny upload; doing it every frame keeps state handling simple.
        app.rebuild_instances();

        // Panels consume events first, so dragging on a panel does not orbit the scene.
        gui.update(
            &mut frame_input.events,
            frame_input.accumulated_time,
            frame_input.viewport,
            frame_input.device_pixel_ratio,
            |ui| {
                app.panels_ui(
                    ui,
                    &camera,
                    frame_input.viewport,
                    frame_input.device_pixel_ratio,
                );
            },
        );

        // Camera: orbit + zoom (left-drag / wheel), then pan (right-drag) and click-to-select.
        control.handle_events(&mut camera, &mut frame_input.events);
        app.handle_pan(&mut camera, &mut frame_input.events);
        app.handle_pick(&context, &camera, &mut frame_input.events);

        let screen = frame_input.screen();
        let result = screen
            .clear(ClearState::color_and_depth(0.05, 0.06, 0.09, 1.0, 1.0))
            .write(|| {
                app.render_objects(&screen, &camera);
                gui.render()
            });
        if let Err(e) = result {
            eprintln!("render error: {e}");
        }

        FrameOutput::default()
    });

    Ok(())
}

struct App {
    map: SystemMap,
    repo: PathBuf,
    placed: BTreeMap<String, PlacedNode>,
    /// Building instance order; index == instance id.
    node_ids: Vec<String>,

    building_transforms: Vec<Mat4>,
    building_base_colors: Vec<Srgba>,
    buildings: Gm<InstancedMesh, PhysicalMaterial>,

    /// Edge data for all edges whose endpoints are both placed (parallel vectors).
    edge_transforms: Vec<Mat4>,
    edge_colors: Vec<Srgba>,
    edge_kinds: Vec<EdgeKind>,
    edges: Gm<InstancedMesh, PhysicalMaterial>,

    ground: Gm<Mesh, PhysicalMaterial>,
    grid: Gm<InstancedMesh, PhysicalMaterial>,
    light: DirectionalLight,
    ambient: AmbientLight,

    selected: Option<String>,
    show_deps: bool,
    show_runtime: bool,
}

impl App {
    fn new(context: &Context, map: SystemMap, repo: PathBuf) -> Self {
        let layout = layout(&map, &map.vocabulary);
        let placed: BTreeMap<String, PlacedNode> = layout
            .placed
            .iter()
            .map(|p| (p.id.clone(), p.clone()))
            .collect();
        let node_ids: Vec<String> = placed.keys().cloned().collect();

        // Buildings: one instanced cube per node, colored by layer/kind, height by hub-ness.
        let mut building_transforms = Vec::with_capacity(node_ids.len());
        let mut building_base_colors = Vec::with_capacity(node_ids.len());
        for id in &node_ids {
            let p = &placed[id];
            let node = map.node(id).expect("placed node in map");
            building_transforms.push(building_transform(p));
            building_base_colors.push(srgba(style::node_color(
                &map.vocabulary,
                node.layer.as_str(),
                node.kind.as_str(),
            )));
        }
        let buildings = Gm::new(
            InstancedMesh::new(
                context,
                &Instances {
                    transformations: building_transforms.clone(),
                    colors: Some(building_base_colors.clone()),
                    ..Default::default()
                },
                &CpuMesh::cube(),
            ),
            lit_white(context),
        );

        // Edges: one instanced cylinder per edge (skipping self-loops), colored by kind.
        let mut edge_transforms = Vec::new();
        let mut edge_colors = Vec::new();
        let mut edge_kinds = Vec::new();
        for edge in &map.edges {
            let (Some(a), Some(b)) = (placed.get(&edge.from), placed.get(&edge.to)) else {
                continue;
            };
            if edge.from == edge.to {
                continue;
            }
            let a = base(a);
            let b = base(b);
            if let Some(t) = segment_transform(a, b, edge_thickness(edge.kind)) {
                edge_transforms.push(t);
                edge_colors.push(srgba(style::edge_color(edge.kind)));
                edge_kinds.push(edge.kind);
            }
        }
        let edges = Gm::new(
            InstancedMesh::new(
                context,
                &Instances {
                    transformations: edge_transforms.clone(),
                    colors: Some(edge_colors.clone()),
                    ..Default::default()
                },
                &CpuMesh::cylinder(10),
            ),
            lit_white(context),
        );

        // Ground plane + grid.
        let mut ground_mesh = Mesh::new(context, &CpuMesh::cube());
        ground_mesh.set_transformation(
            Mat4::from_translation(vec3(0.0, -0.03, 0.0))
                * Mat4::from_nonuniform_scale(400.0, 0.03, 400.0),
        );
        let ground = Gm::new(
            ground_mesh,
            PhysicalMaterial::new_opaque(
                context,
                &CpuMaterial {
                    albedo: Srgba::new(0x12, 0x16, 0x1d, 255),
                    ..Default::default()
                },
            ),
        );

        let grid_transforms = grid_transforms(&placed);
        let grid_n = grid_transforms.len();
        let grid = Gm::new(
            InstancedMesh::new(
                context,
                &Instances {
                    transformations: grid_transforms,
                    colors: Some(vec![Srgba::new(0x24, 0x2a, 0x34, 255); grid_n]),
                    ..Default::default()
                },
                &CpuMesh::cylinder(6),
            ),
            lit_white(context),
        );

        let light = DirectionalLight::new(context, 1.0, Srgba::WHITE, vec3(-0.4, -1.0, -0.6));
        let ambient = AmbientLight::new(context, 0.35, Srgba::WHITE);

        Self {
            map,
            repo,
            placed,
            node_ids,
            building_transforms,
            building_base_colors,
            buildings,
            edge_transforms,
            edge_colors,
            edge_kinds,
            edges,
            ground,
            grid,
            light,
            ambient,
            selected: None,
            show_deps: true,
            show_runtime: true,
        }
    }

    fn camera_pose(&self) -> (Vec3, Vec3) {
        let (center, extent) = self.bounds();
        let d = (extent * 0.9).max(5.0);
        let eye = center + vec3(d, d * 0.8, d);
        (eye, center)
    }

    fn bounds(&self) -> (Vec3, f32) {
        let mut min_x = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut min_z = f32::INFINITY;
        let mut max_z = f32::NEG_INFINITY;
        for p in self.placed.values() {
            min_x = min_x.min(p.wx - p.half);
            max_x = max_x.max(p.wx + p.half);
            min_z = min_z.min(p.wy - p.half);
            max_z = max_z.max(p.wy + p.half);
        }
        let center = vec3((min_x + max_x) * 0.5, 0.0, (min_z + max_z) * 0.5);
        let extent = (max_x - min_x).max(max_z - min_z).max(1.0);
        (center, extent)
    }

    fn rebuild_instances(&mut self) {
        // Buildings: recolor by selection (selected + neighbors highlighted).
        let colors: Vec<Srgba> = self
            .node_ids
            .iter()
            .enumerate()
            .map(|(i, id)| self.highlight_color(id, self.building_base_colors[i]))
            .collect();
        self.buildings.set_instances(&Instances {
            transformations: self.building_transforms.clone(),
            colors: Some(colors),
            ..Default::default()
        });

        // Edges: filter by toggles.
        let mut transforms = Vec::new();
        let mut colors = Vec::new();
        for (i, kind) in self.edge_kinds.iter().enumerate() {
            let visible = match kind {
                EdgeKind::Dependency => self.show_deps,
                EdgeKind::Control | EdgeKind::Data => self.show_runtime,
            };
            if visible {
                transforms.push(self.edge_transforms[i]);
                colors.push(self.edge_colors[i]);
            }
        }
        self.edges.set_instances(&Instances {
            transformations: transforms,
            colors: Some(colors),
            ..Default::default()
        });
    }

    fn highlight_color(&self, id: &str, base: Srgba) -> Srgba {
        if self.selected.as_deref() == Some(id) {
            Srgba::new(0xff, 0xd8, 0x5a, 255)
        } else if self
            .selected
            .as_deref()
            .is_some_and(|s| self.map.neighbors(s).contains(&id))
        {
            Srgba::new(0x8f, 0xd0, 0xff, 255)
        } else {
            base
        }
    }

    fn handle_pan(&self, camera: &mut Camera, events: &mut [Event]) {
        for event in events.iter_mut() {
            if let Event::MouseMotion {
                button: Some(MouseButton::Right),
                delta,
                handled: false,
                ..
            } = event
            {
                let d = *delta;
                let dist = camera.target().distance(camera.position());
                let scale = dist * 0.0015;
                let change = camera.right_direction() * (-d.0 * scale)
                    + camera.up_orthogonal() * (d.1 * scale);
                camera.translate(change);
            }
        }
    }

    fn handle_pick(&mut self, context: &Context, camera: &Camera, events: &mut [Event]) {
        for event in events.iter_mut() {
            if let Event::MousePress {
                button: MouseButton::Left,
                position,
                handled: false,
                ..
            } = event
            {
                let hit =
                    pick(context, camera, *position, [&self.buildings], Cull::Back).unwrap_or(None);
                self.selected = hit.map(|r| self.node_ids[r.instance_id as usize].clone());
            }
        }
    }

    fn render_objects(&self, screen: &RenderTarget, camera: &Camera) {
        let objects = self
            .ground
            .into_iter()
            .chain(&self.grid)
            .chain(&self.edges)
            .chain(&self.buildings);
        screen.render(camera, objects, &[&self.light, &self.ambient]);
    }

    // ── egui panels (overlaid on the 3D scene) ────────────────────────────

    fn panels_ui(&mut self, ui: &mut egui::Ui, camera: &Camera, viewport: Viewport, dpr: f32) {
        egui::Panel::top("toolbar")
            .exact_size(36.0)
            .frame(
                egui::Frame::NONE
                    .fill(PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show_inside(ui, |ui| self.toolbar_ui(ui));

        egui::Panel::bottom("details")
            .resizable(true)
            .default_size(180.0)
            .min_size(80.0)
            .frame(
                egui::Frame::NONE
                    .fill(PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show_inside(ui, |ui| self.details_ui(ui));

        egui::Panel::right("legend")
            .resizable(true)
            .default_size(400.0)
            .min_size(260.0)
            .frame(
                egui::Frame::NONE
                    .fill(PANEL_BG)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show_inside(ui, |ui| self.legend_ui(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| self.labels_ui(ui, camera, viewport, dpr));
    }

    fn labels_ui(&self, ui: &mut egui::Ui, camera: &Camera, viewport: Viewport, dpr: f32) {
        let painter = ui.painter();
        for id in &self.node_ids {
            let Some(p) = self.placed.get(id) else {
                continue;
            };
            let top = vec3(p.wx, p.height + 0.15, p.wy);
            let px = camera.pixel_at_position(top);
            let logical = egui::pos2(px.x / dpr, (viewport.height as f32 - px.y) / dpr);
            if !logical.x.is_finite() || !logical.y.is_finite() {
                continue;
            }
            let label = self
                .map
                .node(id)
                .map(|n| n.label.clone())
                .unwrap_or_else(|| id.clone());
            painter.text(
                logical,
                egui::Align2::CENTER_BOTTOM,
                label,
                egui::FontId::proportional(11.0),
                color32(style::LABEL),
            );
        }
    }

    fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Liberado system map (3D)").strong());
            ui.separator();
            ui.toggle_value(&mut self.show_deps, "dependencies");
            ui.toggle_value(&mut self.show_runtime, "runtime paths");
            ui.separator();
            ui.label(
                egui::RichText::new("left-drag orbit · wheel zoom · right-drag pan · click select")
                    .weak()
                    .small(),
            );
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
                egui::RichText::new(
                    "Buildings are colored by architectural layer; height is dependency hub-ness.",
                )
                .small()
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
            edge_row(
                ui,
                EdgeKind::Dependency,
                "build-time dependency (Cargo.toml)",
            );
            edge_row(ui, EdgeKind::Control, "runtime control flow");
            edge_row(ui, EdgeKind::Data, "runtime data / payload flow");

            ui.add_space(8.0);
            explainer_ui(ui);
        });
    }

    fn details_ui(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            match &self.selected {
                Some(id) => self.node_detail(ui, id),
                None => {
                    ui.label(
                        egui::RichText::new(
                            "Click a building to inspect it. Left-drag to orbit, wheel to zoom, right-drag to pan.",
                        )
                        .weak(),
                    );
                }
            }
        });
    }

    fn node_detail(&self, ui: &mut egui::Ui, id: &str) {
        let Some(node) = self.map.node(id) else {
            return;
        };
        let color = style::node_color(
            &self.map.vocabulary,
            node.layer.as_str(),
            node.kind.as_str(),
        );
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, color32(color));
            ui.heading(&node.label);
        });
        let kind_label = self
            .map
            .vocabulary
            .kind(node.kind.as_str())
            .map(|k| k.label.as_str())
            .unwrap_or_else(|| node.kind.as_str());
        ui.label(egui::RichText::new(format!("{kind_label} · layer: {}", node.layer)).weak());
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
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn base(p: &PlacedNode) -> Vec3 {
    vec3(p.wx, 0.0, p.wy)
}

fn building_transform(p: &PlacedNode) -> Mat4 {
    Mat4::from_translation(vec3(p.wx, p.height * 0.5, p.wy))
        * Mat4::from_nonuniform_scale(p.half, p.height * 0.5, p.half)
}

fn edge_thickness(kind: EdgeKind) -> f32 {
    match kind {
        EdgeKind::Dependency => 0.03,
        EdgeKind::Control | EdgeKind::Data => 0.05,
    }
}

/// Transform for a cylinder (unit radius around +X, spanning x in [0,1]) placed from `a` to `b`.
fn segment_transform(a: Vec3, b: Vec3, thickness: f32) -> Option<Mat4> {
    let dir = b - a;
    let len = dir.magnitude();
    if len < 1e-3 {
        return None;
    }
    let d = dir / len;
    let rot = rotation_matrix_from_dir_to_dir(vec3(1.0, 0.0, 0.0), d);
    Some(Mat4::from_translation(a) * rot * Mat4::from_nonuniform_scale(len, thickness, thickness))
}

fn grid_transforms(placed: &BTreeMap<String, PlacedNode>) -> Vec<Mat4> {
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    for p in placed.values() {
        min_x = min_x.min(p.wx - p.half);
        max_x = max_x.max(p.wx + p.half);
        min_z = min_z.min(p.wy - p.half);
        max_z = max_z.max(p.wy + p.half);
    }
    if !min_x.is_finite() {
        return Vec::new();
    }
    let step = 2.0;
    let mut out = Vec::new();
    let t = 0.012;
    let mut x = (min_x / step).floor() * step;
    while x <= max_x {
        if let Some(m) = segment_transform(vec3(x, 0.0, min_z), vec3(x, 0.0, max_z), t) {
            out.push(m);
        }
        x += step;
    }
    let mut z = (min_z / step).floor() * step;
    while z <= max_z {
        if let Some(m) = segment_transform(vec3(min_x, 0.0, z), vec3(max_x, 0.0, z), t) {
            out.push(m);
        }
        z += step;
    }
    out
}

fn srgba(c: Rgb) -> Srgba {
    Srgba::new(c.r, c.g, c.b, 255)
}

/// A white, opaque, lit material — per-instance colors multiply onto this.
fn lit_white(context: &Context) -> PhysicalMaterial {
    PhysicalMaterial::new_opaque(
        context,
        &CpuMaterial {
            albedo: Srgba::WHITE,
            ..Default::default()
        },
    )
}

fn color32(c: Rgb) -> egui::Color32 {
    egui::Color32::from_rgb(c.r, c.g, c.b)
}

const PANEL_BG: egui::Color32 = egui::Color32::from_rgb(0x1a, 0x1e, 0x26);

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

fn explainer_ui(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.separator();
    ui.heading("About this map");
    ui.label(
        "Liberado is a Rust-native personal AI life OS: a daemon that watches an Obsidian vault, \
         reasons with an LLM, and acts through tools — safely. This map is generated from source, \
         not hand-drawn:",
    );
    ui.label("• every building is a workspace crate (colored by architectural layer), or a runtime component declared in topology.toml.");
    ui.label("• gray edges are build-time dependencies from Cargo.toml.");
    ui.label("• orange/green edges are runtime control and data paths — the perceive → decide → act loop, surfaces, inference, and notification.");
    ui.label(
        "• building height is hub-ness: how many crates depend on it plus how many it depends on.",
    );
    ui.add_space(4.0);
    ui.label(
        "Runtime paths are declared by each crate under [[package.metadata.liberado.flows]] in its \
         Cargo.toml, so the map grows with the codebase, not with this tool. The map is rebuilt on \
         every launch; run `liberado-sysmap --write-json out.json` for the serialized graph.",
    );
}
