//! Coordinate-based quantum circuit representation and manipulation.
//!
//! Provides coordinate-based circuits ([`CoordCircuit`]), stabilizer flow
//! verification ([`FlowEngine`]), chunk-level circuit composition ([`Chunk`]),
//! and Pauli map / detector parity types used throughout the bloq
//! compilation pipeline.

mod chunk;
mod circuit;
mod detslice;
mod error;
mod flow_engine;
mod gate;
mod noise;
mod pauli_map;
mod semantic;

#[doc(no_inline)]
pub use bloq_utils::{Basis, Pauli, PauliBasis};
pub use chunk::{Chunk, ChunkOrLoop, Flow, FlowMarker, FlowMeasurements};
pub use circuit::{
    CoordCircuit, ExpandedMeasurementColumns, FlattenEvent, FlattenLimits, measurement_gate_name,
};
pub use detslice::{
    DetectorRegion, DetectorSlices, DetsliceError, RegionBreak, RegionBreakKind, RegionTerm,
    SliceDetector, SliceRegionSeed, detector_slices_with_seeds,
};
pub use error::{CircuitError, CoordinateOverflowError, FlowError, MeasurementFrameError};
pub use flow_engine::{
    CompletedFlow, ComposedChain, FlowEngine, FlowEngineMeasurements, FlowKey, OffsetFlows,
    PendingLoopDetectorStates, ResidualFlow,
};
pub use gate::GateType;
pub use noise::NoiseModel;
pub use pauli_map::{PauliMap, PauliMapIter};
pub use semantic::{
    BodyId, CircuitBody, ConditionalCorrection, DetectorCoords, DetectorParity, DetectorTerm,
    LoopCarriedDetectorState, LoopStateId, MeasRecord, MeasRegistry, Op,
    checked_translate_coordinate, translate_detector_coords,
};

/// The most commonly used items, for `use bloq_circuit::prelude::*`.
///
/// Also available as `bloq::circuit::prelude` through the facade crate.
pub mod prelude {
    #[doc(no_inline)]
    pub use crate::{Chunk, CoordCircuit, FlowEngine, GateType, NoiseModel, PauliMap};
}
