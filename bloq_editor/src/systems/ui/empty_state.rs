//! First steps on a fresh editor launch.

use super::{UiIntent, UiIntentBuffer};
use crate::resources::{EditorMode, EditorState, GraphState};
use crate::theme::ThemePalette;
use bevy_egui::egui;

pub(super) fn draw_empty_state(
    ctx: &egui::Context,
    viewport: egui::Rect,
    editor: &mut EditorState,
    graph: &GraphState,
    show_welcome: &mut bool,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    if !*show_welcome {
        return;
    }
    if editor.mode != EditorMode::View
        || !graph.graph.is_empty()
        || graph.source_graph.is_some()
        || editor.show_blog_buffer
        || editor.gallery_panel_expanded
    {
        *show_welcome = false;
        return;
    }
    if viewport.width() < 300.0 || viewport.height() < 280.0 {
        return;
    }
    egui::Area::new(egui::Id::new("empty_graph"))
        .pivot(egui::Align2::CENTER_CENTER)
        .fixed_pos(viewport.center())
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(palette.bg_surface)
                .stroke(egui::Stroke::new(1.0, palette.border))
                .corner_radius(12)
                .inner_margin(20)
                .show(ui, |ui| {
                    ui.set_width((viewport.width() - 64.0).min(400.0));
                    ui.heading("Build a graph");
                    ui.add_space(10.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add(
                                egui::Button::new(egui::RichText::new("Start Building").color(
                                    if palette.dark_mode {
                                        palette.bg_dark
                                    } else {
                                        egui::Color32::WHITE
                                    },
                                ))
                                .fill(palette.accent_primary),
                            )
                            .clicked()
                        {
                            *show_welcome = false;
                            intents.push(UiIntent::SetMode(EditorMode::Edit));
                        }
                        if ui.button("Open BLOG").clicked() {
                            *show_welcome = false;
                            intents.push(UiIntent::ImportBlogFromFile);
                        }
                        if ui.button("Browse Gallery").clicked() {
                            *show_welcome = false;
                            editor.gallery_panel_expanded = true;
                            editor.show_side_panel = false;
                        }
                    });
                });
        });
}
