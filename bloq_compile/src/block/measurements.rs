use bloq_circuit::{Chunk, ChunkOrLoop, CoordCircuit, Op};
use glam::IVec2;

use crate::FxMap;

/// Qubit → measurement id for one chunk's circuit.
///
/// The single lookup abstraction shared by every block constructor. A qubit
/// measured more than once in the same chunk is a construction error: earlier
/// per-module helpers silently disagreed on it (first- vs last-measurement
/// wins), so duplicates are rejected eagerly instead of hiding the drift.
#[derive(Debug, Default, Clone)]
pub(crate) struct MeasurementIndex(FxMap<IVec2, u32>);

impl MeasurementIndex {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self(FxMap::with_capacity_and_hasher(
            capacity,
            Default::default(),
        ))
    }

    /// Recover the index by walking the entry body's measurement ops.
    pub(crate) fn from_circuit(circuit: &CoordCircuit) -> Self {
        let body = circuit
            .body(circuit.entry_body())
            .expect("entry body exists while indexing measurements");
        let mut index = Self::with_capacity(circuit.num_measurements() as usize);
        for op in body.ops() {
            let Op::Measure {
                qubits,
                measurements,
                ..
            } = op
            else {
                continue;
            };
            for (&qubit, &measurement) in qubits.iter().zip(measurements) {
                index.record(qubit, measurement);
            }
        }
        index
    }

    /// Record a measurement at emission time.
    pub(crate) fn record(&mut self, qubit: IVec2, measurement: u32) {
        let previous = self.0.insert(qubit, measurement);
        debug_assert!(
            previous.is_none(),
            "qubit {qubit} measured more than once in one chunk"
        );
    }

    pub(crate) fn reserve(&mut self, additional: usize) {
        self.0.reserve(additional);
    }

    pub(crate) fn get(&self, qubit: IVec2) -> Option<u32> {
        self.0.get(&qubit).copied()
    }

    /// The measurement id of a qubit the circuit is known to measure.
    ///
    /// # Panics
    ///
    /// Panics when the circuit does not measure `qubit`. Named `expect_*` (rustc
    /// convention) so call sites do not read like `Option::expect`.
    pub(crate) fn expect_measurement(&self, qubit: IVec2) -> u32 {
        self.get(qubit)
            .expect("generated circuit measures the requested qubit")
    }
}

/// Iterate the stage-indexed chunk sequence (loop bodies flattened) that
/// [`stage_chunk`] resolves one element of. Callers visiting every stage
/// should use this instead of resolving each index from the front.
pub(super) fn stage_chunks(chunks: &[ChunkOrLoop]) -> impl Iterator<Item = &Chunk> {
    chunks.iter().flat_map(|chunk_or_loop| match chunk_or_loop {
        ChunkOrLoop::Single(chunk) => std::slice::from_ref(&**chunk).iter(),
        ChunkOrLoop::Loop { body, .. } => body.iter(),
    })
}

pub(super) fn stage_chunk(chunks: &[ChunkOrLoop], chunk_index: usize) -> &Chunk {
    stage_chunks(chunks)
        .nth(chunk_index)
        .unwrap_or_else(|| panic!("generated chunks do not contain stage index {chunk_index}"))
}
