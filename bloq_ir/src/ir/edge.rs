use std::cmp::Ordering;

use glam::IVec3;

use super::{BloqNodeId, PipePadding};

/// A reference to one source block by its lattice position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SourceBlockRef {
    /// Source lattice position.
    pub pos: IVec3,
}

impl PartialOrd for SourceBlockRef {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SourceBlockRef {
    fn cmp(&self, other: &Self) -> Ordering {
        self.pos.to_array().cmp(&other.pos.to_array())
    }
}

/// A reference to one source temporal pipe.
///
/// Its endpoint lattice positions identify the pipe, with `hadamard` marking
/// a basis-swapping (H) pipe. A spatial output Port's virtual
/// cube-to-temporal-port seam uses its shared source address for both endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TemporalPipeRef {
    /// Source endpoint.
    pub src: IVec3,
    /// Destination endpoint.
    pub dst: IVec3,
    /// Whether the pipe swaps X and Z bases.
    pub hadamard: bool,
}

impl TemporalPipeRef {
    /// The pipe's endpoints ordered by time layer: `(lower, upper)`.
    #[must_use]
    pub fn endpoints_by_z(&self) -> (IVec3, IVec3) {
        if self.src.z <= self.dst.z {
            (self.src, self.dst)
        } else {
            (self.dst, self.src)
        }
    }
}

/// A quantum face seam: its temporal pipes, each carrying the memory-padding
/// templates that may subdivide it.
///
/// Keeping the provenance on the edge distinguishes the two seams on either
/// side of a materialized temporal-Hadamard node and lets an outgoing region
/// boundary describe its body's terminal face.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct QuantumEdge {
    /// Temporal pipes sharing this seam.
    pub pipes: Vec<PipeSeam>,
    /// A level-local classical bit producer selecting this seam. Its value must be
    /// available before the target runs. `None` is an unconditional seam.
    pub guard: Option<ValueRef>,
}

/// One temporal pipe on a quantum seam, with the memory-padding templates the
/// compiler recorded for it.
///
/// `padding` is `None` on a hand-built or synthetic edge, and on a pipe whose
/// patch the padding pass could not resolve. Pairing each pipe with its own
/// provenance is what makes a count or order mismatch between the two
/// unrepresentable. [`crate::Bloq::insert_memory_rounds`] subdivides a seam
/// whole, so it needs *every* member padded and rejects a partially padded
/// edge with [`crate::EditError::EdgePaddingMissing`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PipeSeam {
    /// Source temporal pipe.
    pub pipe: TemporalPipeRef,
    /// Optional memory-padding templates.
    pub padding: Option<PipePadding>,
}

/// Boolean output selected from an observable.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum ObservableOutput {
    /// Corrected parity for indexed observables, assembled parity for fragments.
    #[default]
    Corrected,
    /// Decoder's predicted correction; only indexed observables expose it.
    Flip,
}

/// A level-local Boolean source and its selected output.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ValueRef {
    /// Producer node.
    pub node: BloqNodeId,
    /// Selected Boolean output.
    pub output: ObservableOutput,
}

impl From<BloqNodeId> for ValueRef {
    fn from(node: BloqNodeId) -> Self {
        Self {
            node,
            output: ObservableOutput::Corrected,
        }
    }
}

/// Quantum seams, Boolean dependencies, recipe composition, and sequencing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum BloqEdge {
    /// A quantum face seam: the connecting temporal pipe(s). Linear (consumed
    /// once). Flow / detector composition runs over these edges only.
    /// Its boxed payload keeps the common Value and Order edge slots compact;
    /// this storage detail is absent from both exchange formats.
    Quantum(Box<QuantumEdge>),
    /// A classical-bit dependency: the selected producer output
    /// feeds the consumer's `slot`-th input. Copyable (a producer may fan
    /// out freely); the slot is carried here so [`crate::ClassicalExpr::In`] is robust
    /// to edge reordering under edits. `role` is edge provenance — never
    /// consulted for value semantics (see [`ValueRole`]).
    Value {
        /// Consumer input slot.
        slot: u32,
        /// Provenance of the routed value.
        role: ValueRole,
        /// Source Boolean output.
        output: ObservableOutput,
    },
    /// Structural inclusion of an observable recipe or region boundary export.
    Compose {
        /// Consumer operand slot, sharing the value-slot namespace.
        slot: u32,
        /// Source provenance retained through composition.
        role: ValueRole,
    },
    /// Pure sequencing, no data: spatial occupancy between adjacent-z nodes that
    /// share physical qubits but are not logically piped (teardown then
    /// reinit), or read-after-measure. Carries no flows.
    Order,
}

impl BloqEdge {
    /// A quantum seam without compiler-recorded memory-padding provenance.
    pub fn quantum(pipes: Vec<TemporalPipeRef>) -> Self {
        Self::Quantum(Box::new(QuantumEdge {
            guard: None,
            pipes: pipes
                .into_iter()
                .map(|pipe| PipeSeam {
                    pipe,
                    padding: None,
                })
                .collect(),
        }))
    }

    /// A plain dataflow `Value` edge ([`ValueRole::Data`]) — the common case.
    pub fn value(slot: u32) -> Self {
        BloqEdge::Value {
            slot,
            role: ValueRole::Data,
            output: ObservableOutput::Corrected,
        }
    }

    /// Route the decoder flip into a Boolean operand.
    pub fn flip(slot: u32) -> Self {
        Self::Value {
            slot,
            role: ValueRole::Data,
            output: ObservableOutput::Flip,
        }
    }

    /// Include a child recipe in an observable.
    pub fn compose(slot: u32) -> Self {
        Self::Compose {
            slot,
            role: ValueRole::Data,
        }
    }

    /// The seam's temporal pipes with their padding provenance; empty for a
    /// classical, composition, or order edge.
    pub fn pipes(&self) -> &[PipeSeam] {
        match self {
            BloqEdge::Quantum(edge) => &edge.pipes,
            BloqEdge::Value { .. } | BloqEdge::Compose { .. } | BloqEdge::Order => &[],
        }
    }
}

/// Edge provenance on a [`BloqEdge::Value`] edge: what source construct routed
/// this bit here.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum ValueRole {
    /// Plain dataflow — no provenance beyond the edge itself.
    #[default]
    Data,
    /// This edge folds a `Feedback` action's condition bit into an affected
    /// `Observable` (feedback is Pauli-frame bookkeeping, never a
    /// physical operation). Dynamic DEM construction excludes this runtime
    /// fold from physical observable symptoms. `action` preserves source
    /// provenance across fan-out.
    FeedbackFold {
        /// Source feedback-action ordinal.
        action: u32,
    },
    /// Folds an already corrected readout into a composed named parity.
    /// Dynamic DEM construction excludes this known runtime bit from the
    /// physical symptoms of the new parity's decoder query.
    ReadoutFold,
}
