// [build-start]
use bloq::prelude::*;

fn build_graph() -> Result<BlockGraph> {
    let mut cnot = GalleryItem::CNOT.build();
    cnot.name = "CNOT".into();
    let mut graph = BlockGraph::from_definitions(vec![cnot, BlockGraph::new()])?;
    for index in 0..3 {
        graph.place_module(&format!("child{index}"), "CNOT")?;
    }
    graph.connect_modules("child0.control_out", "child1.control_in")?;
    graph.connect_modules("child1.control_out", "child2.control_in")?;
    Ok(graph)
}
// [build-end]

fn main() -> Result {
    let graph = build_graph()?;
    graph.to_file("module-three-cnots.blog")?;
    compile(&graph, 3)?.validate()?;
    #[cfg(feature = "graph-verify")]
    {
        use bloq::graph::verify::{BoundaryOrder, LogicalVerifier, QuizxGraph, parse_qasm};
        let expected: QuizxGraph = parse_qasm("OPENQASM 2.0; include \"qelib1.inc\"; qreg q[2]; cx q[0],q[1]; cx q[0],q[1]; cx q[0],q[1];")?.to_graph();
        let boundaries = BoundaryOrder::new(
            vec![IVec3::new(0, 0, 0), IVec3::new(1, 1, 0)],
            vec![IVec3::new(0, 0, 7), IVec3::new(1, 1, 7)],
        );
        LogicalVerifier::with_boundaries(&graph.flatten()?, boundaries)?
            .verify(&expected, 8, 0x5eed)?;
    }
    Ok(())
}
