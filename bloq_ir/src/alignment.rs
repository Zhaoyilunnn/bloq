//! Deterministic raw-moment alignment shared by circuit emitters and viewers.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use bloq_circuit::Op;
use glam::IVec2;
use thiserror::Error;

use crate::{
    BloqEdge, BloqNodeId, CycleDetected, FxMap, FxSet, MomentKind, MomentSegment, NodeProvenance,
    QuantumTimeline, SubGraph,
};

/// One node's raw moments and whole-circuit qubit footprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MomentLane {
    /// Node that owns this lane.
    pub node: BloqNodeId,
    /// Node-local moment kinds, with `None` for idle slots.
    pub moments: Vec<Option<MomentKind>>,
    /// Physical qubits occupied by the lane.
    pub qubits: Vec<IVec2>,
}

/// Split a straight-line circuit into alignable kind buckets and idle gaps.
///
/// A tick closes the preceding interval; it does not open a trailing empty
/// interval. Mixed intervals become consecutive same-kind runs, while a leading
/// or between-ticks empty interval is retained as `None`. Measurement-controlled
/// corrections stay in source order inside the adjacent visible runs, matching
/// [`crate::moment_segments`]; they do not add visible moments. A correction-only
/// interval retains an idle slot and carries its corrections to the next run
/// (or the last run if trailing). Correction-only circuits have no such anchor
/// and are rejected.
///
/// # Errors
///
/// Returns [`MomentAlignmentError::UnsupportedCircuitOperation`] for an
/// operation that cannot be represented as an aligned moment.
pub fn aligned_moment_segments(
    ops: &[Op],
) -> Result<Vec<Option<MomentSegment>>, MomentAlignmentError> {
    let mut slots = Vec::new();
    let mut start = 0;
    let mut segmenter = crate::MomentSegmenter::default();

    let mut append_interval = |interval: &[Op],
                               slots: &mut Vec<Option<MomentSegment>>|
     -> Result<(), MomentAlignmentError> {
        for op in interval {
            let unsupported = match op {
                Op::Measure {
                    flip_probability, ..
                } if *flip_probability != 0.0 => Some("measurement flip probability"),
                Op::Gate { .. } | Op::Measure { .. } | Op::MPP { .. } | Op::ConditionalPauli(_) => {
                    None
                }
                Op::Depolarize1 { .. } => Some("DEPOLARIZE1"),
                Op::Depolarize2 { .. } => Some("DEPOLARIZE2"),
                Op::PauliError { .. } => Some("Pauli error"),
                Op::Repeat { .. } => Some("REPEAT"),
                Op::Tick => Some("TICK"),
            };
            if let Some(operation) = unsupported {
                return Err(MomentAlignmentError::UnsupportedCircuitOperation { operation });
            }
        }
        let segments = segmenter.extend(interval);
        if segments.is_empty() {
            slots.push(None);
        } else {
            slots.extend(segments.into_iter().map(Some));
        }
        Ok(())
    };

    for (index, op) in ops.iter().enumerate() {
        if matches!(op, Op::Tick) {
            append_interval(&ops[start..index], &mut slots)?;
            start = index + 1;
        }
    }
    if start < ops.len() {
        append_interval(&ops[start..], &mut slots)?;
    }
    if let Some(last) = slots.iter_mut().rev().find_map(Option::as_mut) {
        segmenter.finish(std::slice::from_mut(last));
    } else if ops
        .iter()
        .any(|op| matches!(op, Op::ConditionalPauli(corrections) if !corrections.is_empty()))
    {
        return Err(MomentAlignmentError::UnsupportedCircuitOperation {
            operation: "conditional Pauli without a visible operation",
        });
    }
    Ok(slots)
}

/// One node-local moment placed into an aligned slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlignedMomentRef {
    /// Node that owns the moment.
    pub node: BloqNodeId,
    /// Node-local moment index.
    pub moment: usize,
}

/// Concurrent compatible moments. `None` is a real idle slot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlignedSlot {
    /// Shared moment kind, or `None` for an idle slot.
    pub kind: Option<MomentKind>,
    /// Node-local moments placed in this slot.
    pub entries: Vec<AlignedMomentRef>,
}

/// Serial aligned slots projected onto one doubled-z display layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlignedLayer {
    /// Doubled-z display layer.
    pub layer: i64,
    /// Serial slots within the layer.
    pub slots: Vec<AlignedSlot>,
}

/// Why raw node moments could not be aligned against their graph level.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum MomentAlignmentError {
    /// The graph has no causal order.
    #[error("{0}")]
    Cycle(#[from] CycleDetected),
    /// A lane names no live node in this graph level.
    #[error("moment lane references missing node {node:?}")]
    UnknownNode {
        /// Missing node id.
        node: BloqNodeId,
    },
    /// Only quantum nodes own physical moments.
    #[error("moment lane references non-quantum node {node:?}")]
    NonQuantumNode {
        /// Non-quantum node id.
        node: BloqNodeId,
    },
    /// A node has exactly one raw moment lane.
    #[error("moment lanes contain duplicate node {node:?}")]
    DuplicateLane {
        /// Duplicated node id.
        node: BloqNodeId,
    },
    /// Hand-built timeline metadata violates its cumulative-layer contract.
    #[error("node {node:?} has invalid quantum timeline metadata")]
    InvalidTimeline {
        /// Node with invalid timeline metadata.
        node: BloqNodeId,
    },
    /// Timeline cuts count logical rounds, so the descriptor count must agree.
    #[error(
        "node {node:?} timeline ends at round {expected}, but its moments mark {actual} round ends"
    )]
    TimelineRoundCountMismatch {
        /// Affected node.
        node: BloqNodeId,
        /// Round count declared by the timeline.
        expected: u32,
        /// Round count found in the moments.
        actual: usize,
    },
    /// The circuit operation cannot be represented by aligned kind buckets.
    #[error("moment alignment does not support circuit operation {operation}")]
    UnsupportedCircuitOperation {
        /// Unsupported operation name.
        operation: &'static str,
    },
    /// Layer-major emission would start a dependent node before its source ends.
    #[error(
        "node {predecessor:?} ends on aligned layer {predecessor_layer}, after dependent node {dependent:?} starts on layer {dependent_layer}"
    )]
    LayerOrderConflict {
        /// Earlier causal node.
        predecessor: BloqNodeId,
        /// Dependent node scheduled too early.
        dependent: BloqNodeId,
        /// Final predecessor layer.
        predecessor_layer: i64,
        /// Initial dependent layer.
        dependent_layer: i64,
    },
    /// The pairwise dynamic-programming table is deliberately bounded.
    #[error("moment alignment pair is too large ({left_moments} by {right_moments} moments)")]
    AlignmentTooLarge {
        /// Left sequence length.
        left_moments: usize,
        /// Right sequence length.
        right_moments: usize,
    },
}

#[derive(PartialEq)]
enum Anchor {
    Start,
    End,
}

struct LaneSlice {
    node: BloqNodeId,
    moments: Arc<Vec<Option<MomentKind>>>,
    range: Range<usize>,
    qubits: Arc<FxSet<IVec2>>,
    anchor: Anchor,
    rank: usize,
}

impl LaneSlice {
    fn len(&self) -> usize {
        self.range.len()
    }

    fn slots(&self) -> Vec<AlignedSlot> {
        self.moments[self.range.clone()]
            .iter()
            .enumerate()
            .map(|(offset, moment)| AlignedSlot {
                kind: *moment,
                entries: vec![AlignedMomentRef {
                    node: self.node,
                    moment: self.range.start + offset,
                }],
            })
            .collect()
    }
}

/// Align raw node moments into deterministic concurrent layer batches.
///
/// Timeline-bearing nodes are cut after the cumulative logical-round ends in
/// [`QuantumTimeline::layer_round_ends`]. Nodes without a timeline stay whole
/// at [`crate::BloqNode::layer`], except a temporal-Hadamard seam (and seam
/// waits beside it), which is displayed on its lower/source block layer.
/// Within a layer, qubit overlap or causal reachability forces separate serial
/// batches; compatible lanes in one batch fold by decreasing length, using a
/// shortest-common-supersequence alignment for each pair of moment-kind
/// sequences.
///
/// # Errors
///
/// Returns [`MomentAlignmentError`] for invalid lanes, timelines, graph order,
/// unsupported operations, or an alignment that exceeds its work bound.
///
/// # Panics
///
/// Panics only if a validated timeline index cannot fit the signed layer type.
pub fn align_moment_lanes(
    graph: &SubGraph,
    lanes: Vec<MomentLane>,
) -> Result<Vec<AlignedLayer>, MomentAlignmentError> {
    let order = graph.deterministic_emit_order()?;
    let ranks: FxMap<_, _> = order
        .iter()
        .copied()
        .enumerate()
        .map(|(rank, node)| (node, rank))
        .collect();
    let mut seen = FxSet::default();
    let mut by_layer = BTreeMap::<i64, Vec<LaneSlice>>::new();
    let hadamard_source_layers: FxSet<_> = graph
        .nodes()
        .filter_map(|(_, node)| match node.provenance {
            NodeProvenance::TemporalPipe { pipe } if pipe.hadamard => {
                Some(pipe.src.z.min(pipe.dst.z))
            }
            _ => None,
        })
        .collect();

    for lane in lanes {
        if !seen.insert(lane.node) {
            return Err(MomentAlignmentError::DuplicateLane { node: lane.node });
        }
        let node = graph
            .node(lane.node)
            .ok_or(MomentAlignmentError::UnknownNode { node: lane.node })?;
        let quantum = node
            .try_quantum()
            .ok_or(MomentAlignmentError::NonQuantumNode { node: lane.node })?;
        let incoming = graph
            .incoming(lane.node)
            .any(|edge| matches!(edge.edge, BloqEdge::Quantum(_)));
        let outgoing = graph
            .outgoing(lane.node)
            .any(|edge| matches!(edge.edge, BloqEdge::Quantum(_)));
        let moments = Arc::new(lane.moments);
        let qubits = Arc::new(lane.qubits.into_iter().collect());
        let rank = ranks[&lane.node];
        let base_layer = match node.provenance {
            NodeProvenance::TemporalPipe { pipe } | NodeProvenance::MemoryPadding { pipe, .. }
                if hadamard_source_layers.contains(&pipe.src.z.min(pipe.dst.z)) =>
            {
                2 * i64::from(pipe.src.z.min(pipe.dst.z))
            }
            _ => node.layer(),
        };

        match &quantum.timeline {
            Some(timeline) => {
                if !crate::validation::valid_quantum_timeline(timeline, &node.provenance) {
                    return Err(MomentAlignmentError::InvalidTimeline { node: lane.node });
                }
                let ranges = timeline_ranges(lane.node, &moments, timeline)?;
                let count = ranges.len();
                for (index, range) in ranges.into_iter().enumerate() {
                    let anchor = if index == 0 {
                        if incoming { Anchor::Start } else { Anchor::End }
                    } else if index + 1 == count && (!incoming || outgoing) {
                        Anchor::End
                    } else {
                        Anchor::Start
                    };
                    let offset = i64::try_from(index)
                        .expect("validated timeline span fits the source z range");
                    let layer = base_layer + 2 * offset;
                    by_layer.entry(layer).or_default().push(LaneSlice {
                        node: lane.node,
                        moments: Arc::clone(&moments),
                        range,
                        qubits: Arc::clone(&qubits),
                        anchor,
                        rank,
                    });
                }
            }
            None => {
                let moment_count = moments.len();
                by_layer.entry(base_layer).or_default().push(LaneSlice {
                    node: lane.node,
                    moments,
                    range: 0..moment_count,
                    qubits,
                    anchor: if incoming { Anchor::Start } else { Anchor::End },
                    rank,
                });
            }
        }
    }

    let mut layer_bounds = FxMap::<_, (i64, i64)>::default();
    for (&layer, slices) in &by_layer {
        for slice in slices.iter().filter(|slice| !slice.range.is_empty()) {
            layer_bounds
                .entry(slice.node)
                .and_modify(|bounds| {
                    bounds.0 = bounds.0.min(layer);
                    bounds.1 = bounds.1.max(layer);
                })
                .or_insert((layer, layer));
        }
    }

    // Layer-major output is only valid when each source lane fully precedes
    // every dependent lane. Same-layer paths are serialized below.
    let mut scratch = graph.path_scratch();
    for (source_index, &source) in order.iter().enumerate() {
        let Some(&(_, source_layer)) = layer_bounds.get(&source) else {
            continue;
        };
        for &target in &order[source_index + 1..] {
            let Some(&(target_layer, _)) = layer_bounds.get(&target) else {
                continue;
            };
            if source_layer > target_layer
                && graph.has_path_with_scratch(source, target, &mut scratch)
            {
                return Err(MomentAlignmentError::LayerOrderConflict {
                    predecessor: source,
                    dependent: target,
                    predecessor_layer: source_layer,
                    dependent_layer: target_layer,
                });
            }
        }
    }

    by_layer
        .into_iter()
        .map(|(layer, mut slices)| {
            slices.sort_unstable_by_key(|slice| slice.rank);
            let slots = partition_batches(graph, slices, &mut scratch)
                .into_iter()
                .map(align_batch)
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect();
            Ok(AlignedLayer { layer, slots })
        })
        .collect()
}

fn timeline_ranges(
    node: BloqNodeId,
    moments: &[Option<MomentKind>],
    timeline: &QuantumTimeline,
) -> Result<Vec<Range<usize>>, MomentAlignmentError> {
    let ends = &timeline.layer_round_ends;
    let round_ends: Vec<_> = moments
        .iter()
        .enumerate()
        .filter_map(|(index, &kind)| (kind == Some(MomentKind::Measurement)).then_some(index + 1))
        .collect();
    let expected = *ends.last().expect("timeline was validated");
    if round_ends.len() != expected as usize {
        return Err(MomentAlignmentError::TimelineRoundCountMismatch {
            node,
            expected,
            actual: round_ends.len(),
        });
    }

    let mut start = 0;
    let mut ranges = Vec::with_capacity(ends.len());
    for &end in &ends[..ends.len() - 1] {
        let cut = if end == 0 {
            0
        } else {
            round_ends[end as usize - 1]
        };
        ranges.push(start..cut);
        start = cut;
    }
    ranges.push(start..moments.len());
    Ok(ranges)
}

fn partition_batches(
    graph: &SubGraph,
    slices: Vec<LaneSlice>,
    scratch: &mut crate::PathScratch,
) -> Vec<Vec<LaneSlice>> {
    // ponytail: O(lanes²) reachability probes suit small physical layers;
    // cache a transitive closure only if profiling shows large layers here.
    let mut batches: Vec<Vec<LaneSlice>> = Vec::new();
    for slice in slices {
        let minimum = batches
            .iter()
            .enumerate()
            .filter(|(_, batch)| {
                batch
                    .iter()
                    .any(|other| slices_conflict(graph, other, &slice, scratch))
            })
            .map(|(index, _)| index + 1)
            .max()
            .unwrap_or(0);
        if minimum == batches.len() {
            batches.push(Vec::new());
        }
        batches[minimum].push(slice);
    }
    batches
}

fn slices_conflict(
    graph: &SubGraph,
    left: &LaneSlice,
    right: &LaneSlice,
    scratch: &mut crate::PathScratch,
) -> bool {
    left.qubits.iter().any(|qubit| right.qubits.contains(qubit))
        || graph.has_path_with_scratch(left.node, right.node, scratch)
}

fn align_batch(mut slices: Vec<LaneSlice>) -> Result<Vec<AlignedSlot>, MomentAlignmentError> {
    slices.sort_unstable_by_key(|slice| (Reverse(slice.len()), slice.rank));
    let mut slices = slices.into_iter();
    let first = slices.next().expect("a batch has at least one lane");
    let mut slots = first.slots();
    for slice in slices {
        let mut lane = slice.slots();
        if slice.anchor == Anchor::End {
            slots.reverse();
            lane.reverse();
            slots = align_from_start(&slots, &lane)?;
            slots.reverse();
        } else {
            slots = align_from_start(&slots, &lane)?;
        }
    }
    for slot in &mut slots {
        slot.entries
            .sort_unstable_by_key(|entry| (entry.node, entry.moment));
    }
    Ok(slots)
}

fn align_from_start(
    left: &[AlignedSlot],
    right: &[AlignedSlot],
) -> Result<Vec<AlignedSlot>, MomentAlignmentError> {
    const MAX_ALIGNMENT_CELLS: usize = 4 * 1024 * 1024;
    let width = right
        .len()
        .checked_add(1)
        .ok_or(MomentAlignmentError::AlignmentTooLarge {
            left_moments: left.len(),
            right_moments: right.len(),
        })?;
    let cells = left
        .len()
        .checked_add(1)
        .and_then(|height| height.checked_mul(width))
        .filter(|&cells| cells <= MAX_ALIGNMENT_CELLS)
        .ok_or(MomentAlignmentError::AlignmentTooLarge {
            left_moments: left.len(),
            right_moments: right.len(),
        })?;
    // ponytail: pairwise O(n²) DP is bounded to 4M cells; use linear-memory
    // reconstruction only if real compiled lanes hit this ceiling.
    let mut costs = vec![0usize; cells];
    let at = |i: usize, j: usize| i * width + j;
    for i in (0..left.len()).rev() {
        costs[at(i, right.len())] = left.len() - i;
    }
    for j in (0..right.len()).rev() {
        costs[at(left.len(), j)] = right.len() - j;
    }
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            let mut best = costs[at(i + 1, j)].min(costs[at(i, j + 1)]) + 1;
            if compatible(left[i].kind, right[j].kind) {
                best = best.min(costs[at(i + 1, j + 1)] + 1);
            }
            costs[at(i, j)] = best;
        }
    }

    let mut out = Vec::with_capacity(costs[0]);
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        let best = costs[at(i, j)];
        if compatible(left[i].kind, right[j].kind) && costs[at(i + 1, j + 1)] + 1 == best {
            out.push(merge_slots(&left[i], &right[j]));
            i += 1;
            j += 1;
        } else if costs[at(i, j + 1)] <= costs[at(i + 1, j)] {
            // On an equal-cost choice, place the new lane first: Start lanes
            // take the earliest optimal match. End lanes run this in reverse.
            out.push(right[j].clone());
            j += 1;
        } else {
            out.push(left[i].clone());
            i += 1;
        }
    }
    out.extend_from_slice(&left[i..]);
    out.extend_from_slice(&right[j..]);
    Ok(out)
}

fn compatible(left: Option<MomentKind>, right: Option<MomentKind>) -> bool {
    left.is_none() || right.is_none() || left == right
}

fn merge_slots(left: &AlignedSlot, right: &AlignedSlot) -> AlignedSlot {
    let mut entries = left.entries.clone();
    entries.extend_from_slice(&right.entries);
    AlignedSlot {
        kind: left.kind.or(right.kind),
        entries,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use bloq_circuit::{GateType, PauliBasis};
    use glam::{ivec2, ivec3};

    use super::*;
    use crate::{BloqNode, ClassicalExpr, RegionNode, SourceBlockRef, TemporalPipeRef};

    fn add_node(graph: &mut SubGraph, x: i32, z: i32) -> BloqNodeId {
        graph.add_node(BloqNode::from_members(vec![SourceBlockRef {
            pos: ivec3(x, 0, z),
        }]))
    }

    fn temporal_pipe(x: i32, z: i32, hadamard: bool) -> TemporalPipeRef {
        TemporalPipeRef {
            src: ivec3(x, 0, z),
            dst: ivec3(x, 0, z + 1),
            hadamard,
        }
    }

    fn rounds(count: usize) -> Vec<Option<MomentKind>> {
        (0..count)
            .flat_map(|_| {
                [
                    Some(MomentKind::Reset),
                    Some(MomentKind::Interaction),
                    Some(MomentKind::Measurement),
                ]
            })
            .collect()
    }

    fn lane(node: BloqNodeId, moments: Vec<Option<MomentKind>>, x: i32) -> MomentLane {
        MomentLane {
            node,
            moments,
            qubits: vec![ivec2(x, 0)],
        }
    }

    fn positions(slots: &[AlignedSlot], node: BloqNodeId) -> Vec<usize> {
        slots
            .iter()
            .enumerate()
            .filter_map(|(slot, aligned)| {
                aligned
                    .entries
                    .iter()
                    .any(|entry| entry.node == node)
                    .then_some(slot)
            })
            .collect()
    }

    fn nodes(slot: &AlignedSlot) -> BTreeSet<BloqNodeId> {
        slot.entries.iter().map(|entry| entry.node).collect()
    }

    #[test]
    fn circuit_segments_split_kinds_and_treat_terminal_ticks_as_closures() {
        let reset = Op::Gate {
            gate: GateType::RZ,
            qubits: vec![ivec2(0, 0)],
        };
        let interaction = Op::Gate {
            gate: GateType::CX,
            qubits: vec![ivec2(0, 0), ivec2(1, 0)],
        };

        let segments =
            aligned_moment_segments(&[reset.clone(), interaction, Op::Tick, Op::Tick]).unwrap();

        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].as_ref().unwrap().kind, MomentKind::Reset);
        assert_eq!(segments[1].as_ref().unwrap().kind, MomentKind::Interaction);
        assert!(segments[2].is_none());
        assert!(aligned_moment_segments(&[]).unwrap().is_empty());
        assert!(aligned_moment_segments(&[Op::Tick]).unwrap()[0].is_none());
        assert_eq!(
            aligned_moment_segments(&[Op::Tick, reset]).unwrap()[1]
                .as_ref()
                .unwrap()
                .kind,
            MomentKind::Reset
        );

        let source_order = aligned_moment_segments(&[
            Op::Gate {
                gate: GateType::H,
                qubits: vec![ivec2(0, 0)],
            },
            Op::Gate {
                gate: GateType::RZ,
                qubits: vec![ivec2(0, 0)],
            },
        ])
        .unwrap();
        assert_eq!(source_order[0].as_ref().unwrap().kind, MomentKind::Rotation);
        assert_eq!(source_order[1].as_ref().unwrap().kind, MomentKind::Reset);

        assert_eq!(
            aligned_moment_segments(&[Op::Depolarize1 {
                probability: 0.01,
                qubits: vec![ivec2(0, 0)],
            }]),
            Err(MomentAlignmentError::UnsupportedCircuitOperation {
                operation: "DEPOLARIZE1",
            })
        );
        assert_eq!(
            aligned_moment_segments(&[Op::Measure {
                basis: PauliBasis::Z,
                qubits: vec![ivec2(0, 0)],
                measurements: vec![0],
                flip_probability: 0.01,
            }]),
            Err(MomentAlignmentError::UnsupportedCircuitOperation {
                operation: "measurement flip probability",
            })
        );
    }

    #[test]
    fn aligned_feedforward_keeps_control_ids_order_and_tracker_moments() {
        let measure = Op::Measure {
            basis: PauliBasis::Z,
            qubits: vec![ivec2(0, 0)],
            measurements: vec![7],
            flip_probability: 0.0,
        };
        let correction = Op::ConditionalPauli(vec![bloq_circuit::ConditionalCorrection {
            pauli: PauliBasis::X,
            control: 7,
            target: ivec2(1, 0),
        }]);
        let reset = Op::Gate {
            gate: GateType::RZ,
            qubits: vec![ivec2(1, 0)],
        };
        for ops in [
            vec![measure.clone(), correction.clone(), reset.clone()],
            vec![
                measure.clone(),
                Op::Tick,
                correction.clone(),
                Op::Tick,
                reset,
            ],
            vec![measure, Op::Tick, correction.clone(), Op::Tick],
        ] {
            let aligned = aligned_moment_segments(&ops).unwrap();
            let visible: Vec<_> = aligned.into_iter().flatten().collect();
            assert_eq!(visible, crate::moment_segments(&ops));
            assert_eq!(
                visible
                    .into_iter()
                    .flat_map(|segment| segment.ops)
                    .collect::<Vec<_>>(),
                ops.into_iter()
                    .filter(|op| !matches!(op, Op::Tick))
                    .collect::<Vec<_>>(),
            );
        }
        assert_eq!(
            aligned_moment_segments(&[correction]),
            Err(MomentAlignmentError::UnsupportedCircuitOperation {
                operation: "conditional Pauli without a visible operation",
            }),
        );
    }

    #[test]
    fn pairwise_alignment_refuses_an_oversized_dp_table() {
        let lane = vec![AlignedSlot::default(); 2048];

        assert_eq!(
            align_from_start(&lane, &lane),
            Err(MomentAlignmentError::AlignmentTooLarge {
                left_moments: 2048,
                right_moments: 2048,
            })
        );
    }

    #[test]
    fn compact_round_aligns_with_six_slot_round_and_keeps_idle_slots() {
        let mut graph = SubGraph::new();
        let compact = add_node(&mut graph, 0, 0);
        let extended = add_node(&mut graph, 1, 0);
        let compact_moments = std::iter::once(Some(MomentKind::Reset))
            .chain((0..4).map(|_| Some(MomentKind::Interaction)))
            .chain(std::iter::once(Some(MomentKind::Measurement)))
            .collect();
        let extended_moments = vec![
            Some(MomentKind::Reset),
            Some(MomentKind::Interaction),
            None,
            Some(MomentKind::Interaction),
            Some(MomentKind::Interaction),
            Some(MomentKind::Interaction),
            None,
            Some(MomentKind::Measurement),
        ];

        let aligned = align_moment_lanes(
            &graph,
            vec![
                lane(compact, compact_moments, 0),
                lane(extended, extended_moments, 1),
            ],
        )
        .unwrap();

        assert_eq!(aligned.len(), 1);
        let slots = &aligned[0].slots;
        assert_eq!(slots.len(), 8);
        assert_eq!(slots[0].kind, Some(MomentKind::Reset));
        assert_eq!(slots[7].kind, Some(MomentKind::Measurement));
        assert_eq!(positions(slots, compact).len(), 6);
        assert_eq!(positions(slots, extended), (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn shorter_lanes_follow_input_and_output_anchors() {
        let mut graph = SubGraph::new();
        let source = add_node(&mut graph, -1, -1);
        let long = add_node(&mut graph, 0, 0);
        let start = add_node(&mut graph, 1, 0);
        let end = add_node(&mut graph, 2, 0);
        let sink = add_node(&mut graph, 3, 1);
        graph.add_edge(source, start, BloqEdge::quantum(Vec::new()));
        graph.add_edge(end, sink, BloqEdge::quantum(Vec::new()));

        let aligned = align_moment_lanes(
            &graph,
            vec![
                lane(long, rounds(3), 0),
                lane(start, rounds(2), 1),
                lane(end, rounds(2), 2),
            ],
        )
        .unwrap();
        let slots = &aligned[0].slots;

        assert_eq!(slots.len(), 9);
        assert_eq!(positions(slots, start), (0..6).collect::<Vec<_>>());
        assert_eq!(positions(slots, end), (3..9).collect::<Vec<_>>());
    }

    #[test]
    fn timeline_cuts_lane_at_cumulative_round_ends() {
        let mut graph = SubGraph::new();
        let node = add_node(&mut graph, 0, 2);
        graph.node_mut(node).unwrap().expect_quantum_mut().timeline = Some(QuantumTimeline {
            layer_round_ends: vec![1, 3],
        });
        let moments = (0..3)
            .flat_map(|_| [Some(MomentKind::Reset), Some(MomentKind::Measurement)])
            .collect();

        let aligned = align_moment_lanes(&graph, vec![lane(node, moments, 0)]).unwrap();

        assert_eq!(
            aligned.iter().map(|layer| layer.layer).collect::<Vec<_>>(),
            vec![4, 6]
        );
        assert_eq!(positions(&aligned[0].slots, node), vec![0, 1]);
        assert_eq!(
            aligned[1]
                .slots
                .iter()
                .flat_map(|slot| &slot.entries)
                .map(|entry| entry.moment)
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 5]
        );
    }

    #[test]
    fn temporal_hadamard_seam_projects_into_its_source_layer() {
        let mut graph = SubGraph::new();
        let hadamard = graph.add_node(BloqNode::from_temporal_pipe(temporal_pipe(0, 0, true)));
        let wait = graph.add_node(BloqNode::memory_padding(temporal_pipe(1, 0, false), 1));
        let aligned = align_moment_lanes(
            &graph,
            vec![
                lane(hadamard, vec![Some(MomentKind::Reset)], 0),
                lane(wait, vec![Some(MomentKind::Reset)], 1),
            ],
        )
        .unwrap();

        assert_eq!(aligned.len(), 1);
        assert_eq!(aligned[0].layer, 0);
        assert_eq!(aligned[0].slots.len(), 1);
        assert_eq!(
            nodes(&aligned[0].slots[0]),
            BTreeSet::from([hadamard, wait])
        );
    }

    #[test]
    fn temporal_hadamard_follows_the_last_slice_of_a_tall_source() {
        let mut graph = SubGraph::new();
        let tall = add_node(&mut graph, 0, 0);
        graph.node_mut(tall).unwrap().expect_quantum_mut().timeline = Some(QuantumTimeline {
            layer_round_ends: vec![1, 2],
        });
        let pipe = temporal_pipe(0, 1, true);
        let hadamard = graph.add_node(BloqNode::from_temporal_pipe(pipe));
        graph.add_edge(tall, hadamard, BloqEdge::quantum(vec![pipe]));

        let aligned = align_moment_lanes(
            &graph,
            vec![
                lane(
                    tall,
                    vec![
                        Some(MomentKind::Reset),
                        Some(MomentKind::Measurement),
                        Some(MomentKind::Reset),
                        Some(MomentKind::Measurement),
                    ],
                    0,
                ),
                lane(hadamard, vec![Some(MomentKind::Rotation)], 0),
            ],
        )
        .unwrap();

        assert_eq!(
            aligned.iter().map(|layer| layer.layer).collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(positions(&aligned[1].slots, tall), vec![0, 1]);
        assert_eq!(positions(&aligned[1].slots, hadamard), vec![2]);
    }

    #[test]
    fn qubit_overlap_and_causal_reachability_serialize_lanes() {
        let mut graph = SubGraph::new();
        let overlap_a = add_node(&mut graph, 0, 0);
        let overlap_b = add_node(&mut graph, 1, 0);
        let causal_a = add_node(&mut graph, 2, 0);
        let causal_b = add_node(&mut graph, 3, 0);
        let barrier = graph.add_node(BloqNode::region(RegionNode::RepeatUntilSuccess {
            restart_source: None,
            restart_condition: ClassicalExpr::Const(false),
            body: SubGraph::new(),
        }));
        graph.add_edge(causal_a, barrier, BloqEdge::Order);
        graph.add_edge(barrier, causal_b, BloqEdge::Order);
        let one = || vec![Some(MomentKind::Reset)];

        let aligned = align_moment_lanes(
            &graph,
            vec![
                lane(overlap_a, one(), 0),
                lane(overlap_b, one(), 0),
                lane(causal_a, one(), 2),
                lane(causal_b, one(), 3),
            ],
        )
        .unwrap();

        let slots = &aligned[0].slots;
        assert_eq!(slots.len(), 2);
        assert_eq!(nodes(&slots[0]), BTreeSet::from([overlap_a, causal_a]));
        assert_eq!(nodes(&slots[1]), BTreeSet::from([overlap_b, causal_b]));
    }

    #[test]
    fn rejects_causality_that_descends_across_aligned_layers() {
        let mut graph = SubGraph::new();
        let source = add_node(&mut graph, 0, 1);
        let target = add_node(&mut graph, 1, 0);
        graph.add_edge(source, target, BloqEdge::quantum(Vec::new()));

        let error = align_moment_lanes(
            &graph,
            vec![
                lane(source, vec![Some(MomentKind::Reset)], 0),
                lane(target, vec![Some(MomentKind::Reset)], 1),
            ],
        )
        .unwrap_err();

        assert_eq!(
            error,
            MomentAlignmentError::LayerOrderConflict {
                predecessor: source,
                dependent: target,
                predecessor_layer: 2,
                dependent_layer: 0,
            }
        );
    }

    #[test]
    fn zero_round_timeline_tail_does_not_extend_causal_bounds() {
        let mut graph = SubGraph::new();
        let source = add_node(&mut graph, 0, 0);
        let target = add_node(&mut graph, 1, 0);
        graph
            .node_mut(source)
            .unwrap()
            .expect_quantum_mut()
            .timeline = Some(QuantumTimeline {
            layer_round_ends: vec![1, 1],
        });
        graph.add_edge(source, target, BloqEdge::quantum(Vec::new()));

        let aligned = align_moment_lanes(
            &graph,
            vec![
                lane(source, vec![Some(MomentKind::Measurement)], 0),
                lane(target, vec![Some(MomentKind::Reset)], 1),
            ],
        )
        .unwrap();

        assert_eq!(aligned[0].slots.len(), 2);
    }
}
