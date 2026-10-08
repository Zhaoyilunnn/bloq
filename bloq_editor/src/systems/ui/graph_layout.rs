//! GUI adapter for the shared IR graph layout.
use bevy_egui::egui;
pub(crate) use bloq_ir::visualization::layout::{CubicSpline, RelativeLayout};

pub(crate) fn compute_relative(
    nodes: &[(u32, egui::Vec2)],
    edges: &[(u32, u32)],
    gap: f32,
) -> RelativeLayout {
    let sizes = nodes
        .iter()
        .map(|(id, size)| (*id, (size.x, size.y)))
        .collect::<Vec<_>>();
    bloq_ir::visualization::layout::compute_relative(&sizes, edges, gap)
}
