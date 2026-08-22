//! Detail-panel and dependency-health presentation helpers.

use eframe::egui::{self, Color32};
use sysmap_core::model::MapNode;
use sysmap_core::style::Rgb;

pub(crate) fn outline_color(in_cycle: bool, base: Rgb) -> Rgb {
    if in_cycle {
        Rgb::new(0xff, 0x55, 0x55)
    } else {
        base.tint(0.35)
    }
}

pub(crate) fn show_cycle_warning(ui: &mut egui::Ui, cycles: &[Vec<String>], id: &str) {
    let Some(cycle) = cycles
        .iter()
        .find(|cycle| cycle.iter().any(|member| member == id))
    else {
        return;
    };
    ui.label(
        egui::RichText::new(format!(
            "Production dependency cycle: {}",
            cycle.join(" -> ")
        ))
        .color(Color32::from_rgb(0xff, 0x77, 0x77))
        .strong(),
    );
}

pub(crate) fn show_metadata(ui: &mut egui::Ui, node: &MapNode) {
    if node.meta.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (key, value) in &node.meta {
            ui.label(egui::RichText::new(format!("{key}: {value}")).weak());
        }
    });
}
