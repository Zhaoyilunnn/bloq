//! Structural queries: the shapes a consumer would otherwise rediscover by
//! matching on [`BloqNodeKind`](crate::BloqNodeKind)/[`RegionNode`] variants
//! and hard-coding source-block coordinates.
//!
//! A compiled program has a small set of recurring landmarks — the region
//! nodes of a given kind, the seams feeding a selection, a graph level's
//! quantum tail, and the node realizing a named
//! source block. Each is a structural fact the IR already knows, so it belongs
//! here rather than in every backend and experiment that needs it.
//!
//! Queries that can only have one answer return [`StructureError`] when the
//! program does not have that shape, so a program whose lowering changed fails
//! at the query instead of silently producing a plausible wrong node.

use glam::IVec3;
use thiserror::Error;

use crate::{Bloq, BloqEdge, BloqNodeId, LevelPath, RegionNode, SubGraph};

/// Why a structural query found no unique answer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum StructureError {
    /// A node does not have exactly one quantum input.
    #[error("node {node:?} has {count} quantum inputs, expected exactly one")]
    QuantumInputNotUnique {
        /// Queried node.
        node: BloqNodeId,
        /// Quantum-input count.
        count: usize,
    },
    /// A selection has no quantum input.
    #[error("selection node {node:?} has no quantum input")]
    SelectionInputMissing {
        /// Selection node.
        node: BloqNodeId,
    },
    /// A graph level has no unique quantum tail.
    #[error(
        "graph level has {count} quantum tails (quantum nodes with no outgoing quantum edge), expected exactly one"
    )]
    QuantumTailNotUnique {
        /// Quantum-tail count.
        count: usize,
    },
}

/// Which [`RegionNode`] variant a structural query selects.
///
/// The discriminant half of [`RegionNode`], so a caller can filter regions
/// without matching a variant it does not intend to destructure. Exhaustive
/// like the IR data enums it mirrors (see the crate docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegionKind {
    /// Repeat-until-success region.
    RepeatUntilSuccess,
}

impl RegionNode {
    /// This region's variant, without its payload.
    pub fn kind(&self) -> RegionKind {
        RegionKind::RepeatUntilSuccess
    }
}

/// A program-unique address of one region node: the path to its graph level
/// plus its level-local id.
///
/// Both halves are needed because [`BloqNodeId`]s are level-local; see
/// [`LevelPath`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RegionRef {
    /// The level the region node lives at; empty for the top level.
    pub path: LevelPath,
    /// The region node's id within that level.
    pub node: BloqNodeId,
}

impl RegionRef {
    /// The region this reference names, or `None` once the program has been
    /// edited out from under it (a stale path, or an id whose node is gone).
    #[must_use]
    pub fn resolve<'a>(&self, bloq: &'a Bloq) -> Option<&'a RegionNode> {
        bloq.level_at(&self.path)?.node(self.node)?.try_region()
    }
}

impl Bloq {
    /// Every region node of `kind`, at every nesting level.
    ///
    /// Ordered by [`Bloq::levels`]: levels pre-order top-down, ascending node
    /// id within a level. Filter on [`RegionRef::path`] being
    /// [top-level](LevelPath::is_top_level) when only the outermost regions
    /// matter.
    #[must_use]
    pub fn regions_of(&self, kind: RegionKind) -> Vec<RegionRef> {
        self.levels()
            .flat_map(|(path, level)| {
                level
                    .nodes()
                    .filter(move |(_, node)| {
                        node.try_region()
                            .is_some_and(|region| region.kind() == kind)
                    })
                    .map(move |(node, _)| RegionRef {
                        path: path.clone(),
                        node,
                    })
            })
            .collect()
    }

    /// The quantum seams each top-level guarded component waits on, as
    /// `(source, selection)` pairs in ascending selection-node-id order.
    ///
    /// Subdividing those edges ([`Bloq::insert_memory_rounds_batch`]) delays the
    /// component until its guard inputs are ready. Within one component, seams
    /// are ordered by source node id. This query does not certify that an
    /// unresolved selection supports scheduling or memory edits.
    ///
    /// # Errors
    ///
    /// Returns [`StructureError::SelectionInputMissing`] if a guarded component has no
    /// incoming quantum edge.
    pub fn selection_seams(&self) -> Result<Vec<(BloqNodeId, BloqNodeId)>, StructureError> {
        let mut seams = Vec::new();
        for (node, _) in self
            .top()
            .quantum_nodes()
            .filter(|(_, quantum)| !quantum.guards.is_empty())
        {
            let mut sources: Vec<_> = self
                .incoming(node)
                .filter(|edge| matches!(edge.edge, BloqEdge::Quantum(_)))
                .map(|edge| edge.source)
                .collect();
            if sources.is_empty() {
                return Err(StructureError::SelectionInputMissing { node });
            }
            sources.sort_unstable();
            seams.extend(sources.into_iter().map(|source| (source, node)));
        }
        Ok(seams)
    }

    /// The top-level node realizing source block `pos`, or `None` when no node
    /// carries it.
    ///
    /// Lowering partitions the source blocks across
    /// [`NodeProvenance::BlockComponent`](crate::NodeProvenance::BlockComponent)
    /// nodes, so at most one node can match — this is a lookup, not a search
    /// with a preferred answer.
    #[must_use]
    pub fn node_by_block(&self, pos: IVec3) -> Option<BloqNodeId> {
        self.nodes().find_map(|(id, node)| {
            node.block_members()
                .iter()
                .any(|member| member.pos == pos)
                .then_some(id)
        })
    }
}

impl SubGraph {
    /// The source of the single quantum edge into `node`.
    ///
    /// This is the seam a padding edit subdivides: every consumer that idles
    /// before a node first has to establish that the node has exactly one
    /// quantum predecessor to idle behind.
    ///
    /// # Errors
    ///
    /// Returns [`StructureError::QuantumInputNotUnique`] when `node` has zero
    /// or several incoming quantum edges (a stale id reads as zero).
    pub fn quantum_input(&self, node: BloqNodeId) -> Result<BloqNodeId, StructureError> {
        let sources: Vec<_> = self
            .incoming(node)
            .filter(|edge| matches!(edge.edge, BloqEdge::Quantum(_)))
            .map(|edge| edge.source)
            .collect();
        match sources[..] {
            [source] => Ok(source),
            _ => Err(StructureError::QuantumInputNotUnique {
                node,
                count: sources.len(),
            }),
        }
    }

    /// The unique quantum tail of this graph level: a quantum node with no
    /// outgoing quantum edge. Classical consumers do not disqualify it.
    ///
    /// This query applies to top-level graphs and nested bodies alike.
    ///
    /// # Errors
    ///
    /// Returns [`StructureError::QuantumTailNotUnique`] unless exactly one
    /// quantum node is terminal.
    pub fn quantum_tail(&self) -> Result<BloqNodeId, StructureError> {
        let tails: Vec<_> = self
            .quantum_nodes()
            .map(|(id, _)| id)
            .filter(|&id| {
                !self
                    .outgoing(id)
                    .any(|edge| matches!(edge.edge, BloqEdge::Quantum(_)))
            })
            .collect();
        match tails[..] {
            [tail] => Ok(tail),
            _ => Err(StructureError::QuantumTailNotUnique { count: tails.len() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use glam::ivec3;

    use super::*;
    use crate::BodySelector;

    /// One `RepeatUntilSuccess` (its body a source node feeding an escape node
    /// that has no quantum successor) followed by a guarded component fed by one quantum
    /// seam — the landmark set every query here answers about.
    fn program() -> Bloq {
        Bloq::from_text(
            "\
BLOQIR 1

template t0 {
  circuit {
    MPP X(0,0):m0
  }
}

graph {
  n0 quantum {
    instance i0 t0 @ (0,0)
    from blocks (0,0,0)
  }
  n1 rus in0 source n2 {
    body {
      n0 quantum {
        instance i1 t0 @ (2,0)
      }
      n1 quantum {
        instance i2 t0 @ (2,2)
      }
      n2 observable fragment measurements i2:m0 from generator 0
      n0 -> n1 quantum (0,0,0)>(0,0,1)
      n1 -> n2 order
    }
  }
  n2 quantum {
    instance i3 t0 @ (4,0)
    from blocks (1,0,2)
  }
  n3 quantum {
    instance i4 t0 @ (4,0)
    guard 0 i4
  }
  n4 compute 1
  n4 -> n3 value 0
  n0 -> n2 quantum (0,0,0)>(0,0,1)
  n2 -> n3 quantum (1,0,2)>(1,0,3)
}
",
        )
        .expect("valid .bloqir text")
    }

    #[test]
    fn regions_of_selects_one_variant_across_levels() {
        let program = program();

        assert_eq!(
            program.regions_of(RegionKind::RepeatUntilSuccess),
            [RegionRef {
                path: LevelPath::default(),
                node: BloqNodeId(1),
            }]
        );
    }

    #[test]
    fn selection_seams_report_the_edge_each_selection_waits_on() {
        assert_eq!(
            program().selection_seams(),
            Ok(vec![(BloqNodeId(2), BloqNodeId(3))])
        );
    }

    #[test]
    fn selection_seams_report_every_quantum_input() {
        let mut program = program();
        program.add_edge(
            BloqNodeId(0),
            BloqNodeId(3),
            BloqEdge::quantum(vec![crate::TemporalPipeRef {
                src: ivec3(0, 0, 0),
                dst: ivec3(0, 0, 1),
                hadamard: false,
            }]),
        );

        assert_eq!(
            program.selection_seams(),
            Ok(vec![
                (BloqNodeId(0), BloqNodeId(3)),
                (BloqNodeId(2), BloqNodeId(3)),
            ])
        );
    }

    #[test]
    fn selection_seams_reject_a_component_without_quantum_inputs() {
        use petgraph::visit::EdgeRef;
        let mut program = program();
        let edge = program
            .graph()
            .edges_connecting(
                petgraph::stable_graph::NodeIndex::new(2),
                petgraph::stable_graph::NodeIndex::new(3),
            )
            .next()
            .unwrap()
            .id();
        program.graph_mut_internal().remove_edge(edge);

        assert_eq!(
            program.selection_seams(),
            Err(StructureError::SelectionInputMissing {
                node: BloqNodeId(3),
            })
        );
    }

    #[test]
    fn quantum_tail_is_the_terminal_quantum_node_of_a_region_body() {
        let program = program();
        let body = program
            .level_at(&LevelPath::default().child(BloqNodeId(1), BodySelector::Body))
            .expect("rus body");

        assert_eq!(body.quantum_tail(), Ok(BloqNodeId(1)));
        assert_eq!(program.top().quantum_tail(), Ok(BloqNodeId(3)));
    }

    #[test]
    fn quantum_tail_ignores_classical_consumers_and_requires_a_unique_tail() {
        use crate::{BloqNode, ClassicalExpr, ClassicalNode, QuantumNode};

        let mut graph = SubGraph::new();
        assert_eq!(
            graph.quantum_tail(),
            Err(StructureError::QuantumTailNotUnique { count: 0 })
        );
        let head = graph.add_node(BloqNode::quantum(QuantumNode::default()));
        let tail = graph.add_node(BloqNode::quantum(QuantumNode::default()));
        let readout = graph.add_node(BloqNode::classical(ClassicalNode::Compute {
            expr: ClassicalExpr::Const(false),
        }));
        graph.add_edge(head, tail, BloqEdge::quantum(vec![]));
        graph.add_edge(tail, readout, BloqEdge::Order);
        assert_eq!(graph.quantum_tail(), Ok(tail));
        graph.add_node(BloqNode::quantum(QuantumNode::default()));
        assert_eq!(
            graph.quantum_tail(),
            Err(StructureError::QuantumTailNotUnique { count: 2 })
        );
    }

    #[test]
    fn node_by_block_addresses_a_node_by_its_source_coordinates() {
        let program = program();

        assert_eq!(program.node_by_block(ivec3(1, 0, 2)), Some(BloqNodeId(2)));
        // A block inside a region body is not a top-level member.
        assert_eq!(program.node_by_block(ivec3(9, 9, 9)), None);
    }
}
