//! Count serialized payloads and expression boxes in existing binary IR files.
//! `cargo run --release -p bloq_ir --example inspect_ir -- FILE.bloq [...]`

use bloq_ir::{Bloq, ClassicalExpr, ClassicalNode, RegionNode};

fn encoded_bytes(value: &impl serde::Serialize) -> Result<usize, postcard::Error> {
    postcard::to_extend(value, Vec::new()).map(|bytes| bytes.len())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut paths = std::env::args_os().skip(1).peekable();
    if paths.peek().is_none() {
        return Err("usage: inspect_ir FILE.bloq [...]".into());
    }
    for (index, path) in paths.enumerate() {
        let binary = std::fs::read(path)?;
        let program = Bloq::from_binary(&binary)?;
        let (mut nodes, mut edges, mut computes) = (0, 0, 0);
        let mut classical_activations = 0;
        let (mut expression_bytes, mut expression_nodes, mut expression_boxes) =
            (0, 0usize, 0usize);
        let mut expression_heap_bytes = 0usize;
        let (mut observable_bytes, mut record_terms, mut binding_bytes, mut bindings) =
            (0, 0, 0, 0);
        let mut max_expression_depth = 0usize;
        for (_, level) in program.levels() {
            nodes += level.node_count();
            edges += level.edge_count();
            for (_, node) in level.nodes() {
                classical_activations +=
                    usize::from(node.try_classical().is_some() && node.activation.is_some());
                let expr = match node.try_classical() {
                    Some(ClassicalNode::Compute { expr }) => {
                        computes += 1;
                        Some(expr)
                    }
                    Some(ClassicalNode::Discard { condition }) => Some(condition),
                    Some(
                        classical @ ClassicalNode::Observable {
                            measurements,
                            operators,
                            ..
                        },
                    ) => {
                        observable_bytes += encoded_bytes(classical)?;
                        record_terms += measurements.len();
                        binding_bytes += encoded_bytes(operators)?;
                        bindings += operators.len();
                        None
                    }
                    _ => node.try_region().map(|region| match region {
                        RegionNode::RepeatUntilSuccess {
                            restart_condition, ..
                        } => restart_condition,
                    }),
                };
                let Some(expr) = expr else { continue };
                expression_bytes += encoded_bytes(expr)?;
                let mut pending = vec![(expr, 1)];
                while let Some((expr, depth)) = pending.pop() {
                    expression_nodes += 1;
                    max_expression_depth = max_expression_depth.max(depth);
                    let heap_bytes = match expr {
                        ClassicalExpr::Parity { inputs, .. } => {
                            std::mem::size_of_val(inputs.as_ref())
                        }
                        _ => std::mem::size_of_val(expr.operands()),
                    };
                    expression_boxes += usize::from(heap_bytes != 0);
                    expression_heap_bytes += heap_bytes;
                    pending.extend(expr.operands().iter().map(|operand| (operand, depth + 1)));
                }
            }
        }
        let binary_bytes = binary.len();
        let template_bytes = encoded_bytes(program.templates())?;
        println!(
            "{{\"index\":{index},\"binary_bytes\":{binary_bytes},\"template_bytes\":{template_bytes},\"nodes\":{nodes},\"edges\":{edges},\"computes\":{computes},\"classical_activations\":{classical_activations},\"expression_bytes\":{expression_bytes},\"expression_nodes\":{expression_nodes},\"expression_boxes\":{expression_boxes},\"expression_heap_bytes\":{expression_heap_bytes},\"expression_size\":{},\"max_expression_depth\":{max_expression_depth},\"observable_bytes\":{observable_bytes},\"record_terms\":{record_terms},\"binding_bytes\":{binding_bytes},\"bindings\":{bindings}}}",
            std::mem::size_of::<ClassicalExpr>()
        );
    }
    Ok(())
}
