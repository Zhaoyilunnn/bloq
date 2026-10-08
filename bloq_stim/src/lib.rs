//! Stim text backend for the `bloq` pipeline.
//!
//! Linearizes a compiled [`bloq_ir::Bloq`] program (or a lower-level
//! `bloq_circuit` coordinate circuit) into Stim's `.stim` circuit text.
//! The `verify` feature additionally exposes stim-based flow verification.

mod bloq;
mod dialect;
mod emit;
mod layout;
mod measurement_frame;
mod plan;
#[cfg(feature = "verify")]
mod stim_noise;
mod text_utils;
#[cfg(feature = "verify")]
mod verify;

pub use bloq::{
    BloqStimOptions, BloqStimSegments, InputTrust, IsolatedTAttemptArtifacts,
    IsolatedTAttemptManifest, IsolatedTFrameRecipe, IsolatedTFrontierSheet, StimSegment,
    emit_bloq_stim, emit_bloq_stim_segments, emit_bloq_stim_segments_pair,
    emit_bloq_stim_segments_with, emit_bloq_stim_with, emit_isolated_t_attempts,
};
pub use dialect::{HONEST_T_TAG, StimDialect, clifft_to_stim_text, stim_to_clifft_text};
pub use emit::StimEmissionError;
pub use plan::{PlanStim, PlanStimOptions, emit_plan_stim};
#[cfg(feature = "verify")]
pub use stim_noise::{StimNoiseError, emit_bloq_stim_with_stim_noise};
pub use text_utils::write_stim_tag;
#[cfg(feature = "verify")]
pub use verify::{StimFlowVerifier, StimVerifyError, emit_annotated_stim};

/// Default extension for Stim circuit text.
pub const STIM_FILE_EXTENSION: &str = "stim";

/// The most commonly used items, for `use bloq_stim::prelude::*`.
///
/// Aggregated into `bloq::prelude` by the `bloq` facade crate.
pub mod prelude {
    #[doc(no_inline)]
    pub use crate::{BloqStimOptions, STIM_FILE_EXTENSION, emit_bloq_stim, emit_bloq_stim_with};
}
