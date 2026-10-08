"""Build a logical T gate from blocks, pipes, and structured actions."""

# [build-start]
import bloq


def build_graph():
    graph = bloq.BlockGraph()
    for position, kind in [
        ((0, 0, 0), "Port"),
        ((0, 0, 1), "XZX"),
        ((0, 0, 2), "Port"),
        ((1, 0, 0), "T"),
        ((1, 0, 1), "XZX"),
        ((1, 0, 2), "XY"),
    ]:
        graph.add_block(bloq.Block(position, kind))
    for position, direction in [
        ((0, 0, 0), "+Z"),
        ((0, 0, 1), "+Z"),
        ((0, 0, 1), "+X"),
        ((1, 0, 0), "+Z"),
        ((1, 0, 1), "+Z"),
    ]:
        graph.add_pipe(bloq.Pipe(position, direction))
    graph.set_actions([
        bloq.Action.measure(bloq.MeasureTarget.edge((0, 0, 1), "+X"), "mzz"),
        bloq.Action.resolve((1, 0, 2), ~bloq.Expr.var("mzz")),
    ])
    graph.validate()
    return graph
# [build-end]


if __name__ == "__main__":
    graph = build_graph()
    graph.save("action-t-gate.blog")
    bloq.compile(graph, distance=3).validate()
