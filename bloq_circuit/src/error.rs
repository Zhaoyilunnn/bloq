use glam::IVec2;
use thiserror::Error;

use crate::{BodyId, GateType, LoopStateId};

/// A coordinate plus layout offset does not fit the signed 32-bit lattice.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("translating coordinate {coordinate:?} by {offset:?} overflows i32")]
pub struct CoordinateOverflowError {
    /// Coordinate before translation.
    pub coordinate: IVec2,
    /// Requested translation offset.
    pub offset: IVec2,
}

/// A failure while matching stabilizer flows across chunk boundaries.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FlowError {
    /// Translating a flow boundary overflowed the coordinate lattice.
    #[error("{0}")]
    CoordinateOverflow(#[from] CoordinateOverflowError),
    /// Flow boundaries could not be composed consistently.
    #[error("{0}")]
    Composition(String),
}

/// An overflow or out-of-range condition while accounting for the
/// measurements a repeated (`REPEAT`) circuit body contributes.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MeasurementFrameError {
    /// One operation contains more measurement records than the backend count can address.
    #[error("operation measurement count {measurements} exceeds the u32 record range")]
    MeasurementCountOutOfRange {
        /// Measurement records carried by one operation.
        measurements: usize,
    },
    /// One repeated block contributes too many measurements.
    #[error(
        "repeat frame with {measurements_per_iteration} measurements per iteration repeated {repetitions} times overflows the emitted measurement count"
    )]
    RepeatedMeasurementCountOverflow {
        /// Measurements emitted by one iteration.
        measurements_per_iteration: u32,
        /// Number of loop iterations.
        repetitions: u32,
    },
    /// Adding measurements to the emitted count overflowed.
    #[error(
        "emitted measurement count {emitted_count} plus repeated contribution {additional} overflows the emitted measurement count"
    )]
    EmittedMeasurementCountOverflow {
        /// Count before the addition.
        emitted_count: u32,
        /// Measurements being added.
        additional: u32,
    },
    /// A loop-body measurement offset lies outside one iteration.
    #[error(
        "repeat body measurement offset {offset} is outside measurements per iteration {measurements_per_iteration}"
    )]
    RepeatMeasurementOffsetOutOfRange {
        /// Invalid body-local offset.
        offset: u32,
        /// Measurements emitted by one iteration.
        measurements_per_iteration: u32,
    },
    /// Computing a loop-body measurement offset overflowed.
    #[error("repeat body measurement offset overflowed")]
    RepeatMeasurementOffsetOverflow,
    /// A Stim record lookback exceeds its encoded range.
    #[error("measurement record lookback {lookback} exceeds backend limit {max}")]
    RecordLookbackOutOfRange {
        /// Requested positive lookback distance.
        lookback: u32,
        /// Largest supported distance.
        max: u32,
    },
}

/// An error building, validating, or lowering a [`CoordCircuit`].
///
/// [`CoordCircuit`]: crate::CoordCircuit
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CircuitError {
    /// A measurement id is absent from the circuit registry.
    #[error("measurement id {0} is not allocated in this circuit")]
    InvalidMeasurementId(u32),
    /// A circuit body id is absent.
    #[error("invalid circuit body id {0:?}")]
    InvalidCircuitBody(BodyId),
    /// A circuit body index cannot be represented by a body id.
    #[error("circuit body index {index} exceeds the u32 body-id range")]
    BodyIdOutOfRange {
        /// Index of the body being addressed.
        index: usize,
    },
    /// A loop detector state id is absent.
    #[error("invalid loop detector state id {0:?}")]
    InvalidLoopDetectorState(LoopStateId),
    /// A coordinate is absent from the output layout.
    #[error("qubit at {0} not found in layout")]
    QubitNotFoundInLayout(IVec2),
    /// A qubit index is outside the output layout.
    #[error("qubit index {index} is outside a {num_qubits}-qubit layout")]
    QubitIndexOutOfRange {
        /// Invalid qubit index.
        index: usize,
        /// Layout size.
        num_qubits: usize,
    },
    /// Measurement-frame accounting failed.
    #[error("{0}")]
    MeasurementFrame(#[from] MeasurementFrameError),
    /// A paired gate received an odd number of targets.
    #[error("{gate} requires paired targets, got {targets}")]
    InvalidGateTargetCount {
        /// Gate being applied.
        gate: GateType,
        /// Supplied target count.
        targets: usize,
    },
    /// An MPP operation contains no non-identity target.
    #[error("MPP product must have at least one non-identity Pauli target")]
    EmptyPauliProduct,
    /// Repeat expansion exceeds the measurement-id space.
    #[error("flattening repeat blocks would overflow the u32 measurement id space")]
    FlattenMeasurementIdOverflow,
    /// The expanded operation stream, targets, or replay events exceed the
    /// caller's work allowance. `observed` saturates if the estimate overflows.
    #[error("flattening requires at least {observed} work units (limit {limit})")]
    FlattenResourceLimit {
        /// Required or observed work.
        observed: usize,
        /// Configured work limit.
        limit: usize,
    },
}
