//! Qubit-layout geometry and egui screen projection for the circuit viewer.

use std::collections::HashMap;

use bevy_egui::egui::{self, Pos2};
use glam::IVec2;

#[derive(Clone, Copy)]
pub(super) struct CanvasGeometry {
    pub(super) bounds: CoordBounds,
    pub(super) origin: Pos2,
    pub(super) pitch: f32,
}

#[derive(Clone, Copy)]
pub(super) struct CoordBounds {
    pub(super) min: IVec2,
    pub(super) max: IVec2,
}

pub(super) fn coord_bounds(qubit_coords: &HashMap<usize, IVec2>) -> Option<CoordBounds> {
    let mut coords = qubit_coords.values().copied();
    let first = coords.next()?;
    let mut min = first;
    let mut max = first;
    for coord in coords {
        min.x = min.x.min(coord.x);
        min.y = min.y.min(coord.y);
        max.x = max.x.max(coord.x);
        max.y = max.y.max(coord.y);
    }
    Some(CoordBounds { min, max })
}

pub(super) fn circuit_origin(
    rect: egui::Rect,
    bounds: CoordBounds,
    pitch: f32,
    pan_offset: glam::Vec2,
) -> Pos2 {
    let span = bounds.max - bounds.min;
    let span_vec = egui::vec2(span.x as f32 * pitch, span.y as f32 * pitch);
    let base = rect.center() - span_vec * 0.5;
    egui::pos2(base.x + pan_offset.x, base.y + pan_offset.y)
}

pub(super) fn fitted_pitch(rect: egui::Rect, bounds: CoordBounds) -> f32 {
    let span = bounds.max - bounds.min;
    let cols = (span.x.max(0) + 1) as f32;
    let rows = (span.y.max(0) + 1) as f32;
    let width_pitch = rect.width() / (cols + 1.0);
    let height_pitch = rect.height() / (rows + 1.0);
    width_pitch
        .min(height_pitch)
        .clamp(super::VIEWER_MIN_PITCH, 26.0)
}

pub(super) fn grid_pos(x: f32, y: f32, geometry: CanvasGeometry) -> Pos2 {
    let CanvasGeometry {
        bounds,
        origin,
        pitch,
    } = geometry;
    egui::pos2(
        origin.x + (x - bounds.min.x as f32) * pitch,
        origin.y + (bounds.max.y as f32 - y) * pitch,
    )
}

pub(super) fn qubit_pos(coord: IVec2, geometry: CanvasGeometry) -> Pos2 {
    grid_pos(coord.x as f32, coord.y as f32, geometry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_egui::egui::Rect;
    use glam::ivec2;

    #[test]
    fn fitted_pitch_shrinks_large_layouts_to_canvas() {
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(320.0, 180.0));
        let bounds = CoordBounds {
            min: ivec2(0, 0),
            max: ivec2(20, 10),
        };

        let pitch = fitted_pitch(rect, bounds);

        assert!(pitch < 26.0);
        assert!(pitch >= 4.0);
    }

    #[test]
    fn grid_positions_use_positive_y_upwards() {
        let bounds = CoordBounds {
            min: ivec2(0, 0),
            max: ivec2(2, 2),
        };
        let origin = egui::pos2(10.0, 20.0);
        let pitch = 8.0;

        let geometry = CanvasGeometry {
            bounds,
            origin,
            pitch,
        };
        let low_y = grid_pos(1.0, 0.0, geometry);
        let high_y = grid_pos(1.0, 2.0, geometry);

        assert_eq!(low_y.x, high_y.x);
        assert!(high_y.y < low_y.y);
        assert_eq!(high_y, origin + egui::vec2(8.0, 0.0));
    }
}
