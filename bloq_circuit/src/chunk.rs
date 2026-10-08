use glam::IVec2;
use smallvec::SmallVec;

use crate::{CoordCircuit, PauliMap};

const FLOW_MEASUREMENTS_INLINE_CAP: usize = 4;

/// The measurement ids a [`Flow`] carries, kept inline for the common small
/// case.
pub type FlowMeasurements = SmallVec<[u32; FLOW_MEASUREMENTS_INLINE_CAP]>;

/// How a completed flow chain's parity is consumed downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum FlowMarker {
    /// The completed chain emits a detector.
    #[default]
    Detector,
    /// The chain still participates in matching/fusion (so an incoming
    /// stabilizer is consumed and nothing dangles) but emits no detector. A
    /// transversal Pauli measurement uses this to suppress the opposite-basis
    /// stabilizer parities it randomizes.
    Discard,
    /// The chain emits a **restart parity** (a `RepeatUntilSuccess`
    /// post-selection syndrome) instead of a detector: odd parity restarts the
    /// enclosing attempt rather than feeding the decoder.
    Restart,
}

impl FlowMarker {
    /// Combine a flow's marker with the marker of the chain it fuses into.
    /// [`Detector`](FlowMarker::Detector) is the identity. Returns `None` when a
    /// [`Discard`](FlowMarker::Discard) component meets a
    /// [`Restart`](FlowMarker::Restart) one — a topology-dependent conflict
    /// (not any one builder's invariant) that the caller rejects.
    pub(crate) fn fuse(self, other: FlowMarker) -> Option<FlowMarker> {
        match (self, other) {
            (marker, FlowMarker::Detector) | (FlowMarker::Detector, marker) => Some(marker),
            (a, b) if a == b => Some(a),
            _ => None,
        }
    }
}

/// A stabilizer flow: a Pauli boundary condition `start` propagated through a
/// chunk to `end`, together with the measurements it accumulates.
///
/// An empty `start` marks a flow created inside the chunk (nothing enters);
/// an empty `end` marks one that terminates in the chunk (nothing leaves).
/// [`FlowEngine`](crate::FlowEngine) fuses flows whose boundaries match across
/// chunks into detector chains.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Flow {
    /// The Pauli boundary entering the chunk; empty if the flow originates
    /// fully from here.
    pub start: PauliMap,
    /// The Pauli boundary leaving the chunk; empty if the flow terminates here.
    pub end: PauliMap,
    /// Measurement ids the flow accumulates as it crosses the chunk.
    pub measurements: FlowMeasurements,
    /// Whether the stabilizer relation has negative sign. Equivalently, this
    /// is the expected XOR of `measurements` when both boundaries have positive
    /// eigenvalue.
    pub sign: bool,
    /// Coordinate for the flow center, if any.
    pub center: Option<IVec2>,
    /// How the completed chain's parity is consumed. See [`FlowMarker`].
    pub marker: FlowMarker,
}

impl Flow {
    /// A flow from `start` to `end` with no measurements, no center, and a
    /// [`FlowMarker::Detector`] marker. Chain the `with_*` methods to set the
    /// rest, so adding a new field does not churn every construction site.
    pub fn new(start: PauliMap, end: PauliMap) -> Self {
        Self {
            start,
            end,
            measurements: FlowMeasurements::new(),
            sign: false,
            center: None,
            marker: FlowMarker::Detector,
        }
    }

    /// Replaces the carried measurement parity.
    pub fn with_measurements(mut self, measurements: impl IntoIterator<Item = u32>) -> Self {
        self.measurements = measurements.into_iter().collect();
        self
    }

    /// Sets whether the stabilizer relation is negative.
    pub fn with_sign(mut self, sign: bool) -> Self {
        self.sign = sign;
        self
    }

    /// Sets the optional flow center.
    pub fn with_center(mut self, center: impl Into<Option<IVec2>>) -> Self {
        self.center = center.into();
        self
    }

    /// Sets how downstream compilation consumes this flow.
    pub fn with_marker(mut self, marker: FlowMarker) -> Self {
        self.marker = marker;
        self
    }
}

impl std::fmt::Display for Flow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> {}", self.start, self.end)?;
        for m in &self.measurements {
            write!(f, "*meas[{m:?}]")?;
        }
        if self.sign {
            write!(f, " [negative]")?;
        }
        if let Some(c) = self.center {
            write!(f, ", center=({},{})", c.x, c.y)?;
        }
        match self.marker {
            FlowMarker::Detector => {}
            FlowMarker::Discard => write!(f, " [discard]")?,
            FlowMarker::Restart => write!(f, " [restart]")?,
        }
        Ok(())
    }
}

/// A circuit fragment paired with the stabilizer flows crossing its
/// boundaries — the unit the compiler composes into a full program.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// The chunk's operations in absolute coordinates.
    pub circuit: CoordCircuit,
    /// Flows entering, crossing, and leaving this chunk.
    pub flows: Vec<Flow>,
}

/// A chunk or a loop of chunks (for repeated syndrome extraction rounds).
#[derive(Debug, Clone)]
pub enum ChunkOrLoop {
    /// A single, non-repeated chunk.
    Single(Box<Chunk>),
    /// A body of chunks executed `repetitions` times.
    Loop {
        /// Ordered loop body.
        body: Vec<Chunk>,
        /// Iteration count.
        repetitions: u32,
    },
}
