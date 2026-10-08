"""Build CCZ gate teleportation with three authored correction regions."""

# [build-start]
import bloq


def build_graph():
    graph = bloq.BlockGraph()
    for position, kind, role, tag in [
        ((0, 2, 0), "Port", "input", "CCZ_0"),
        ((1, 2, 0), "Port", "input", "CCZ_1"),
        ((2, 2, 0), "Port", "input", "CCZ_2"),
        ((0, 0, 1), "Port", "multiplex", "x"),
        ((0, 1, 1), "ZXX", "auto", ""),
        ((0, 2, 1), "ZXX", "auto", ""),
        ((1, 0, 1), "Port", "multiplex", "y"),
        ((1, 1, 1), "ZZX", "auto", ""),
        ((1, 2, 1), "ZXX", "auto", ""),
        ((2, 0, 1), "Port", "multiplex", "z"),
        ((2, 1, 1), "ZZX", "auto", ""),
        ((2, 2, 1), "ZXX", "auto", ""),
        ((1, 3, 1), "ZZX", "auto", ""),
        ((0, 3, 1), "XZX", "auto", ""),
        ((2, 3, 1), "ZXX", "auto", ""),
    ]:
        graph.add_block(bloq.Block(position, kind, role=role, tag=tag))
    for position, direction in [
        ((0, 2, 0), "+Z"),
        ((1, 2, 0), "+Z"),
        ((0, 0, 1), "+Y"),
        ((0, 1, 1), "+Y"),
        ((1, 0, 1), "+Y"),
        ((1, 1, 1), "+Y"),
        ((1, 2, 1), "+Y"),
        ((2, 0, 1), "+Y"),
        ((2, 1, 1), "+Y"),
        ((2, 2, 1), "-Z"),
        ((2, 2, 1), "+Y"),
        ((1, 3, 1), "-X"),
    ]:
        graph.add_pipe(bloq.Pipe(position, direction))

    graph.add_branches(
        [
            bloq.Branch(
                "b0", "m0x",
                on_false=bloq.BranchArm([((2, 3, 2), "X"), ((1, 2, 2), "X")]),
                on_true=bloq.BranchArm(
                    [((2, 3, 2), "ZXZ"), ((1, 2, 2), "ZXX"), ((1, 3, 2), "ZZX")],
                    pipes=[bloq.Pipe((2, 3, 2), "-X", hadamard=True),
                           bloq.Pipe((1, 3, 2), "-Y")],
                ),
            ),
            bloq.Branch(
                "b1", "m1y",
                on_false=bloq.BranchArm([((0, 1, 2), "X"), ((2, 2, 2), "X")]),
                on_true=bloq.BranchArm(
                    [((0, 1, 2), "ZXZ"), ((2, 2, 2), "ZXX"),
                     ((2, 1, 2), "ZZX"), ((1, 1, 2), "ZZX")],
                    pipes=[bloq.Pipe((2, 2, 2), "-Y"),
                           bloq.Pipe((2, 1, 2), "-X"),
                           bloq.Pipe((1, 1, 2), "-X", hadamard=True)],
                ),
            ),
            bloq.Branch(
                "b2", "m2z",
                on_false=bloq.BranchArm([((0, 3, 2), "X"), ((0, 2, 2), "X")]),
                on_true=bloq.BranchArm(
                    [((0, 3, 2), "XZZ"), ((0, 2, 2), "ZXX")],
                    pipes=[bloq.Pipe((0, 2, 2), "+Y", hadamard=True)],
                ),
            ),
        ],
        pipes=[bloq.Pipe(position, "+Z") for position in
               [(2, 3, 1), (1, 2, 1), (0, 1, 1), (2, 2, 1), (0, 3, 1), (0, 2, 1)]],
        actions=[bloq.Action.measure(bloq.MeasureTarget.edge((x, 0, 1), "+Y"), name)
                 for x, name in enumerate(["m0x", "m1y", "m2z"])],
    )
    m0x, m1y, m2z = (bloq.Expr.var(name) for name in ["m0x", "m1y", "m2z"])
    for position, condition in [
        ((2, 0, 1), m0x & m1y),
        ((1, 0, 1), m0x & m2z),
        ((0, 0, 1), m1y & m2z),
    ]:
        graph.add_action(bloq.Action.feedback([("Z", position)], condition=condition))
    graph.validate()
    return graph
# [build-end]


if __name__ == "__main__":
    graph = build_graph()
    graph.save("action-ccz-teleport.blog")
    bloq.compile(graph, distance=3).validate()
