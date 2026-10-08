// [build-start]
use bloq::prelude::*;

fn build_graph() -> Result<BlockGraph> {
    let mut graph = BlockGraph::new();
    for (position, kind) in [
        ([0, 0, 0], BlockKind::Port),
        ([0, 0, 1], BlockKind::Cube(CubeKind::XZX)),
        ([0, 0, 2], BlockKind::Port),
        ([1, 0, 1], BlockKind::Cube(CubeKind::XZX)),
        ([1, 0, 2], BlockKind::Y),
    ] {
        graph.try_add_block(Block::new(position, kind))?;
    }
    for (position, direction) in [
        ([0, 0, 0], Direction::ZPLUS),
        ([0, 0, 1], Direction::ZPLUS),
        ([0, 0, 1], Direction::XPLUS),
        ([1, 0, 1], Direction::ZPLUS),
    ] {
        graph.try_add_pipe(Pipe::new(position, direction))?;
    }
    graph.validate()?;
    Ok(graph)
}
// [build-end]

fn main() -> Result {
    let _graph = build_graph()?;
    Ok(())
}
