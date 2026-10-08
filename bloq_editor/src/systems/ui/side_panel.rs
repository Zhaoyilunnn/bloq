//! The left side panel: gallery browser and per-graph working tools (validate,
//! stabilizers, transforms).

// ==============================================================================
// Side Panels — Left working surface and right gallery browser
// ==============================================================================

use std::sync::LazyLock;

use super::intents::{UiIntent, UiIntentBuffer};
use super::thumbnail::paint_texture_thumbnail;
use super::{activated, fuzzy_score};
use crate::components::GraphElement;
use crate::resources::{
    ActionViewerState, EditorMode, EditorState, GraphRotationAvailability, GraphState,
    GraphUiSummary, ThumbnailTextures,
};
use crate::systems::jobs::EditorJobs;
use crate::systems::thumbnails::ThumbnailKey;
use crate::theme::{self, ThemePalette};
use bevy::prelude::{Color, Srgba};
use bevy_egui::egui;
use bloq_graph::{GalleryCategory, GalleryItem, UDirection};

const LEFT_PANEL_DEFAULT_WIDTH: f32 = 320.0;
const LEFT_PANEL_MIN_WIDTH: f32 = 260.0;
const LEFT_PANEL_MAX_WIDTH: f32 = 440.0;
const LEFT_PANEL_EXPANDED_ID: &str = "properties_panel_expanded";
const LEFT_PANEL_COLLAPSED_ID: &str = "properties_panel_collapsed";
const LEFT_PANEL_COLLAPSED_WIDTH: f32 = 42.0;
const GALLERY_PANEL_EXPANDED_ID: &str = "gallery_panel_expanded";
const GALLERY_PANEL_COLLAPSED_ID: &str = "gallery_panel_collapsed";
const GALLERY_PANEL_DEFAULT_WIDTH: f32 = 340.0;
const GALLERY_PANEL_MIN_WIDTH: f32 = 208.0;
const GALLERY_PANEL_MAX_WIDTH: f32 = 340.0;
const GALLERY_PANEL_COLLAPSED_WIDTH: f32 = 42.0;
const GALLERY_TOGGLE_BUTTON_SIZE: egui::Vec2 = egui::vec2(28.0, 28.0);
const GALLERY_CARD_MIN_WIDTH: f32 = 96.0;
const GALLERY_CARD_GAP: f32 = 8.0;
const GALLERY_CARD_PADDING: f32 = 6.0;
const GALLERY_CARD_STROKE: f32 = 1.0;
/// Horizontal space a card's frame adds around its content (inner margin +
/// border stroke, both sides). `card_width` means the card's *outer* width, so
/// the grid must fit `columns * card_width` rows and the card must size its
/// content to `card_width - GALLERY_CARD_CHROME`. If rows exceed the panel's
/// inner width, egui grows the panel every frame to chase its own content and
/// the resize separator drifts off the panel edge.
const GALLERY_CARD_CHROME: f32 = 2.0 * (GALLERY_CARD_PADDING + GALLERY_CARD_STROKE);
const GALLERY_CARD_LABEL_HEIGHT: f32 = 28.0;
const GALLERY_GRID_COLUMNS: usize = 2;
const GALLERY_GRID_RIGHT_PADDING: f32 = 16.0;
const COMPACT_EDITOR_WIDTH: f32 = 1000.0;

static GALLERY_ENTRIES: LazyLock<GalleryEntryCache> = LazyLock::new(GalleryEntryCache::build);

struct GalleryEntryCache {
    all: Vec<GalleryItem>,
    clifford: Vec<GalleryItem>,
    non_clifford: Vec<GalleryItem>,
    factory: Vec<GalleryItem>,
    external_resource: Vec<GalleryItem>,
    adaptive: Vec<GalleryItem>,
    arithmetic: Vec<GalleryItem>,
    addition: Vec<GalleryItem>,
}

impl GalleryEntryCache {
    fn build() -> Self {
        Self {
            all: sorted_gallery_entries(None),
            clifford: sorted_gallery_entries(Some(GalleryCategory::Clifford)),
            non_clifford: sorted_gallery_entries(Some(GalleryCategory::NonClifford)),
            factory: sorted_gallery_entries(Some(GalleryCategory::Factory)),
            external_resource: sorted_gallery_entries(Some(GalleryCategory::ExternalResource)),
            adaptive: sorted_gallery_entries(Some(GalleryCategory::Adaptive)),
            arithmetic: sorted_gallery_entries(Some(GalleryCategory::Arithmetic)),
            addition: sorted_gallery_entries(Some(GalleryCategory::Addition)),
        }
    }

    fn entries(&self, category: Option<GalleryCategory>) -> &[GalleryItem] {
        match category {
            None => &self.all,
            Some(GalleryCategory::Clifford) => &self.clifford,
            Some(GalleryCategory::NonClifford) => &self.non_clifford,
            Some(GalleryCategory::Factory) => &self.factory,
            Some(GalleryCategory::ExternalResource) => &self.external_resource,
            Some(GalleryCategory::Adaptive) => &self.adaptive,
            Some(GalleryCategory::Arithmetic) => &self.arithmetic,
            Some(GalleryCategory::Addition) => &self.addition,
            Some(GalleryCategory::AnalysisOnly) => &[],
        }
    }
}

#[derive(Clone, Copy)]
enum PanelSide {
    Left,
    Right,
}

/// Draws the left side panel into `root`, shrinking the shared central viewport
/// by the width it claims (hidden in Bloq mode).
pub(crate) fn draw_side_panel(
    root: &mut egui::Ui,
    editor_state: &mut EditorState,
    action_viewer: &mut ActionViewerState,
    graph_state: &GraphState,
    graph_ui_summary: &GraphUiSummary,
    jobs: &EditorJobs,
    thumbnails: &mut ThumbnailTextures,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    if editor_state.mode.covers_viewport() || editor_state.mode == EditorMode::Module {
        return;
    }

    // Both panels draw into the same root `Ui`, so egui composes them: the left
    // panel claims the left edge and the gallery the right, leaving the middle.
    draw_left_panel(
        root,
        editor_state,
        action_viewer,
        graph_state,
        graph_ui_summary,
        jobs,
        intents,
        palette,
    );
    draw_gallery_panel(root, editor_state, thumbnails, intents, palette);
}

fn draw_left_panel(
    root: &mut egui::Ui,
    editor_state: &mut EditorState,
    action_viewer: &mut ActionViewerState,
    graph_state: &GraphState,
    graph_ui_summary: &GraphUiSummary,
    jobs: &EditorJobs,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    if editor_state.mode != EditorMode::View {
        return;
    }
    let compact = root.ctx().content_rect().width() < COMPACT_EDITOR_WIDTH;
    let expanded =
        editor_state.show_side_panel && (!compact || !editor_state.gallery_panel_expanded);
    let panel_id = if expanded {
        LEFT_PANEL_EXPANDED_ID
    } else {
        LEFT_PANEL_COLLAPSED_ID
    };
    let mut panel = egui::Panel::left(panel_id).frame(
        egui::Frame::new()
            .fill(palette.bg_panel)
            .stroke(egui::Stroke::new(1.0, palette.border))
            .inner_margin(egui::Margin::symmetric(8, 8)),
    );

    panel = if expanded {
        panel
            .default_size(LEFT_PANEL_DEFAULT_WIDTH)
            .size_range(LEFT_PANEL_MIN_WIDTH..=LEFT_PANEL_MAX_WIDTH)
            .resizable(true)
    } else {
        panel
            .exact_size(LEFT_PANEL_COLLAPSED_WIDTH)
            .resizable(false)
    };

    panel.show(root, |ui| {
        ui.horizontal(|ui| {
            let toggle_hover = if expanded {
                "Collapse controls"
            } else {
                "Expand controls"
            };
            let response = panel_toggle_button(ui, expanded, PanelSide::Left, palette)
                .on_hover_text(toggle_hover);
            if activated(&response) {
                editor_state.show_side_panel = !expanded;
                if compact && editor_state.show_side_panel {
                    editor_state.gallery_panel_expanded = false;
                }
            }

            if expanded {
                ui.label(
                    egui::RichText::new("Controls")
                        .strong()
                        .color(palette.text_bright),
                );
            }
        });

        if !expanded {
            return;
        }

        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                draw_view_section(ui, editor_state, intents, palette);
                draw_graph_tools_section(
                    ui,
                    editor_state,
                    action_viewer,
                    graph_ui_summary,
                    jobs,
                    intents,
                    palette,
                );
                draw_branch_section(ui, editor_state, graph_state, intents, palette);
                draw_transform_section(ui, editor_state, graph_ui_summary, intents, palette);
            });
    });
}

fn draw_gallery_panel(
    root: &mut egui::Ui,
    editor_state: &mut EditorState,
    thumbnails: &mut ThumbnailTextures,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    let compact = root.ctx().content_rect().width() < COMPACT_EDITOR_WIDTH;
    if editor_state.mode.covers_viewport() {
        return;
    }

    let expanded = editor_state.gallery_panel_expanded;
    let panel_id = if expanded {
        GALLERY_PANEL_EXPANDED_ID
    } else {
        GALLERY_PANEL_COLLAPSED_ID
    };
    let mut panel = egui::Panel::right(panel_id)
        .frame(
            egui::Frame::new()
                .fill(palette.bg_panel)
                .stroke(egui::Stroke::new(1.0, palette.border))
                .inner_margin(egui::Margin::symmetric(8, 8)),
        )
        .show_separator_line(true);

    panel = if expanded {
        panel
            .default_size(GALLERY_PANEL_DEFAULT_WIDTH)
            .size_range(GALLERY_PANEL_MIN_WIDTH..=GALLERY_PANEL_MAX_WIDTH)
            .resizable(true)
    } else {
        panel
            .exact_size(GALLERY_PANEL_COLLAPSED_WIDTH)
            .resizable(false)
    };

    panel.show(root, |ui| {
        ui.horizontal(|ui| {
            let toggle_hover = if expanded {
                "Collapse gallery"
            } else {
                "Expand gallery"
            };
            let response = panel_toggle_button(ui, expanded, PanelSide::Right, palette)
                .on_hover_text(toggle_hover);
            if activated(&response) {
                editor_state.gallery_panel_expanded = !expanded;
                if compact && editor_state.gallery_panel_expanded {
                    editor_state.show_side_panel = false;
                }
            }

            if expanded {
                ui.label(
                    egui::RichText::new("Gallery")
                        .strong()
                        .color(palette.text_bright),
                );
            }
        });

        if !expanded {
            return;
        }

        ui.add_space(8.0);
        draw_gallery_tabs(ui, editor_state, palette);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut editor_state.gallery_search)
                    .desired_width((ui.available_width() - 34.0).max(40.0))
                    .hint_text("Search examples")
                    .font(egui::TextStyle::Small),
            );
            if ui
                .add_enabled(
                    !editor_state.gallery_search.is_empty(),
                    egui::Button::new("\u{f00d}"),
                )
                .on_hover_text("Clear search")
                .clicked()
            {
                editor_state.gallery_search.clear();
            }
        });
        ui.small("Click to open a tab · Ctrl+click to insert");
        ui.add_space(6.0);
        draw_gallery_entries(ui, editor_state, thumbnails, intents, palette);
    });
}

fn panel_toggle_button(
    ui: &mut egui::Ui,
    expanded: bool,
    side: PanelSide,
    palette: &ThemePalette,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(GALLERY_TOGGLE_BUTTON_SIZE, egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            match (side, expanded) {
                (PanelSide::Left, true) => "Collapse controls",
                (PanelSide::Left, false) => "Expand controls",
                (PanelSide::Right, true) => "Collapse gallery",
                (PanelSide::Right, false) => "Expand gallery",
            },
        )
    });
    let background = if response.hovered() || response.has_focus() {
        palette.bg_hover
    } else {
        palette.bg_surface
    };
    let border = if response.hovered() || response.has_focus() {
        palette.border_bright
    } else {
        palette.border
    };
    let icon = if response.hovered() || response.has_focus() {
        palette.text_bright
    } else {
        palette.grey2
    };
    let accent = if response.hovered() || response.has_focus() {
        palette.accent_secondary
    } else {
        palette.text_dim
    };

    ui.painter().rect_filled(rect, 7.0, background);
    ui.painter().rect_stroke(
        rect,
        7.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );

    let icon_rect = rect.shrink2(egui::vec2(6.0, 6.0));
    ui.painter().rect_stroke(
        icon_rect,
        4.0,
        egui::Stroke::new(1.4, icon),
        egui::StrokeKind::Inside,
    );

    let sidebar_x = match side {
        PanelSide::Left => icon_rect.left() + icon_rect.width() * 0.36,
        PanelSide::Right => icon_rect.right() - icon_rect.width() * 0.36,
    };
    ui.painter().line_segment(
        [
            egui::pos2(sidebar_x, icon_rect.top() + 1.5),
            egui::pos2(sidebar_x, icon_rect.bottom() - 1.5),
        ],
        egui::Stroke::new(1.2, icon),
    );

    let chevron_center = match side {
        PanelSide::Left => egui::pos2(
            f32::midpoint(sidebar_x, icon_rect.right()),
            icon_rect.center().y,
        ),
        PanelSide::Right => egui::pos2(
            f32::midpoint(icon_rect.left(), sidebar_x),
            icon_rect.center().y,
        ),
    };
    let chevron_dx = 2.4;
    let chevron_dy = 3.0;
    let chevron_points = match (side, expanded) {
        (PanelSide::Left, false) | (PanelSide::Right, true) => [
            egui::pos2(chevron_center.x - chevron_dx, chevron_center.y - chevron_dy),
            egui::pos2(chevron_center.x + chevron_dx, chevron_center.y),
            egui::pos2(chevron_center.x - chevron_dx, chevron_center.y + chevron_dy),
        ],
        (PanelSide::Left, true) | (PanelSide::Right, false) => [
            egui::pos2(chevron_center.x + chevron_dx, chevron_center.y - chevron_dy),
            egui::pos2(chevron_center.x - chevron_dx, chevron_center.y),
            egui::pos2(chevron_center.x + chevron_dx, chevron_center.y + chevron_dy),
        ],
    };
    ui.painter().line_segment(
        [chevron_points[0], chevron_points[1]],
        egui::Stroke::new(1.4, accent),
    );
    ui.painter().line_segment(
        [chevron_points[1], chevron_points[2]],
        egui::Stroke::new(1.4, accent),
    );

    response
}

// =============================================================================
// View Settings
// =============================================================================

fn draw_view_section(
    ui: &mut egui::Ui,
    editor_state: &mut EditorState,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    theme::section_header(ui, palette, "\u{f06e} View");
    theme::section_frame(ui, palette, |ui| {
        egui::Grid::new("view_grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Pipe Length");
                draw_pipe_length_control(ui, editor_state, intents, palette);
                ui.end_row();

                ui.label("Plane Height");
                draw_plane_height_control(ui, editor_state, intents);
                ui.end_row();

                ui.label("Background");
                let rgba = editor_state.bg_color.to_srgba();
                let mut color_arr = [rgba.red, rgba.green, rgba.blue, rgba.alpha];
                if ui
                    .color_edit_button_rgba_unmultiplied(&mut color_arr)
                    .changed()
                {
                    editor_state.bg_color = Color::Srgba(Srgba::new(
                        color_arr[0],
                        color_arr[1],
                        color_arr[2],
                        color_arr[3],
                    ));
                    intents.push(UiIntent::SetBackgroundColor(editor_state.bg_color));
                }
                ui.end_row();
            });

        ui.add_space(4.0);

        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut editor_state.show_axis, "Show Axes")
                .changed()
            {
                intents.push(UiIntent::SetAxisVisibility(editor_state.show_axis));
            }
            ui.checkbox(&mut editor_state.show_grid, "Show Grid");
        });

        if ui
            .checkbox(&mut editor_state.show_port_tags, "Show Port Tags")
            .changed()
        {
            intents.push(UiIntent::RerenderGraph);
        }

        if editor_state.mode == EditorMode::View
            && ui
                .checkbox(
                    &mut editor_state.view_current_layer_only,
                    "Highlight Current Layer",
                )
                .changed()
        {
            intents.push(UiIntent::SetViewCurrentLayerOnly(
                editor_state.view_current_layer_only,
            ));
        }

        ui.add_space(4.0);
        if activated(&ui.button("Reset Camera")) {
            intents.push(UiIntent::ResetCamera);
        }
    });
}

fn draw_pipe_length_control(
    ui: &mut egui::Ui,
    editor_state: &mut EditorState,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    egui::Frame::new()
        .fill(pipe_length_slider_fill(palette))
        .stroke(egui::Stroke::new(1.0, pipe_length_slider_stroke(palette)))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.set_min_width(206.0);
            ui.scope(|ui| {
                if !palette.dark_mode {
                    let visuals = &mut ui.style_mut().visuals.widgets;
                    visuals.inactive.bg_fill = palette.bg_active;
                    visuals.inactive.bg_stroke = egui::Stroke::new(1.0, palette.border_bright);
                    visuals.hovered.bg_fill = palette.bg_hover;
                    visuals.hovered.bg_stroke = egui::Stroke::new(1.0, palette.accent_primary);
                    visuals.active.bg_fill = palette.bg_active;
                    visuals.active.bg_stroke = egui::Stroke::new(1.0, palette.accent_primary);
                }

                // Rerender only when the drag ends: each tick would otherwise
                // mint a fresh mesh/material generation for a value the user is
                // still scrubbing through.
                let response = ui.add(
                    egui::Slider::new(&mut editor_state.pipe_length, 0.5..=10.0)
                        .show_value(true)
                        .text(""),
                );
                if response.drag_stopped() || (response.changed() && !response.dragged()) {
                    intents.push(UiIntent::RerenderGraph);
                }
            });
        });
}

fn draw_plane_height_control(
    ui: &mut egui::Ui,
    editor_state: &EditorState,
    intents: &mut UiIntentBuffer,
) {
    let mut plane_height = editor_state.plane_height;
    ui.horizontal(|ui| {
        let decrement_response = ui.small_button("-").on_hover_text("Plane down (J)");
        if activated(&decrement_response) {
            plane_height = plane_height.saturating_sub(1);
        }

        let changed = ui
            .add(
                egui::DragValue::new(&mut plane_height)
                    .speed(1)
                    .range(-9999..=9999)
                    .prefix("z="),
            )
            .on_hover_text("Type an exact plane height or drag to adjust")
            .changed();

        let increment_response = ui.small_button("+").on_hover_text("Plane up (K)");
        if activated(&increment_response) {
            plane_height = plane_height.saturating_add(1);
        }

        if changed || plane_height != editor_state.plane_height {
            intents.push(UiIntent::SetPlaneHeight(plane_height));
        }
    });
}

fn pipe_length_slider_fill(palette: &ThemePalette) -> egui::Color32 {
    if palette.dark_mode {
        palette.bg_dark
    } else {
        egui::Color32::from_rgb(226, 236, 246)
    }
}

fn pipe_length_slider_stroke(palette: &ThemePalette) -> egui::Color32 {
    if palette.dark_mode {
        palette.border
    } else {
        palette.border_bright
    }
}

// =============================================================================
// Graph Tools
// =============================================================================

fn draw_graph_tools_section(
    ui: &mut egui::Ui,
    editor_state: &EditorState,
    action_viewer: &mut ActionViewerState,
    graph_ui_summary: &GraphUiSummary,
    jobs: &EditorJobs,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    theme::section_header(ui, palette, "\u{f1b3} Graph");
    theme::section_frame(ui, palette, |ui| {
        let fill_cycle = editor_state.active_fill_ports_cycle(graph_ui_summary.revision);
        let fill_ports_available = graph_ui_summary.is_open || fill_cycle.is_some();
        let fill_ports_label = if let Some(cycle) = fill_cycle {
            format!(
                "Fill Ports (F)  {}/{}",
                cycle.current_index + 1,
                cycle.variants.len()
            )
        } else {
            "Fill Ports (F)".to_string()
        };

        ui.horizontal_wrapped(|ui| {
            let response = ui.add_enabled(
                !jobs.validation_running(),
                egui::Button::new("Validate Graph"),
            );
            if activated(&response) {
                intents.push(UiIntent::ValidateGraph);
            }

            let response = ui.add_enabled(
                !jobs.stabilizers_running(),
                egui::Button::new("Toggle Stabilizers"),
            );
            if activated(&response) {
                intents.push(UiIntent::ToggleStabilizers {
                    layer_only: editor_state.view_current_layer_only,
                    plane_height: editor_state.plane_height,
                });
            }
        });

        ui.add_space(6.0);

        if editor_state.showing_stabilizers() {
            let label = if let Some((_, generator)) = &editor_state.action_stabilizer {
                format!(
                    "Measurement surface: {} (selected in Actions)",
                    generator.measurement_name().unwrap_or("unnamed")
                )
            } else {
                format!(
                    "Stabilizer view: Q/E cycles ({}/{})",
                    editor_state.current_stabilizer_index + 1,
                    editor_state.stabilizers.len()
                )
            };
            ui.label(egui::RichText::new(label).small().color(palette.text_dim));
            ui.add_space(6.0);
        }

        ui.horizontal_wrapped(|ui| {
            let response =
                ui.add_enabled(fill_ports_available, egui::Button::new(fill_ports_label));
            if activated(&response) {
                intents.push(UiIntent::FillPorts);
            }

            if activated(&ui.button("Fix Shadow")) {
                intents.push(UiIntent::FixShadowedFaces);
            }

            if activated(&ui.button("Flip XZ Basis")) {
                intents.push(UiIntent::FlipXZBasis);
            }

            if activated(&ui.button("Random Selective Basis")) {
                intents.push(UiIntent::RandomlyResolveSelectives);
            }

            // The Actions window has no toolbar button; this is its entry point.
            let response = ui
                .add(
                    egui::Button::new("\u{f0e8} Actions (A)").fill(if action_viewer.open {
                        palette.bg_active
                    } else {
                        palette.bg_surface
                    }),
                )
                .on_hover_text("Show the action DAG, and add or edit actions");
            if activated(&response) {
                action_viewer.toggle();
            }
        });

        if let Some(cycle) = fill_cycle {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "Press F again to cycle variants ({}/{})",
                    cycle.current_index + 1,
                    cycle.variants.len()
                ))
                .small()
                .color(palette.text_dim),
            );
        }
    });
}

fn draw_branch_section(
    ui: &mut egui::Ui,
    editor_state: &mut EditorState,
    graph_state: &GraphState,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    let graph = &graph_state.graph;
    theme::section_header(ui, palette, "\u{f126} Branches");
    theme::section_frame(ui, palette, |ui| {
        let selected_blocks = editor_state
            .selected_elements()
            .filter(|element| matches!(element, GraphElement::Block(_)))
            .count();
        let selected_pipes = editor_state
            .selected_elements()
            .filter(|element| matches!(element, GraphElement::Pipe(..)))
            .count();

        if let Some(pending) = graph_state.pending_branch_arm.as_ref() {
            let (captured, next) = if pending.captured_true {
                ("true", "false")
            } else {
                ("false", "true")
            };
            ui.label(format!(
                "{}: {captured} arm hidden ({} blocks, {} pipes)",
                pending.name,
                pending.arm.blocks().count(),
                pending.arm.pipes().count()
            ));
            ui.small(format!(
                "Build and select every block and pipe in the {next} arm."
            ));
            ui.horizontal_wrapped(|ui| {
                let finish = ui.add_enabled(
                    selected_blocks > 0,
                    egui::Button::new(format!("Finish {next} arm ({selected_blocks})")),
                );
                if activated(&finish) {
                    intents.push(UiIntent::CaptureBranchArm {
                        captured_true: !pending.captured_true,
                    });
                }
                if activated(&ui.button("Cancel")) {
                    intents.push(UiIntent::CancelBranchArm);
                }
            });
        } else {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut editor_state.branch_name);
            });
            let branch_name = editor_state.branch_name.trim();
            let name_available =
                !branch_name.is_empty() && graph.branch_by_name(branch_name).is_none();
            if !branch_name.is_empty() && !name_available {
                ui.small("That branch name is already in use.");
            }
            ui.small(format!(
                "{selected_blocks} block(s), {selected_pipes} pipe(s) selected."
            ));
            ui.horizontal_wrapped(|ui| {
                for (label, captured_true) in [("Mark false arm", false), ("Mark true arm", true)] {
                    let response = ui.add_enabled(
                        selected_blocks > 0 && name_available,
                        egui::Button::new(label),
                    );
                    if activated(&response) {
                        intents.push(UiIntent::CaptureBranchArm { captured_true });
                    }
                }
            });
        }

        if !graph.branch_definitions().is_empty() {
            ui.separator();
            for branch in graph.branch_definitions() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&branch.name).monospace());
                    for (label, show_true) in [("false", false), ("true", true)] {
                        if ui
                            .selectable_label(branch.shown_true() == show_true, label)
                            .clicked()
                        {
                            intents.push(UiIntent::ShowBranchArm {
                                name: branch.name.clone(),
                                show_true,
                            });
                        }
                    }
                });
            }
        }
    });
}

fn draw_transform_section(
    ui: &mut egui::Ui,
    editor_state: &mut EditorState,
    graph_ui_summary: &GraphUiSummary,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    theme::section_header(ui, palette, "\u{f021} Transform");
    theme::section_frame(ui, palette, |ui| {
        let graph_is_empty = graph_ui_summary.is_empty;
        let rotation_availability = graph_ui_summary.rotation_availability;
        let transforming_selection = editor_state.selection_count() > 0;

        ui.label(
            egui::RichText::new(if transforming_selection {
                "Selection transform"
            } else {
                "Whole graph transform"
            })
            .small()
            .color(palette.text_primary),
        );

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Axis").small().color(palette.text_dim));
            ui.radio_value(&mut editor_state.transform_axis, UDirection::X, "X");
            ui.radio_value(&mut editor_state.transform_axis, UDirection::Y, "Y");
            ui.radio_value(&mut editor_state.transform_axis, UDirection::Z, "Z");
        });

        let axis = editor_state.transform_axis;

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Translate")
                    .small()
                    .color(palette.text_dim),
            );
            let response = ui.add_enabled(!graph_is_empty, egui::Button::new("-1"));
            if activated(&response) {
                intents.push(UiIntent::TranslateGraph { axis, step: -1 });
            }
            let response = ui.add_enabled(!graph_is_empty, egui::Button::new("+1"));
            if activated(&response) {
                intents.push(UiIntent::TranslateGraph { axis, step: 1 });
            }
        });

        let rotation_enabled = !graph_is_empty
            && !matches!(rotation_availability, GraphRotationAvailability::Disabled);
        let quarter_turns = match rotation_availability {
            GraphRotationAvailability::HalfTurnsOnly => 2,
            GraphRotationAvailability::QuarterTurns | GraphRotationAvailability::Disabled => 1,
        };

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Rotate")
                    .small()
                    .color(palette.text_dim),
            );
            let response = ui.add_enabled(rotation_enabled, egui::Button::new("\u{f0e2}"));
            if activated(&response) {
                intents.push(UiIntent::RotateGraph {
                    axis,
                    quarter_turns: -quarter_turns,
                });
            }
            let response = ui.add_enabled(rotation_enabled, egui::Button::new("\u{f01e}"));
            if activated(&response) {
                intents.push(UiIntent::RotateGraph {
                    axis,
                    quarter_turns,
                });
            }
        });
    });
}

// =============================================================================
// Gallery
// =============================================================================

fn draw_gallery_tabs(ui: &mut egui::Ui, editor_state: &mut EditorState, palette: &ThemePalette) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 3.0);
        for category in gallery_tabs() {
            let label = match category {
                None => "All".to_string(),
                Some(GalleryCategory::Arithmetic) => "Arithmetic".to_string(),
                Some(GalleryCategory::Addition) => "Addition".to_string(),
                Some(cat) => cat.to_string(),
            };
            let is_selected = editor_state.selected_gallery_category == category;
            let text = egui::RichText::new(label).small().color(if is_selected {
                palette.accent_secondary
            } else {
                palette.text_dim
            });
            let response = ui.add(
                egui::Button::new(text)
                    .fill(if is_selected {
                        palette.bg_active
                    } else {
                        egui::Color32::TRANSPARENT
                    })
                    .stroke(if is_selected {
                        egui::Stroke::new(1.0, palette.accent_secondary)
                    } else {
                        egui::Stroke::NONE
                    }),
            );
            if activated(&response) {
                editor_state.selected_gallery_category = category;
            }
        }
    });
}

fn draw_gallery_entries(
    ui: &mut egui::Ui,
    editor_state: &EditorState,
    thumbnails: &mut ThumbnailTextures,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    let entries = matching_gallery_entries(
        editor_state.selected_gallery_category,
        &editor_state.gallery_search,
    );

    if entries.is_empty() {
        ui.label(
            egui::RichText::new(if editor_state.gallery_search.trim().is_empty() {
                "No entries."
            } else {
                "No matching examples. Try another search or choose All."
            })
            .small()
            .color(palette.text_dim),
        );
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // The right padding is clearance for the (floating) scrollbar: it is
            // reserved by subtracting it here, NOT by allocating space after the
            // cards — allocated trailing space would widen the rows past the
            // panel's inner width (see GALLERY_CARD_CHROME).
            let available_width =
                (ui.available_width() - GALLERY_GRID_RIGHT_PADDING).max(GALLERY_CARD_MIN_WIDTH);
            let columns = GALLERY_GRID_COLUMNS.max(1);
            // Outer card width sized so a full row exactly fills the available
            // width. No lower clamp beyond the `available_width` floor above:
            // clamping cards wider than what fits would overflow the panel.
            let card_width = (available_width
                - GALLERY_CARD_GAP * (columns.saturating_sub(1) as f32))
                / columns as f32;
            let thumbnail_height = (card_width * 0.76).clamp(72.0, 120.0);
            let card_height =
                thumbnail_height + GALLERY_CARD_LABEL_HEIGHT + GALLERY_CARD_PADDING * 2.0;

            ui.spacing_mut().item_spacing = egui::vec2(GALLERY_CARD_GAP, GALLERY_CARD_GAP);
            for row in entries.chunks(columns) {
                ui.horizontal_top(|ui| {
                    for entry in row {
                        draw_gallery_entry_card(
                            ui,
                            *entry,
                            card_width,
                            card_height,
                            thumbnail_height,
                            thumbnails,
                            intents,
                            palette,
                        );
                    }

                    for _ in row.len()..columns {
                        ui.allocate_exact_size(
                            egui::vec2(card_width, card_height),
                            egui::Sense::hover(),
                        );
                    }
                });
            }
        });
}

fn draw_gallery_entry_card(
    ui: &mut egui::Ui,
    entry: GalleryItem,
    card_width: f32,
    card_height: f32,
    thumbnail_height: f32,
    thumbnails: &mut ThumbnailTextures,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    let frame = egui::Frame::new()
        .fill(palette.bg_surface)
        .inner_margin(egui::Margin::same(GALLERY_CARD_PADDING as i8))
        .corner_radius(8.0)
        .stroke(egui::Stroke::new(GALLERY_CARD_STROKE, palette.border));

    let inner = frame.show(ui, |ui| {
        // Size the frame's content so the card's *outer* rect is exactly
        // `card_width` wide — the frame allocates content + chrome around it.
        let content_width = (card_width - GALLERY_CARD_CHROME).max(40.0);
        ui.set_min_size(egui::vec2(
            content_width,
            (card_height - GALLERY_CARD_CHROME).max(0.0),
        ));
        ui.set_max_width(content_width);
        ui.with_layout(
            egui::Layout::top_down(egui::Align::Center).with_cross_align(egui::Align::Center),
            |ui| {
                let thumbnail_rect = ui
                    .allocate_exact_size(
                        egui::vec2(content_width, thumbnail_height),
                        egui::Sense::hover(),
                    )
                    .0;
                ui.painter()
                    .rect_filled(thumbnail_rect, 6.0, palette.bg_dark);
                if ui.is_rect_visible(thumbnail_rect)
                    && let Some(texture_id) =
                        thumbnails.get_or_request(ThumbnailKey::Gallery(entry))
                {
                    paint_texture_thumbnail(
                        ui.painter(),
                        texture_id,
                        thumbnail_rect.shrink2(egui::vec2(8.0, 8.0)),
                        1.0,
                    );
                }

                ui.add_space(6.0);
                // Centered, *non-justified* label: `add_sized` would wrap the
                // widget in a justified layout, stretching wrapped lines to the
                // card width and smearing the letters apart.
                ui.allocate_ui_with_layout(
                    egui::vec2(content_width, GALLERY_CARD_LABEL_HEIGHT),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(entry.to_string())
                                    .small()
                                    .color(palette.text_bright),
                            )
                            .wrap()
                            .sense(egui::Sense::hover()),
                        );
                    },
                );
            },
        );
    });

    let response = inner
        .response
        .interact(egui::Sense::click())
        .on_hover_text(format!(
            "{}\nClick: new tab · Ctrl+click: insert",
            entry.description()
        ));

    if activated(&response) {
        if response.ctx.input(|input| input.modifiers.ctrl) {
            intents.push(UiIntent::InsertGraph(Box::new(entry.build())));
        } else {
            intents.push(UiIntent::NewTab);
            intents.push(UiIntent::LoadGallery(entry));
        }
    }

    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Open {}", entry))
    });
    if response.hovered() || response.has_focus() {
        let rect = response.rect.expand2(egui::vec2(1.0, 1.0));
        ui.painter().rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(1.0, palette.accent_secondary),
            egui::StrokeKind::Outside,
        );
    }
}

/// Entries of `category` kept by the fuzzy `query`, best match first.
///
/// The gallery is a couple of dozen entries, so re-scoring every frame is
/// cheaper than caching and invalidating the filtered list.
fn matching_gallery_entries(category: Option<GalleryCategory>, query: &str) -> Vec<GalleryItem> {
    let entries = GALLERY_ENTRIES.entries(category);
    let query = query.trim();
    if query.is_empty() {
        return entries.to_vec();
    }

    // An id hit always outranks a description hit, however loose: typing "cz"
    // should surface a `cz_*` entry above anything merely *describing* a CZ.
    let mut scored: Vec<_> = entries
        .iter()
        .filter_map(|entry| {
            let by_id = fuzzy_score(entry.id(), query);
            let by_description = fuzzy_score(entry.description(), query).map(|score| score - 1000);
            by_id.max(by_description).map(|score| (score, *entry))
        })
        .collect();
    scored.sort_by(|(a, left), (b, right)| b.cmp(a).then_with(|| left.id().cmp(right.id())));
    scored.into_iter().map(|(_, entry)| entry).collect()
}

fn sorted_gallery_entries(category: Option<GalleryCategory>) -> Vec<GalleryItem> {
    let mut entries = match category {
        None => GalleryItem::iter().collect::<Vec<_>>(),
        Some(category) => GalleryItem::iter_by_category(category).collect::<Vec<_>>(),
    };
    entries.sort_by_key(|entry| entry.id());
    entries
}

fn gallery_tabs() -> [Option<GalleryCategory>; 8] {
    [
        None,
        Some(GalleryCategory::Clifford),
        Some(GalleryCategory::NonClifford),
        Some(GalleryCategory::Factory),
        Some(GalleryCategory::ExternalResource),
        Some(GalleryCategory::Adaptive),
        Some(GalleryCategory::Arithmetic),
        Some(GalleryCategory::Addition),
    ]
}

#[cfg(test)]
mod tests {
    use super::{fuzzy_score, matching_gallery_entries};

    #[test]
    fn fuzzy_search_ranks_and_filters_gallery_entries() {
        assert!(fuzzy_score("cnot", "cn").unwrap() > fuzzy_score("cnot", "ct").unwrap());
        assert!(fuzzy_score("t_gate", "gate").unwrap() < fuzzy_score("gate", "gate").unwrap());
        assert_eq!(fuzzy_score("cnot", "xyz"), None);

        let hits = matching_gallery_entries(None, "cz");
        assert_eq!(hits.first().map(|entry| entry.id()), Some("cz_spatial_h"));
        assert!(matching_gallery_entries(None, "zzzzzz").is_empty());
        assert_eq!(
            matching_gallery_entries(None, "  ").len(),
            matching_gallery_entries(None, "").len()
        );
    }
}
