// [build-start]
use bloq::graph::{Expr, parse_actions};
use bloq::prelude::*;

fn build_graph() -> Result<BlockGraph> {
    let mut graph = BlockGraph::new();
    for (position, kind, role, tag) in [
        ([0, 2, 0], "Port", PortRole::Input, "CCZ_0"),
        ([1, 2, 0], "Port", PortRole::Input, "CCZ_1"),
        ([2, 2, 0], "Port", PortRole::Input, "CCZ_2"),
        ([0, 0, 1], "Port", PortRole::Multiplex, "x"),
        ([0, 1, 1], "ZXX", PortRole::Auto, ""),
        ([0, 2, 1], "ZXX", PortRole::Auto, ""),
        ([1, 0, 1], "Port", PortRole::Multiplex, "y"),
        ([1, 1, 1], "ZZX", PortRole::Auto, ""),
        ([1, 2, 1], "ZXX", PortRole::Auto, ""),
        ([2, 0, 1], "Port", PortRole::Multiplex, "z"),
        ([2, 1, 1], "ZZX", PortRole::Auto, ""),
        ([2, 2, 1], "ZXX", PortRole::Auto, ""),
        ([1, 3, 1], "ZZX", PortRole::Auto, ""),
        ([0, 3, 1], "XZX", PortRole::Auto, ""),
        ([2, 3, 1], "ZXX", PortRole::Auto, ""),
    ] {
        let mut block = Block::new(position, kind.parse::<BlockKind>()?).with_tag(tag)?;
        if block.kind().is_port() {
            block = block.with_port_role(role)?;
        }
        graph.try_add_block(block)?;
    }
    for (position, direction) in [
        ([0, 2, 0], Direction::ZPLUS),
        ([1, 2, 0], Direction::ZPLUS),
        ([0, 0, 1], Direction::YPLUS),
        ([0, 1, 1], Direction::YPLUS),
        ([1, 0, 1], Direction::YPLUS),
        ([1, 1, 1], Direction::YPLUS),
        ([1, 2, 1], Direction::YPLUS),
        ([2, 0, 1], Direction::YPLUS),
        ([2, 1, 1], Direction::YPLUS),
        ([2, 2, 1], Direction::ZMINUS),
        ([2, 2, 1], Direction::YPLUS),
        ([1, 3, 1], Direction::XMINUS),
    ] {
        graph.try_add_pipe(Pipe::new(position, direction))?;
    }
    graph.try_add_branches(
        [
            Branch::new(
                "b0",
                Expr::Var("m0x".into()),
                BranchArm::try_from_blocks([([2, 3, 2], "X"), ([1, 2, 2], "X")])?,
                BranchArm::try_from_blocks([
                    ([2, 3, 2], "ZXZ"),
                    ([1, 2, 2], "ZXX"),
                    ([1, 3, 2], "ZZX"),
                ])?
                .with_pipes([
                    Pipe::new([2, 3, 2], Direction::XMINUS).with_hadamard(),
                    Pipe::new([1, 3, 2], Direction::YMINUS),
                ]),
            ),
            Branch::new(
                "b1",
                Expr::Var("m1y".into()),
                BranchArm::try_from_blocks([([0, 1, 2], "X"), ([2, 2, 2], "X")])?,
                BranchArm::try_from_blocks([
                    ([0, 1, 2], "ZXZ"),
                    ([2, 2, 2], "ZXX"),
                    ([2, 1, 2], "ZZX"),
                    ([1, 1, 2], "ZZX"),
                ])?
                .with_pipes([
                    Pipe::new([2, 2, 2], Direction::YMINUS),
                    Pipe::new([2, 1, 2], Direction::XMINUS),
                    Pipe::new([1, 1, 2], Direction::XMINUS).with_hadamard(),
                ]),
            ),
            Branch::new(
                "b2",
                Expr::Var("m2z".into()),
                BranchArm::try_from_blocks([([0, 3, 2], "X"), ([0, 2, 2], "X")])?,
                BranchArm::try_from_blocks([([0, 3, 2], "XZZ"), ([0, 2, 2], "ZXX")])?
                    .with_pipes([Pipe::new([0, 2, 2], Direction::YPLUS).with_hadamard()]),
            ),
        ],
        [
            [2, 3, 1],
            [1, 2, 1],
            [0, 1, 1],
            [2, 2, 1],
            [0, 3, 1],
            [0, 2, 1],
        ]
        .map(|position| Pipe::new(position, Direction::ZPLUS)),
        parse_actions(
            r"m0x = measure [0, 0, 1] -> +Y
m1y = measure [1, 0, 1] -> +Y
m2z = measure [2, 0, 1] -> +Y
feedback Z [2, 0, 1] if m0x & m1y
feedback Z [1, 0, 1] if m0x & m2z
feedback Z [0, 0, 1] if m1y & m2z",
            |_, _| None,
        )?,
    )?;
    graph.validate()?;
    Ok(graph)
}
// [build-end]

fn main() -> Result {
    let graph = build_graph()?;
    compile(&graph, 3)?.validate()?;
    Ok(())
}
