//! Structural inventory of a program's stored IR.

use std::collections::BTreeMap;
use std::fmt;

use crate::{Bloq, BloqEdge, BloqNodeKind, ClassicalNode};

/// Structural counts across every graph level, including guarded and RUS bodies.
///
/// These describe stored IR without inspecting physical circuit payloads,
/// expanding fixed repeats, selecting guards, or multiplying RUS attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BloqStats {
    /// Templates in the compiled pool, including unreferenced templates.
    pub template_count: usize,
    /// Nodes across all graph levels, including region containers.
    pub node_count: usize,
    /// Edges across all graph levels.
    pub edge_count: usize,
    /// Node counts by concrete variant name, including zero-count types.
    pub node_counts: BTreeMap<&'static str, usize>,
    /// Edge counts by concrete variant, including zero-count types.
    pub edge_counts: BTreeMap<&'static str, usize>,
    /// Whether execution structure is fixed: no regions, activation, guarded
    /// membership/seams, or shot discard. Ordinary classical readout/decoder
    /// computation and fixed circuit repeats do not make structure dynamic.
    /// This does not certify validity or backend emittability.
    pub is_static: bool,
}

impl fmt::Display for BloqStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Bloq IR")?;
        writeln!(f, "├── Compiled Templates: {}", self.template_count)?;
        for (label, count, types) in [
            ("Nodes", self.node_count, &self.node_counts),
            ("Edges", self.edge_count, &self.edge_counts),
        ] {
            writeln!(f, "├── {label}: {count}")?;
            for (index, (kind, count)) in types.iter().enumerate() {
                let branch = if index + 1 == types.len() {
                    "└──"
                } else {
                    "├──"
                };
                writeln!(f, "│   {branch} {kind}: {count}")?;
            }
        }
        write!(
            f,
            "└── Static: {}",
            if self.is_static { "Yes" } else { "No" }
        )
    }
}

/// Failure to inventory stored IR; this does not certify well-formedness.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BloqStatsError {
    /// A structural count cannot be represented by `usize`.
    #[error("structural IR statistics count overflows usize")]
    CountOverflow,
}

fn add(total: &mut usize, count: usize) -> Result<(), BloqStatsError> {
    *total = total
        .checked_add(count)
        .ok_or(BloqStatsError::CountOverflow)?;
    Ok(())
}

impl Bloq {
    /// Inventory stored IR structure without inspecting physical circuit payloads.
    ///
    /// Unlike [`Self::node_count`] and [`Self::edge_count`], counts include all
    /// nested graph levels and concrete node/edge kinds. Every stored region
    /// body contributes once, independently of guards and retry attempts.
    /// Static means fixed execution structure: no regions, node activation,
    /// conditional quantum membership/seams, or shot discard. Classical
    /// computation/readout/decoding and fixed circuit repeats alone remain static.
    /// This is neither the explicit [`Self::validate`] audit nor a guarantee
    /// that a particular backend can emit the program.
    ///
    /// # Errors
    ///
    /// Returns [`BloqStatsError::CountOverflow`] if a structural total overflows.
    pub fn stats(&self) -> Result<BloqStats, BloqStatsError> {
        let mut stats = BloqStats {
            template_count: self.templates().len(),
            node_count: 0,
            edge_count: 0,
            node_counts: [
                "Quantum",
                "Compute",
                "Observable",
                "Discard",
                "RepeatUntilSuccess",
            ]
            .into_iter()
            .map(|kind| (kind, 0))
            .collect(),
            edge_counts: [("Quantum", 0), ("Value", 0), ("Compose", 0), ("Order", 0)].into(),
            is_static: true,
        };
        for (_, level) in self.levels() {
            add(&mut stats.node_count, level.node_count())?;
            add(&mut stats.edge_count, level.edge_count())?;
            for (_, node) in level.nodes() {
                stats.is_static &= node.activation.is_none();
                stats.is_static &= match &node.kind {
                    BloqNodeKind::Quantum(quantum) => quantum.guards.is_empty(),
                    BloqNodeKind::Classical(classical) => {
                        !matches!(classical.as_ref(), ClassicalNode::Discard { .. })
                    }
                    BloqNodeKind::Region(_) => false,
                };
                add(stats.node_counts.entry(node.kind_name()).or_default(), 1)?;
            }
            for edge in level.edges() {
                let kind = match edge.edge {
                    BloqEdge::Quantum(quantum) => {
                        stats.is_static &= quantum.guard.is_none();
                        "Quantum"
                    }
                    BloqEdge::Value { .. } => "Value",
                    BloqEdge::Compose { .. } => "Compose",
                    BloqEdge::Order => "Order",
                };
                add(stats.edge_counts.entry(kind).or_default(), 1)?;
            }
        }
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit::{CircuitBody, CoordCircuit, Op, PauliBasis};
    use crate::lowering::{BloqTemplate, TemplateInstance, TemplateInstanceId};
    use crate::{
        BloqNode, ClassicalExpr, ObservableOutput, QuantumGuard, RegionNode, SubGraph, ValueRef,
    };
    use glam::ivec2;

    #[test]
    fn structural_stats_count_nested_kinds_without_inspecting_or_expanding_circuits() {
        let mut circuit = CoordCircuit::new();
        circuit.measure(PauliBasis::Z, [ivec2(1, 0)]);
        let repeated = circuit.add_body(CircuitBody::new());
        circuit
            .body_mut(circuit.entry_body())
            .unwrap()
            .ops_mut()
            .push(Op::Repeat {
                body: repeated,
                repetitions: u32::MAX,
            });
        let mut program = Bloq::new();
        let template = program.add_template(BloqTemplate::new(circuit));
        let mut body = SubGraph::new();
        let mut node = BloqNode::from_members(vec![]);
        node.expect_quantum_mut()
            .instances
            .push(TemplateInstance::new(
                TemplateInstanceId(0),
                template,
                ivec2(i32::MAX, 0),
            ));
        let a = body.add_node(node);
        let b = body.add_node(BloqNode::classical(ClassicalNode::Compute {
            expr: ClassicalExpr::Const(false),
        }));
        body.add_edge(a, b, BloqEdge::Order);
        body.add_edge(b, a, BloqEdge::value(0));
        program.add_node(BloqNode::region(RegionNode::RepeatUntilSuccess {
            body,
            restart_condition: ClassicalExpr::In(0),
            restart_source: Some(ValueRef {
                node: b,
                output: ObservableOutput::Corrected,
            }),
        }));
        let before = program.to_binary();
        assert!(
            program.qubit_count().is_err(),
            "physical coordinate overflow does not block structural reads"
        );
        let stats = program.stats().unwrap();
        assert_eq!(stats.node_count, 3);
        assert_eq!(stats.node_counts.values().sum::<usize>(), stats.node_count);
        assert_eq!(stats.node_counts["Compute"], 1);
        assert_eq!(stats.node_counts["Quantum"], 1);
        assert_eq!(stats.node_counts["RepeatUntilSuccess"], 1);
        assert_eq!(stats.edge_count, 2);
        assert_eq!(
            stats.edge_counts,
            [("Compose", 0), ("Order", 1), ("Quantum", 0), ("Value", 1)].into()
        );
        assert_eq!(stats.template_count, 1);
        assert!(!stats.is_static);
        assert_eq!(
            stats.to_string(),
            "Bloq IR\n├── Compiled Templates: 1\n├── Nodes: 3\n│   ├── Compute: 1\n│   ├── Discard: 0\n│   ├── Observable: 0\n│   ├── Quantum: 1\n│   └── RepeatUntilSuccess: 1\n├── Edges: 2\n│   ├── Compose: 0\n│   ├── Order: 1\n│   ├── Quantum: 0\n│   └── Value: 1\n└── Static: No"
        );
        assert_eq!(program.node_count(), 1, "existing count remains top-level");
        assert_eq!(program.to_binary(), before, "statistics preserve stored IR");
        let mut count = usize::MAX;
        assert_eq!(add(&mut count, 1), Err(BloqStatsError::CountOverflow));
    }

    #[test]
    fn static_classification_tracks_execution_structure_not_classical_values() {
        let mut program = Bloq::new();
        assert!(program.stats().unwrap().is_static);
        let readout = program.add_node(BloqNode::classical(ClassicalNode::observable(0)));
        for classical in [
            ClassicalNode::Compute {
                expr: ClassicalExpr::And(Box::new([ClassicalExpr::In(0), ClassicalExpr::In(1)])),
            },
            ClassicalNode::observable_fragment(Vec::new(), Vec::new()),
        ] {
            program.add_node(BloqNode::classical(classical));
        }
        assert!(
            program.stats().unwrap().is_static,
            "classical computation has fixed structure"
        );
        program.node_mut(readout).unwrap().activation = Some(0);
        assert!(!program.stats().unwrap().is_static);
        program.node_mut(readout).unwrap().activation = None;
        let discard = program.add_node(BloqNode::classical(ClassicalNode::Discard {
            condition: ClassicalExpr::Const(false),
        }));
        assert!(
            !program.stats().unwrap().is_static,
            "stored discard remains dynamic even with constant predicate"
        );
        program.remove_node(discard);
        let a = program.add_node(BloqNode::from_members(vec![]));
        let b = program.add_node(BloqNode::from_members(vec![]));
        program.add_edge(a, b, BloqEdge::quantum(vec![]));
        assert!(program.stats().unwrap().is_static);
        program
            .node_mut(a)
            .unwrap()
            .expect_quantum_mut()
            .guards
            .push(QuantumGuard::default());
        assert!(!program.stats().unwrap().is_static);
        program
            .node_mut(a)
            .unwrap()
            .expect_quantum_mut()
            .guards
            .clear();
        let mut guarded = BloqEdge::quantum(vec![]);
        let BloqEdge::Quantum(edge) = &mut guarded else {
            unreachable!()
        };
        edge.guard = Some(ValueRef {
            node: readout,
            output: ObservableOutput::Corrected,
        });
        program.add_edge(b, a, guarded);
        assert!(!program.stats().unwrap().is_static);
        program.add_node(BloqNode::region(RegionNode::RepeatUntilSuccess {
            restart_source: None,
            restart_condition: ClassicalExpr::Const(false),
            body: SubGraph::new(),
        }));
        assert_eq!(
            program.stats().unwrap().node_counts["RepeatUntilSuccess"],
            1
        );
    }
}
