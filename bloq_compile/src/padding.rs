//! Compile-time producer of the IR's memory-padding provenance
//! ([`bloq_ir::PipePadding`]).
//!
//! [`record_edge_padding`] stamps every padding-capable quantum edge, including
//! region boundaries, with
//! precompiled one-round and looped padding templates plus the offsets placing
//! them over the edge's cross-section, so
//! [`bloq_ir::Bloq::insert_memory_rounds`] can splice waits into a deserialized
//! program without the source graph or a recompile. A temporal Hadamard is a
//! real IR node, so its parent and child edges are stamped independently from
//! their adjacent cube faces.

use std::sync::{Arc, Mutex};

use bloq_graph::{Basis, BlockGraph, BlockKind, PortRole};
use bloq_ir::{Bloq, BloqEdge, BloqNodeId, NodeProvenance, PipePadding, TemporalPipeRef};
use glam::{IVec2, IVec3};

use crate::CompileError;
use crate::block::compile_seam_padding_rounds;

/// The round count the recorded looped padding template is compiled at. Any
/// value whose repeat body survives chunk normalization (>= 3, i.e. two or
/// more repetitions) works; insertion specializes this padding template's
/// repetition count to the requested duration.
const LOOPED_PADDING_ROUNDS: u32 = 3;

/// The patch inputs a pipe's padding template is compiled from, plus the
/// per-block offset placing it.
///
/// The patch is a plain `d x d` surface code face named by the basis on its
/// `y`-normal edges. That one bit is the whole space: a cube admits a temporal
/// pipe only when its odd-basis axis is spatial, and neither the temporal
/// basis, spatial connectivity, nor the layer's CX-slot count can reach a
/// padding round. `fixed_bulk::tests::seam_padding_matches_the_cube_derived_patch`
/// pins this against cube-derived padding.
struct PipePatch {
    y_face_basis: Basis,
    offset: IVec2,
}

/// Immutable padding templates shared by every compilation that reaches this
/// cache — one [`crate::CompileContext`], or all the contexts sharing a
/// [`crate::SharedCompileCache`] shard. Program-local template ids are still
/// assigned by each output [`Bloq`].
///
/// The lock is internal so the padding pass can hold `&self` for its whole walk
/// without serializing concurrent compilations behind it: only the map probe
/// and insert are guarded, never [`compile_padding_templates`].
#[derive(Debug, Default)]
pub(crate) struct PaddingTemplateCache {
    templates: Mutex<crate::FxMap<Basis, CachedPaddingTemplates>>,
}

impl PaddingTemplateCache {
    fn get_or_compile(
        &self,
        y_face_basis: Basis,
        distance: u32,
    ) -> Result<CachedPaddingTemplates, CompileError> {
        if let Some(templates) = crate::cache::lock(&self.templates).get(&y_face_basis) {
            return Ok(templates.clone());
        }
        let templates = compile_padding_templates(y_face_basis, distance)?;
        // A concurrent miss on the same patch may have won the race; its entry
        // is just as good, and keeping it avoids invalidating what it returned.
        Ok(crate::cache::lock(&self.templates)
            .entry(y_face_basis)
            .or_insert(templates)
            .clone())
    }

    pub(crate) fn prepare(
        &self,
        needed: bool,
        distance: u32,
    ) -> Result<PreparedPaddingTemplates, CompileError> {
        if !needed {
            return Ok(PreparedPaddingTemplates::default());
        }
        let mut templates = crate::FxMap::default();
        for basis in [Basis::X, Basis::Z] {
            templates.insert(basis, self.get_or_compile(basis, distance)?);
        }
        Ok(PreparedPaddingTemplates { templates })
    }
}

/// Padding circuits captured by a prepared program. Final lowering
/// only assigns their program-local template ids.
#[derive(Debug, Default)]
pub(crate) struct PreparedPaddingTemplates {
    templates: crate::FxMap<Basis, CachedPaddingTemplates>,
}

/// The block whose side of the seam the wait sits on.
///
/// A wait idles the patch its *lower* node handed over, so the lower node's
/// block is the anchor. Around a materialized temporal Hadamard only one of the
/// two IR edges has a lower node that owns a block — the other's lower node is
/// the Hadamard itself — and there the upper node's block is the one whose patch
/// idles. If the other endpoint is a region, the temporal-pipe node itself
/// identifies which side of the Hadamard the seam occupies.
fn seam_anchor(
    bloq: &Bloq,
    graph: &BlockGraph,
    endpoints: (BloqNodeId, BloqNodeId),
    pipe: &TemporalPipeRef,
) -> Option<IVec3> {
    let (from, to) = endpoints;
    let (lower, upper) = pipe.endpoints_by_z();
    if owns_endpoint(bloq, graph, from, lower) {
        return Some(lower);
    }
    if owns_endpoint(bloq, graph, to, upper) {
        return Some(upper);
    }
    // A materialized temporal Hadamard owns the pipe rather than either
    // endpoint. On its upper edge the waiting patch is already Hadamard-
    // transformed, including when the upper endpoint lowers to a region.
    let is_pipe_node = |id| {
        matches!(
            bloq.node(id).map(|node| &node.provenance),
            Some(NodeProvenance::TemporalPipe { pipe: node_pipe }) if node_pipe == pipe
        )
    };
    if is_pipe_node(from) {
        return Some(upper);
    }
    if is_pipe_node(to) {
        return Some(lower);
    }
    // A region on an ordinary pipe (a RUS escape or selective fix) leaves the
    // seam anchored on its source patch.
    let is_region = |id| matches!(bloq.node(id), Some(node) if node.try_region().is_some());
    (is_region(from) || is_region(to)).then_some(lower)
}

/// Whether IR node `id` was lowered from the block covering `endpoint`.
/// Resolved through [`BlockGraph::get_endpoint_block`] so a block addressed by
/// its far end (a patch rotation) still matches its own source ref.
fn owns_endpoint(bloq: &Bloq, graph: &BlockGraph, id: BloqNodeId, endpoint: IVec3) -> bool {
    let Some(block) = graph.get_endpoint_block(endpoint) else {
        return false;
    };
    matches!(
        bloq.node(id).map(|node| &node.provenance),
        Some(NodeProvenance::BlockComponent { members })
            if members.iter().any(|member| member.pos == block.pos())
    )
}

/// The patch one IR edge's temporal pipe waits on.
///
/// The basis is read off the *pipe*, not off a cube: `infer_pipe_basis_from_endpoint`
/// reports the pipe's face bases as seen from one end, deriving them from
/// whichever end is a cube and flipping across a Hadamard. That is what lets a
/// seam terminating in a port, a `Y`, or a `T` injection resolve at all — those
/// blocks have no bases of their own, and the pipe's are exactly what idles.
fn edge_pipe_patch(
    bloq: &Bloq,
    graph: &BlockGraph,
    endpoints: (BloqNodeId, BloqNodeId),
    pipe: &TemporalPipeRef,
    distance: u32,
) -> Result<Option<PipePatch>, CompileError> {
    let (from, to) = endpoints;
    let (Some(from_node), Some(to_node)) = (bloq.node(from), bloq.node(to)) else {
        return Ok(None);
    };
    let addressable =
        |node: &bloq_ir::BloqNode| node.try_quantum().is_some() || node.try_region().is_some();
    if !addressable(from_node) || !addressable(to_node) {
        return Ok(None);
    }
    // The positionless temporal half shares its source address with the
    // expanded cube. Its virtual seam idles that cube's actual patch.
    if pipe.src == pipe.dst
        && (matches!(to_node.provenance, NodeProvenance::SpatialPortSubstitution { source, role: PortRole::Output } if source == pipe.src)
            || matches!(from_node.provenance, NodeProvenance::SpatialPortSubstitution { source, role } if source == pipe.src && role.has_input_boundary()))
    {
        let Some(BlockKind::Cube(kind)) = graph.get_block(pipe.src).map(bloq_graph::Block::kind)
        else {
            return Ok(None);
        };
        return Ok(Some(PipePatch {
            y_face_basis: kind.y(),
            offset: crate::compile::block_xy_offset(pipe.src.truncate(), distance)?,
        }));
    }
    let Some(anchor) = seam_anchor(bloq, graph, endpoints, pipe) else {
        return Ok(None);
    };
    let Some(source) = graph.get_pipe(pipe.src, pipe.dst) else {
        return Ok(None);
    };
    let [_, y_face_basis, _] = graph.infer_pipe_basis_from_endpoint(source, anchor);
    let Some(y_face_basis) = y_face_basis else {
        return Ok(None);
    };
    Ok(Some(PipePatch {
        y_face_basis,
        // A temporal pipe's two endpoints share a column, so the seam's
        // cross-section sits at the anchor's own layout position.
        offset: crate::compile::block_xy_offset(anchor.truncate(), distance)?,
    }))
}

/// Stamp each padding-capable `Quantum` edge in `bloq` with one
/// [`PipePadding`] per pipe. One-round and looped
/// ([`LOOPED_PADDING_ROUNDS`]) templates are deduped per patch shape.
///
/// Edges padding cannot fully cover — empty seams, unsupported endpoints, or a
/// pipe with no resolvable patch — retain empty provenance rather than
/// failing compilation. Region-boundary records also drive
/// [`bloq_ir::MemoryRoundTarget::After`].
/// Template compilation errors on a supported cube are real compile bugs and
/// propagate.
pub(crate) fn record_edge_padding(
    prepared: &PreparedPaddingTemplates,
    bloq: &mut Bloq,
    graph: &BlockGraph,
    distance: u32,
) -> Result<(), CompileError> {
    // Only top-level seams carry source pipes today (region-body Quantum edges
    // are synthetic), so this walk covers both ordinary and region-boundary
    // padding. Collected first: recording mutates the template pool.
    let edges: Vec<(BloqNodeId, BloqNodeId, Vec<TemporalPipeRef>)> = bloq
        .edges()
        .filter_map(|edge| match edge.edge {
            BloqEdge::Quantum(quantum) => Some((
                edge.source,
                edge.target,
                quantum.pipes.iter().map(|seam| seam.pipe).collect(),
            )),
            BloqEdge::Value { .. } | BloqEdge::Compose { .. } | BloqEdge::Order => None,
        })
        .collect();

    let mut by_patch: crate::FxMap<Basis, PaddingTemplates> = crate::FxMap::default();
    for (from, to, pipes) in edges {
        if pipes.is_empty() {
            continue;
        }
        let mut patches = Vec::with_capacity(pipes.len());
        for pipe in &pipes {
            let Some(patch) = edge_pipe_patch(bloq, graph, (from, to), pipe, distance)? else {
                patches.clear();
                break;
            };
            patches.push(patch);
        }
        if patches.len() != pipes.len() {
            continue;
        }

        let mut refs = Vec::with_capacity(pipes.len());
        for patch in &patches {
            let key = patch.y_face_basis;
            let templates = match by_patch.get(&key) {
                Some(&templates) => templates,
                None => {
                    let cached = prepared
                        .templates
                        .get(&key)
                        .expect("module-object preparation covers both patch bases")
                        .clone();
                    let templates = PaddingTemplates {
                        one_round: bloq.add_shared_template(cached.one_round),
                        looped: bloq.add_shared_template(cached.looped),
                    };
                    by_patch.insert(key, templates);
                    templates
                }
            };
            refs.push(PipePadding {
                offset: patch.offset,
                one_round: templates.one_round,
                looped: templates.looped,
            });
        }
        assert!(
            bloq.set_quantum_edge_padding(from, to, refs),
            "lowering emits one quantum edge per node pair"
        );
    }
    Ok(())
}

/// The two padding templates registered for one cube patch. A named pair so
/// the one-round and looped ids cannot be transposed at construction (both are
/// `TemplateId`, so a positional tuple would swap silently).
#[derive(Debug, Clone, Copy)]
struct PaddingTemplates {
    one_round: bloq_ir::TemplateId,
    looped: bloq_ir::TemplateId,
}

#[derive(Debug, Clone)]
struct CachedPaddingTemplates {
    one_round: Arc<bloq_ir::lowering::BloqTemplate>,
    looped: Arc<bloq_ir::lowering::BloqTemplate>,
}

/// Compile the one-round and looped padding templates for one patch shape.
fn compile_padding_templates(
    y_face_basis: Basis,
    distance: u32,
) -> Result<CachedPaddingTemplates, CompileError> {
    // Both round counts pad the same patch shape, so the entry point builds its
    // Theta(d^2) tiles once and compiles each count against it.
    let [one_round, looped] =
        compile_seam_padding_rounds(y_face_basis, distance, [1, LOOPED_PADDING_ROUNDS])?;
    Ok(CachedPaddingTemplates { one_round, looped })
}

#[cfg(test)]
mod tests {
    use bloq_ir::{LevelPath, MemoryRoundTarget};

    use super::*;

    #[test]
    fn virtual_spatial_inputs_support_physical_memory_padding() {
        for role in ["input", "multiplex"] {
            let graph = BlockGraph::from_blog_text(&format!(
                "BLOG 1.0\n0: Port [-1,0,0] role={role}\n1: ZXZ [0,0,0]\n2: Port [0,0,1] role=output\n0 -> +X\n1 -> +Z\n"
            )).unwrap();
            let mut program = crate::compile(&graph, 3).unwrap();
            let input = program.nodes().find(|(_, node)| matches!(node.provenance,
                NodeProvenance::SpatialPortSubstitution { role, .. } if role.has_input_boundary()
            )).unwrap().0;
            let target = program
                .outgoing(input)
                .find(|edge| matches!(edge.edge, BloqEdge::Quantum(_)))
                .unwrap()
                .target;
            program
                .insert_memory_rounds(
                    MemoryRoundTarget::Edge {
                        path: LevelPath::default(),
                        from: input,
                        to: target,
                    },
                    2,
                )
                .unwrap();
            program.validate().unwrap();
            let report = bloq_vm::run_bloq_with_io(
                &program,
                4,
                7,
                |sim, ctx| {
                    for input in ctx.inputs {
                        sim.cx(input.qubit, sim.num_qubits())?;
                    }
                    Ok(())
                },
                |_, _| Ok(()),
            )
            .unwrap();
            assert_eq!(report.discarded, 0);
            assert!(
                report
                    .detectors
                    .iter()
                    .all(|detector| detector.constant && detector.value == Some(false))
            );
        }
    }

    #[test]
    fn padding_template_cache_reuses_compiled_templates() {
        let cache = PaddingTemplateCache::default();

        let first = cache.get_or_compile(Basis::Z, 3).unwrap();
        let second = cache.get_or_compile(Basis::Z, 3).unwrap();

        assert!(Arc::ptr_eq(&first.one_round, &second.one_round));
        assert!(Arc::ptr_eq(&first.looped, &second.looped));
    }
}
