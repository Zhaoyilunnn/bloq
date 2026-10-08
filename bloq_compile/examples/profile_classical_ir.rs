//! Structural compression diagnostic: `profile_classical_ir WIDTH|GALLERY`.
//! Counts exact payload sharing without changing activation or decoder semantics.

use std::collections::{BTreeMap, HashMap, HashSet};

use bloq_compile::{CompileConfig, CompileContext};
use bloq_graph::GalleryItem;
use bloq_ir::lowering::{InstanceBoundaryOperator, InstanceMeasurement};
use bloq_ir::{Bloq, BloqEdge, ClassicalExpr, ClassicalNode, ValueRole};

fn expression_size(expr: &ClassicalExpr) -> usize {
    1 + expr.operands().iter().map(expression_size).sum::<usize>()
}

fn counts(bloq: &Bloq) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::<String, usize>::new();
    let mut add = |key: &str, n| *counts.entry(key.to_owned()).or_default() += n;
    let mut record_packets = HashSet::new();
    let mut operator_packets = HashSet::new();
    let mut record_terms = HashSet::<&InstanceMeasurement>::new();
    let mut operator_terms = HashSet::<&InstanceBoundaryOperator>::new();
    let mut expression_shapes = HashSet::new();
    let mut record_chunks = HashSet::new();
    let mut chunk_shapes = HashMap::new();
    let templates: HashMap<_, _> = bloq
        .levels()
        .flat_map(|(_, level)| level.quantum_nodes())
        .flat_map(|(_, node)| node.instances.iter())
        .map(|instance| (instance.id, instance.template_id))
        .collect();
    for (_, level) in bloq.levels() {
        add("ir_nodes", level.node_count());
        add("ir_edges", level.edge_count());
        for (id, node) in level.nodes() {
            let Some(classical) = node.try_classical() else {
                continue;
            };
            add("classical_nodes", 1);
            let kind = match classical {
                ClassicalNode::Compute { .. } => "Compute",
                ClassicalNode::Observable { .. } => "Observable",
                ClassicalNode::Discard { .. } => "Discard",
            };
            add(kind, 1);
            if node.activation.is_some() {
                add(&format!("activated_{kind}"), 1);
            }
            if let ClassicalNode::Compute { expr } = classical {
                add("expression_nodes", expression_size(expr));
                expr.for_each_input(&mut |_| add("expression_input_occurrences", 1));
                if expr.is_linear() {
                    add("linear_computes", 1);
                }
                if expression_shapes.insert(expr) {
                    add("expression_shapes", 1);
                    add("pooled_expression_nodes", expression_size(expr));
                    expr.for_each_input(&mut |_| add("pooled_expression_input_occurrences", 1));
                }
                add("compute_value_inputs", level.value_inputs(id).count());
            }
            let records = classical.measurements();
            if !records.is_empty() {
                add("record_packets", 1);
                add("record_terms", records.len());
                record_terms.extend(records);
                for chunk in records.chunk_by(|a, b| a.instance == b.instance) {
                    add("record_chunk_uses", 1);
                    if record_chunks.insert(chunk) {
                        add("unique_record_chunks", 1);
                        add("pooled_record_chunk_terms", chunk.len());
                    }
                    let owner = chunk[0].instance;
                    let key = (
                        templates[&owner],
                        chunk
                            .iter()
                            .map(|record| record.measurement)
                            .collect::<Vec<_>>(),
                    );
                    let (shape, _) = chunk_shapes.get_key_value(&key).unwrap_or((&key, &()));
                    // A local row plus its owner reconstructs the exact ordered
                    // payload, including duplicates. This does not move reads
                    // across activation or change the quantum dependency.
                    assert!(chunk.iter().copied().eq(shape.1.iter().map(|&measurement| {
                        InstanceMeasurement {
                            instance: owner,
                            measurement,
                        }
                    })));
                    if chunk_shapes.insert(key, ()).is_none() {
                        add("record_chunk_shapes", 1);
                        add("record_chunk_shape_terms", chunk.len());
                    }
                }
                if record_packets.insert(records) {
                    add("unique_record_packets", 1);
                    add("pooled_record_terms", records.len());
                    add(
                        "pooled_record_chunk_uses",
                        records.chunk_by(|a, b| a.instance == b.instance).count(),
                    );
                }
            }
            let operators = classical.operators();
            if !operators.is_empty() {
                add("operator_packets", 1);
                add("operator_terms", operators.len());
                operator_terms.extend(operators);
                if operator_packets.insert(operators) {
                    add("unique_operator_packets", 1);
                    add("pooled_operator_terms", operators.len());
                }
            }
        }
        for edge in level.edges() {
            match edge.edge {
                BloqEdge::Quantum(_) => add("quantum_edges", 1),
                BloqEdge::Order => add("order_edges", 1),
                BloqEdge::Compose { .. } => {
                    add("compose_edges", 1);
                    add("readout_input_edges", 1);
                }
                BloqEdge::Value { slot, role, .. } => {
                    add("value_edges", 1);
                    let target = &level[edge.target];
                    if Some(*slot) == target.activation {
                        add("activation_edges", 1);
                    } else if matches!(
                        target.try_classical(),
                        Some(ClassicalNode::Observable { .. })
                    ) {
                        add("readout_input_edges", 1);
                    }
                    if matches!(target.try_classical(), Some(ClassicalNode::Compute { .. }))
                        && matches!(
                            level[edge.source].try_classical(),
                            Some(ClassicalNode::Compute { .. })
                        )
                    {
                        add("compute_to_compute_edges", 1);
                    }
                    match role {
                        ValueRole::Data => add("data_edges", 1),
                        ValueRole::ReadoutFold => add("readout_fold_edges", 1),
                        ValueRole::FeedbackFold { .. } => add("feedback_fold_edges", 1),
                    }
                }
            }
        }
    }
    add("unique_records", record_terms.len());
    add("unique_operators", operator_terms.len());
    counts
}

fn main() {
    let source = std::env::args().nth(1).expect("WIDTH or gallery slug");
    let graph = if let Ok(bits) = source.parse::<usize>() {
        assert!(bits >= 3, "width must be at least three bits");
        bloq_test::benchmark::controlled_adder(bits)
    } else {
        source.parse::<GalleryItem>().expect("gallery slug").build()
    };
    let compiled = CompileContext::new(CompileConfig::new(3))
        .compile(&graph)
        .expect("compile diagnostic source");
    let counts = counts(&compiled.bloq);
    // Identities catch omitted node/edge kinds in this diagnostic.
    assert_eq!(
        counts["ir_edges"],
        [
            "quantum_edges",
            "order_edges",
            "value_edges",
            "compose_edges"
        ]
        .iter()
        .map(|key| counts.get(*key).copied().unwrap_or(0))
        .sum::<usize>()
    );
    assert_eq!(
        counts["classical_nodes"],
        ["Compute", "Observable", "Discard"]
            .iter()
            .map(|key| counts.get(*key).copied().unwrap_or(0))
            .sum::<usize>()
    );
    for (small, large) in [
        ("pooled_record_chunk_terms", "pooled_record_terms"),
        ("pooled_record_terms", "record_terms"),
        ("pooled_operator_terms", "operator_terms"),
    ] {
        assert!(counts.get(small).copied().unwrap_or(0) <= counts.get(large).copied().unwrap_or(0));
    }
    for (key, value) in counts {
        println!("{key} {value}");
    }
}
