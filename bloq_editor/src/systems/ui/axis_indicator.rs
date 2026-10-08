//! The corner gizmo showing the current world-axis orientation.

use crate::components::EditorCamera;
use crate::resources::{CentralViewport, EditorState};
use crate::theme::{ThemePalette, palette};
use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};

/// Draws the X/Y/Z orientation gizmo in a corner of the viewport (hidden in
/// Bloq mode).
pub(crate) fn draw_axis_indicator_system(
    mut contexts: EguiContexts,
    editor_state: Res<EditorState>,
    central: Res<CentralViewport>,
    camera: Single<&Transform, With<EditorCamera>>,
) {
    if editor_state.mode.covers_viewport() {
        return;
    }

    let Ok(ctx) = contexts.ctx_mut() else { return };
    let palette = palette(editor_state.theme_preset);
    let viewport_rect = central.0;
    if viewport_rect.width() < 160.0 || viewport_rect.height() < 120.0 {
        return;
    }

    let painter = ctx.layer_painter(egui::LayerId::new(
        // Viewport decoration stays behind notification cards and viewers.
        egui::Order::Background,
        egui::Id::new("axis_indicator_overlay"),
    ));
    draw_axis_indicator(
        &painter,
        viewport_rect,
        editor_state.plane_height,
        palette,
        camera.rotation,
    );
}

fn draw_axis_indicator(
    painter: &egui::Painter,
    viewport_rect: egui::Rect,
    plane_height: i32,
    palette: &ThemePalette,
    camera_rotation: Quat,
) {
    let origin = egui::pos2(viewport_rect.right() - 78.0, viewport_rect.bottom() - 42.0);
    let (x_offset, y_offset, z_offset) = axis_indicator_tip_offsets(camera_rotation);
    let x_tip = origin + x_offset;
    let y_tip = origin + y_offset;
    let z_tip = origin + z_offset;
    let (x_color, y_color, z_color, outline_color, origin_fill) = axis_indicator_colors(palette);

    draw_axis_arrow(painter, origin, y_tip, y_color, outline_color);
    draw_axis_arrow(painter, origin, x_tip, x_color, outline_color);
    draw_axis_arrow(painter, origin, z_tip, z_color, outline_color);
    painter.circle_filled(origin, 3.5, origin_fill);
    painter.circle_stroke(origin, 3.5, egui::Stroke::new(1.0, outline_color));

    let small_font = egui::FontId::monospace(11.0);
    draw_outlined_text(
        painter,
        x_tip + x_offset.normalized() * 8.0,
        "x",
        small_font.clone(),
        x_color,
        outline_color,
    );
    draw_outlined_text(
        painter,
        y_tip + y_offset.normalized() * 8.0,
        "y",
        small_font,
        y_color,
        outline_color,
    );
    draw_plane_height_label(
        painter,
        z_tip + z_offset.normalized() * 15.0,
        &format!("z={plane_height}"),
        z_color,
        palette,
    );
}

fn axis_indicator_tip_offsets(camera_rotation: Quat) -> (egui::Vec2, egui::Vec2, egui::Vec2) {
    let world_to_screen = |axis: Vec3| {
        let view_axis = camera_rotation.inverse() * axis;
        egui::vec2(view_axis.x, -view_axis.y) * 44.0
    };

    (
        world_to_screen(Vec3::X),
        world_to_screen(Vec3::NEG_Z),
        world_to_screen(Vec3::Y),
    )
}

fn draw_axis_arrow(
    painter: &egui::Painter,
    origin: egui::Pos2,
    tip: egui::Pos2,
    color: egui::Color32,
    outline: egui::Color32,
) {
    let delta = tip - origin;
    painter.arrow(origin, delta, egui::Stroke::new(5.0, outline));
    painter.arrow(origin, delta, egui::Stroke::new(2.5, color));
}

fn draw_outlined_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    font_id: egui::FontId,
    color: egui::Color32,
    outline: egui::Color32,
) {
    for offset in [
        egui::vec2(-1.0, 0.0),
        egui::vec2(1.0, 0.0),
        egui::vec2(0.0, -1.0),
        egui::vec2(0.0, 1.0),
    ] {
        painter.text(
            pos + offset,
            egui::Align2::CENTER_CENTER,
            text,
            font_id.clone(),
            outline,
        );
    }
    painter.text(pos, egui::Align2::CENTER_CENTER, text, font_id, color);
}

fn draw_plane_height_label(
    painter: &egui::Painter,
    center: egui::Pos2,
    text: &str,
    z_color: egui::Color32,
    palette: &ThemePalette,
) {
    let width = text.chars().count() as f32 * 7.2 + 18.0;
    let height = 20.0;
    let rect = egui::Rect::from_center_size(center, egui::vec2(width, height));
    let fill = if palette.dark_mode {
        egui::Color32::from_rgba_unmultiplied(24, 26, 27, 230)
    } else {
        egui::Color32::from_rgba_unmultiplied(246, 250, 255, 236)
    };
    let text_color = if palette.dark_mode {
        egui::Color32::from_rgb(174, 211, 255)
    } else {
        egui::Color32::from_rgb(18, 72, 176)
    };

    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.25, z_color),
        egui::StrokeKind::Outside,
    );
    painter.line_segment(
        [
            egui::pos2(rect.center().x, rect.bottom()),
            egui::pos2(rect.center().x, rect.bottom() + 7.0),
        ],
        egui::Stroke::new(1.25, z_color),
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::monospace(12.0),
        text_color,
    );
}

fn axis_indicator_colors(
    palette: &ThemePalette,
) -> (
    egui::Color32,
    egui::Color32,
    egui::Color32,
    egui::Color32,
    egui::Color32,
) {
    if palette.dark_mode {
        (
            egui::Color32::from_rgb(234, 105, 98),
            egui::Color32::from_rgb(169, 182, 101),
            egui::Color32::from_rgb(125, 174, 248),
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 210),
            egui::Color32::from_rgba_unmultiplied(245, 245, 238, 230),
        )
    } else {
        (
            egui::Color32::from_rgb(178, 44, 48),
            egui::Color32::from_rgb(28, 122, 72),
            egui::Color32::from_rgb(25, 90, 218),
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 230),
            egui::Color32::from_rgba_unmultiplied(24, 32, 48, 210),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::axis_indicator_tip_offsets;
    use bevy::prelude::*;
    use bevy_egui::egui;

    #[test]
    fn camera_rotation_rotates_projected_graph_axes() {
        let (x_before, _, z_before) = axis_indicator_tip_offsets(Quat::IDENTITY);
        let (x_after, _, z_after) =
            axis_indicator_tip_offsets(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));

        assert!((x_before - egui::vec2(44.0, 0.0)).length() < 1e-4);
        assert!((z_before - egui::vec2(0.0, -44.0)).length() < 1e-4);
        assert!((x_after - egui::vec2(0.0, 44.0)).length() < 1e-4);
        assert!((z_after - egui::vec2(44.0, 0.0)).length() < 1e-4);
    }
}
