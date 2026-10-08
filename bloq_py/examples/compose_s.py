"""Compose the logical S-gate layout from blocks and pipes."""

# [build-start]
import bloq


def build_graph():
    graph = bloq.BlockGraph()
    for position, kind in [
        ((0, 0, 0), "Port"),
        ((0, 0, 1), "XZX"),
        ((0, 0, 2), "Port"),
        ((1, 0, 1), "XZX"),
        ((1, 0, 2), "Y"),
    ]:
        graph.add_block(bloq.Block(position, kind))
    for position, direction in [
        ((0, 0, 0), "+Z"),
        ((0, 0, 1), "+Z"),
        ((0, 0, 1), "+X"),
        ((1, 0, 1), "+Z"),
    ]:
        graph.add_pipe(bloq.Pipe(position, direction))
    graph.validate()
    return graph
# [build-end]
