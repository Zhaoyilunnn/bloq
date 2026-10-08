//! Painting pre-rendered block, pipe, and gallery thumbnail textures.

use bevy_egui::egui::{self, Color32, Rect};

/// Paints a pre-rendered texture thumbnail into `rect`, letterboxed to preserve
/// its aspect ratio.
pub(crate) fn paint_texture_thumbnail(
    painter: &egui::Painter,
    texture_id: egui::TextureId,
    rect: Rect,
    texture_aspect_ratio: f32,
) {
    let fitted = fit_rect_with_aspect(rect, texture_aspect_ratio);
    painter.image(
        texture_id,
        fitted,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

fn fit_rect_with_aspect(rect: Rect, aspect_ratio: f32) -> Rect {
    let aspect_ratio = aspect_ratio.max(0.01);
    let container_aspect = (rect.width() / rect.height()).max(0.01);

    let size = if container_aspect > aspect_ratio {
        egui::vec2(rect.height() * aspect_ratio, rect.height())
    } else {
        egui::vec2(rect.width(), rect.width() / aspect_ratio)
    };

    Rect::from_center_size(rect.center(), size)
}
