// [build-start]
use bloq::graph::{ModuleInterface, ModuleRotation, PortDirection, QuantumPort};
use bloq::prelude::*;

fn interface(input_z: i32, output_z: i32) -> ModuleInterface {
    ModuleInterface {
        quantum_ports: vec![
            QuantumPort {
                name: "q_in".into(),
                position: IVec3::new(0, 0, input_z),
                direction: PortDirection::Input,
                resource_type: "data".into(),
            },
            QuantumPort {
                name: "q_out".into(),
                position: IVec3::new(0, 0, output_z),
                direction: PortDirection::Output,
                resource_type: "data".into(),
            },
        ],
        ..Default::default()
    }
}

fn build_graph() -> Result<BlockGraph> {
    let mut stage = BlockGraph::new();
    stage.try_add_block(
        Block::new([0, 0, -1], BlockKind::Port)
            .with_port_role(PortRole::Input)?
            .with_tag("q_in")?,
    )?;
    stage.try_add_block(Block::new([0, 0, 0], BlockKind::Cube(CubeKind::ZXZ)))?;
    stage.try_add_block(
        Block::new([0, 0, 1], BlockKind::Port)
            .with_port_role(PortRole::Output)?
            .with_tag("q_out")?,
    )?;
    stage.try_add_pipe(Pipe::new([0, 0, -1], Direction::ZPLUS))?;
    stage.try_add_pipe(Pipe::new([0, 0, 0], Direction::ZPLUS))?;
    let stage = BlockGraph::definition("Stage", stage, interface(-1, 1), vec![], vec![], vec![]);

    let mut graph = BlockGraph::from_definitions(vec![stage, BlockGraph::new()])?;
    graph.place_module("child0", "Stage")?;
    graph.place_module_with(
        "child1",
        "Stage",
        None,
        ModuleRotation::new(UDirection::Z, 2),
    )?;
    graph.connect_modules("child0.q_out", "child1.q_in")?;
    graph.validate()?;
    Ok(graph)
}
// [build-end]

fn main() -> Result {
    let graph = build_graph()?;
    // [exchange-start]
    let text = graph.to_blog_text();
    let restored = BlockGraph::from_text(&text)?;
    restored.validate()?;
    assert_eq!(restored.modules().count(), 2);

    std::fs::write("modular-memory.blog", &text)?;
    let loaded = BlockGraph::load("modular-memory.blog")?;
    loaded.validate()?;
    assert_eq!(loaded.to_blog_text(), restored.to_blog_text());
    // [exchange-end]

    compile(&loaded, 3)?.validate()?;
    Ok(())
}
