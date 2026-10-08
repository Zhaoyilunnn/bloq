//! Bevy ECS components tagging the editor's 3D scene entities and the small
//! value types (graph elements, camera framing) attached to them.

use bevy::prelude::*;
use bloq_graph::BlockGraph;

/// Marks the single orbit camera that renders the editor viewport.
#[derive(Component)]
pub(crate) struct EditorCamera;

/// Stores a rendered entity's base material so highlight/selection overlays can
/// be reverted to it.
#[derive(Component)]
pub(crate) struct OriginalMaterial(pub(crate) Handle<StandardMaterial>);

/// A pickable piece of the block graph: a block at a grid cell, or a pipe
/// between two adjacent cells.
///
/// Pipe endpoints are unordered; use [`GraphElement::canonical`] before
/// comparing or hashing so the two orderings compare equal.
#[derive(
    Component, Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub(crate) enum GraphElement {
    Block(IVec3),
    Pipe(IVec3, IVec3),
}

impl GraphElement {
    /// Returns the element with pipe endpoints in a canonical order, so
    /// `Pipe(u, v)` and `Pipe(v, u)` normalize to the same value.
    pub(crate) fn canonical(self) -> Self {
        match self {
            Self::Block(pos) => Self::Block(pos),
            Self::Pipe(u, v) => {
                let key = PipeKey::new(u, v);
                Self::Pipe(key.a, key.b)
            }
        }
    }

    /// Iterates over every displayed block and pipe in `graph`.
    pub(crate) fn all_in(graph: &BlockGraph) -> impl Iterator<Item = Self> + '_ {
        graph.blocks().map(|block| Self::Block(block.pos())).chain(
            graph
                .pipe_endpoints_with_blocks()
                .map(|(u, v, _, _, _)| Self::Pipe(u, v).canonical()),
        )
    }
}

/// An order-independent key for a pipe, with endpoints sorted so either
/// direction produces the same key. Used to index pipes in maps and sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PipeKey {
    pub(crate) a: IVec3,
    pub(crate) b: IVec3,
}

impl PipeKey {
    /// Builds a key from two endpoints, sorting them so `new(u, v)` and
    /// `new(v, u)` are equal.
    pub(crate) fn new(u: IVec3, v: IVec3) -> Self {
        if (u.x, u.y, u.z) <= (v.x, v.y, v.z) {
            Self { a: u, b: v }
        } else {
            Self { a: v, b: u }
        }
    }
}

/// Marks a world-space axis bar drawn at the origin.
#[derive(Component, Clone, Copy)]
pub(crate) enum AxisHelper {
    X,
    Y,
    Z,
}

/// Marks one visual layer of the grid overlay drawn at the current editing plane.
#[derive(Component)]
pub(crate) struct CurrentPlaneGrid {
    pub(crate) part: CurrentPlaneGridPart,
}

/// The distinct layers of the current-plane grid, each drawn as its own mesh.
///
/// The height anchor stays fixed at the origin while the connectors and block
/// markers follow the camera focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CurrentPlaneGridPart {
    HeightAnchor,
    Connectors,
    BlockMarkers,
}

/// Marks the root entity that all rendered block/pipe meshes are parented to.
#[derive(Component)]
pub(crate) struct GraphRoot;

/// Marks the translucent mesh previewing where a block would be placed.
#[derive(Component)]
pub(crate) struct PreviewMesh;

/// Marks the mesh previewing a walking block's oblique path.
#[derive(Component)]
pub(crate) struct WalkingPreviewMesh;

/// A clickable preview marker for a candidate pipe/walking endpoint, carrying
/// the source and target grid cells it would connect.
#[derive(Component)]
pub(crate) struct PreviewEndpointMesh {
    pub(crate) endpoint: PreviewEndpoint,
    pub(crate) source_pos: Option<IVec3>,
    pub(crate) target_pos: Option<IVec3>,
}

/// Identifies which preview endpoint marker an entity represents.
///
/// `PipeHint`/`WalkingHint` carry the index of the candidate among the fixed
/// pool of hint slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewEndpoint {
    Start,
    End,
    PipeHint(usize),
    WalkingHint(usize),
}

/// Orbit-camera framing: a focus point plus spherical offset (`radius`,
/// azimuth `alpha`, polar `beta` in radians).
#[derive(Component, Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CameraSettings {
    pub(crate) focus: Vec3,
    pub(crate) radius: f32,
    pub(crate) alpha: f32,
    pub(crate) beta: f32,
}

impl Default for CameraSettings {
    fn default() -> Self {
        Self {
            focus: Vec3::ZERO,
            radius: 51.961525,
            alpha: std::f32::consts::FRAC_PI_4 * 3.0,
            beta: 0.9553166,
        }
    }
}
