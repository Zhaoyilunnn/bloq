// [build-start]
use bloq::graph::{Action, Expr, MeasureTarget, SelectiveKind};
use bloq::prelude::*;

fn build_graph() -> Result<BlockGraph> {
    let mut graph = BlockGraph::new();
    for (position, kind) in [
        ([0, 0, 0], BlockKind::Port),
        ([0, 0, 1], BlockKind::Cube(CubeKind::XZX)),
        ([0, 0, 2], BlockKind::Port),
        ([1, 0, 0], BlockKind::T),
        ([1, 0, 1], BlockKind::Cube(CubeKind::XZX)),
        ([1, 0, 2], BlockKind::Selective(SelectiveKind::XY)),
    ] {
        graph.try_add_block(Block::new(position, kind))?;
    }
    for (position, direction) in [
        ([0, 0, 0], Direction::ZPLUS),
        ([0, 0, 1], Direction::ZPLUS),
        ([0, 0, 1], Direction::XPLUS),
        ([1, 0, 0], Direction::ZPLUS),
        ([1, 0, 1], Direction::ZPLUS),
    ] {
        graph.try_add_pipe(Pipe::new(position, direction))?;
    }
    graph.set_actions(vec![
        Action::Measure {
            target: MeasureTarget::Edge {
                src: IVec3::new(0, 0, 1),
                dir: Direction::XPLUS,
            },
            name: "mzz".into(),
        },
        Action::Resolve {
            target: IVec3::new(1, 0, 2),
            condition: Expr::Not(Box::new(Expr::Var("mzz".into()))),
        },
    ])?;
    graph.validate()?;
    Ok(graph)
}
// [build-end]

fn main() -> Result {
    let graph = build_graph()?;
    compile(&graph, 3)?.validate()?;
    Ok(())
}
