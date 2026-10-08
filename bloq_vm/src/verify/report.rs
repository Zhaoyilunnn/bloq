//! The report a verification batch produces, and the detector-constancy check
//! that grades it.
//!
//! # Observables are raw physical values (contract)
//!
//! An [`ObservableReport`] contains the XOR of its bit-carrying producers and
//! any live `Output`-face boundary Pauli, before terminal frame correction. For
//! a feedforward program (e.g. a T block), this raw value can vary by shot. The
//! deterministic logical value is `raw ⊕ frame-sign`, where the frame sign is
//! the anticommuting bit of the output's [`FramePairReport`].

use glam::IVec3;

/// Per-detector result across the batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectorReport {
    /// Whether every applicable shot agreed (the pass condition; vacuously
    /// true when no shot reached this detector).
    pub constant: bool,
    /// The shared value when at least one shot reached this detector and all
    /// applicable shots agreed; otherwise `None`.
    pub value: Option<bool>,
    /// The detector's value in each applicable, non-discarded shot, in shot
    /// order. Shots that did not reach its owning scope are omitted.
    pub per_shot: Vec<bool>,
}

/// Per-observable result across the batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservableReport {
    /// The observable's raw physical value in each applicable, non-discarded
    /// shot (see the module contract above; unevaluable observables are
    /// omitted, and feedforward byproducts are corrected with the relevant
    /// [`FramePairReport`] frame sign).
    pub per_shot: Vec<bool>,
}

/// Terminal Pauli-frame sign bits for one output, per shot.
///
/// The deterministic logical observable is the raw observable XOR the
/// anticommuting [`bloq_ir::FramePair`] bit (X-type observable ⊕ `z_bits`; Z-type ⊕
/// `x_bits`).
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramePairReport {
    /// The source output port this frame corrects.
    pub port: IVec3,
    /// X-frame sign per shot (`None` if unevaluable that shot).
    pub x_bits: Vec<Option<bool>>,
    /// Z-frame sign per shot (`None` if unevaluable that shot).
    pub z_bits: Vec<Option<bool>>,
}

/// The outcome of a verification batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// Number of shots run.
    pub shots: usize,
    /// Number of shots rejected by a `Discard` node (excluded from the detector
    /// and observable statistics).
    pub discarded: usize,
    /// One entry per input detector, in order.
    pub detectors: Vec<DetectorReport>,
    /// One entry per input observable, in order.
    pub observables: Vec<ObservableReport>,
    /// The largest stabilizer rank reached in any shot, sampled once per
    /// replayed instruction, so an `Op` carrying
    /// several `T` targets does not hide the intermediate ranks it passed
    /// through.
    pub max_rank: usize,
    /// Terminal Pauli-frame signs per output — the correction channel for raw
    /// observables.
    pub frame_pairs: Vec<FramePairReport>,
    /// Every stamped selector outcome across the run, in evaluation order.
    /// Lets a caller confirm feedforward exercised both arms across seeds.
    pub branch_selectors: Vec<bool>,
}

impl VerifyReport {
    /// Whether every detector was constant across shots (the pass condition).
    #[must_use]
    pub fn all_detectors_constant(&self) -> bool {
        self.detectors.iter().all(|d| d.constant)
    }
}

/// Collapse a detector's applicable-shot values into a [`DetectorReport`]. An
/// unreached detector is vacuously constant but has no shared value.
pub(crate) fn detector_report(per_shot: Vec<bool>) -> DetectorReport {
    let constant = per_shot.windows(2).all(|w| w[0] == w[1]);
    DetectorReport {
        value: per_shot.first().copied().filter(|_| constant),
        constant,
        per_shot,
    }
}
