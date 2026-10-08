// [build-start]
use bloq::graph::{Action, Expr, FeedbackTarget, MeasureTarget};
use bloq::graph::{BitOutput, ModuleInterface, PortDirection, QuantumPort};
use bloq::prelude::*;

fn quantum_port(name: &str, position: [i32; 3], direction: PortDirection) -> QuantumPort {
    QuantumPort {
        name: name.into(),
        position: position.into(),
        direction,
        resource_type: "data".into(),
    }
}

fn build_graph() -> Result<BlockGraph> {
    let mut read = GalleryItem::CNOT.build().flatten()?;
    read.remove_block([1, 1, 3]);
    read.try_add_block(Block::new([1, 1, 3], BlockKind::Measurement(Basis::Z)))?;
    read.try_add_pipe(Pipe::new([1, 1, 2], Direction::ZPLUS))?;
    read.set_actions(vec![Action::Measure {
        name: "mz".into(),
        target: MeasureTarget::Node(IVec3::new(1, 1, 3)),
    }])?;
    read.name = "ReadParity".into();
    read.interface = ModuleInterface {
        quantum_ports: vec![
            quantum_port("control", [0, 0, 0], PortDirection::Input),
            quantum_port("target", [1, 1, 0], PortDirection::Input),
            quantum_port("control_out", [0, 0, 3], PortDirection::Output),
        ],
        bit_outputs: vec![BitOutput {
            name: "result".into(),
            expr: Expr::Var("mz".into()),
        }],
        ..Default::default()
    };
    let mut correct = BlockGraph::new();
    correct
        .try_add_block(Block::new([0, 0, 0], BlockKind::Port).with_port_role(PortRole::Input)?)?;
    correct.try_add_block(Block::new([0, 0, 1], BlockKind::Cube(CubeKind::ZXZ)))?;
    correct
        .try_add_block(Block::new([0, 0, 2], BlockKind::Port).with_port_role(PortRole::Output)?)?;
    correct.try_add_pipe(Pipe::new([0, 0, 0], Direction::ZPLUS))?;
    correct.try_add_pipe(Pipe::new([0, 0, 1], Direction::ZPLUS))?;
    correct.interface = ModuleInterface {
        quantum_ports: vec![
            quantum_port("q_in", [0, 0, 0], PortDirection::Input),
            quantum_port("q_out", [0, 0, 2], PortDirection::Output),
        ],
        bit_inputs: vec!["flip".into()],
        ..Default::default()
    };
    correct.set_actions_with_inputs(
        vec![Action::Feedback {
            targets: vec![FeedbackTarget {
                pauli: PauliBasis::X,
                target: IVec3::new(0, 0, 1),
                direction: None,
            }],
            condition: Some(Expr::Var("flip".into())),
        }],
        ["flip".into()],
    )?;
    correct.name = "Correct".into();
    let mut graph = BlockGraph::from_definitions(vec![read, correct, BlockGraph::new()])?;
    graph.place_module("read", "ReadParity")?;
    graph.place_module("correct", "Correct")?;
    graph.connect_modules("read.control_out", "correct.q_in")?;
    graph.bind_modules("read.result", "correct.flip")?;
    Ok(graph)
}
// [build-end]

fn main() -> Result {
    let graph = build_graph()?;
    graph.to_file("module-feedforward.blog")?;
    compile(&graph, 3)?.validate()?;
    Ok(())
}
