//! The editor's Bevy systems, grouped by concern: camera control, pointer/
//! keyboard input, background jobs, screenshotting, scene setup, thumbnail
//! rendering, the egui UI, and 3D visual synchronization.
//!
//! Each concern owns a feature `Plugin` that registers its own systems and
//! resources; `plugins::BloqEditorPlugin` is the thin aggregator that adds them.

use bevy::prelude::*;

pub(crate) mod camera;
pub(crate) mod input;
pub(crate) mod jobs;
pub(crate) mod screenshot;
pub(crate) mod setup;
pub(crate) mod thumbnails;
pub(crate) mod ui;
pub(crate) mod visuals;

/// Coarse ordering of the per-frame `Update` work, shared by every feature
/// plugin so their systems land in the right phase regardless of which plugin
/// registers them. `configure_sets` in `BloqEditorPlugin` pins the phase order.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum EditorUpdateSet {
    Jobs,
    Rendering,
    Interaction,
}
