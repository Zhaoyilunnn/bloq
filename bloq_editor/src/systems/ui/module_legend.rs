//! Compact viewport legend for Module View.

use crate::resources::{EditorMode, EditorState, GraphState};
use crate::theme::{self, ThemePalette};
use bevy_egui::egui;

pub(crate) fn draw_module_legend(
    ctx: &egui::Context,
    viewport: egui::Rect,
    editor_state: &EditorState,
    graph_state: &GraphState,
    palette: &ThemePalette,
) {
    if editor_state.mode != EditorMode::Module || viewport.width() < 320.0 {
        return;
    }
    let Some(module_view) = &editor_state.module_view else {
        return;
    };

    egui::Area::new(egui::Id::new("module_view_legend"))
        .order(egui::Order::Foreground)
        .fixed_pos(viewport.left_top() + egui::vec2(10.0, 10.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(palette.bg_panel)
                .stroke(egui::Stroke::new(1.0, palette.border_bright))
                .corner_radius(5.0)
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(210.0);
                    ui.label(
                        egui::RichText::new(format!("MODULES · {}", module_view.modules().len()))
                            .small()
                            .strong()
                            .color(palette.text_bright),
                    );
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height((viewport.height() * 0.55).clamp(96.0, 420.0))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (index, module) in module_view.modules().iter().enumerate() {
                                ui.horizontal(|ui| {
                                    let (swatch, response) = ui.allocate_exact_size(
                                        egui::vec2(13.0, 13.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(
                                        swatch,
                                        2.0,
                                        theme::module_color(index),
                                    );
                                    ui.painter().rect_stroke(
                                        swatch,
                                        2.0,
                                        egui::Stroke::new(1.0, palette.border_bright),
                                        egui::StrokeKind::Inside,
                                    );
                                    response.on_hover_text(format!(
                                        "{} block{} across {} instance{}",
                                        module.block_count,
                                        if module.block_count == 1 { "" } else { "s" },
                                        module.instance_count,
                                        if module.instance_count == 1 { "" } else { "s" },
                                    ));
                                    ui.label(
                                        egui::RichText::new(&module.name)
                                            .small()
                                            .color(palette.text_primary),
                                    );
                                    if module.instance_count > 1 {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "×{}",
                                                        module.instance_count
                                                    ))
                                                    .small()
                                                    .color(palette.text_dim),
                                                );
                                            },
                                        );
                                    }
                                });
                            }
                        });
                    ui.separator();
                    ui.label(
                        egui::RichText::new("Shared links keep normal colors")
                            .small()
                            .color(palette.text_dim),
                    );
                });
        });

    let Some(pointer) = ctx
        .pointer_latest_pos()
        .filter(|pointer| viewport.contains(*pointer))
    else {
        return;
    };
    let Some(module) = editor_state
        .hovered_element
        .and_then(|element| module_view.module_for_element(&graph_state.graph, element))
        .and_then(|index| module_view.modules().get(index))
    else {
        return;
    };

    egui::Area::new(egui::Id::new("module_view_hover"))
        .order(egui::Order::Foreground)
        .interactable(false)
        .fixed_pos(pointer + egui::vec2(12.0, 12.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.label(
                    egui::RichText::new(&module.name)
                        .small()
                        .strong()
                        .color(palette.text_bright),
                );
            });
        });
}
