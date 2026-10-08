"""Feed a CNOT target measurement into a correction on its surviving control."""

# [build-start]
import bloq


def build_graph():
    read = bloq.GalleryItem.CNOT.load().flatten()
    read.remove_block((1, 1, 3))
    read.add_block(bloq.Block((1, 1, 3), "Z"))
    read.add_pipe(bloq.Pipe((1, 1, 2), "+Z"))
    read.set_actions([bloq.Action.measure((1, 1, 3), "mz")])
    read = bloq.BlockGraph.definition(
        "ReadParity", read,
        inputs={"control": (0, 0, 0), "target": (1, 1, 0)},
        outputs={"control_out": (0, 0, 3)}, bit_outputs={"result": "mz"},
    )

    correct = bloq.BlockGraph()
    correct.add_block(bloq.Block((0, 0, 0), "Port", role="input"))
    correct.add_block(bloq.Block((0, 0, 1), "ZXZ"))
    correct.add_block(bloq.Block((0, 0, 2), "Port", role="output"))
    correct.add_pipe(bloq.Pipe((0, 0, 0), "+Z"))
    correct.add_pipe(bloq.Pipe((0, 0, 1), "+Z"))
    correct = bloq.BlockGraph.definition(
        "Correct", correct, inputs={"q_in": (0, 0, 0)},
        outputs={"q_out": (0, 0, 2)}, bit_inputs=["flip"],
        actions=[bloq.Action.feedback([("X", (0, 0, 1))], condition="flip")],
    )

    graph = bloq.BlockGraph.from_definitions([read, correct, bloq.BlockGraph()])
    graph.place_module("read", "ReadParity")
    graph.place_module("correct", "Correct")
    graph.connect_modules("read.control_out", "correct.q_in")
    graph.bind_modules("read.result", "correct.flip")
    return graph
# [build-end]


if __name__ == "__main__":
    graph = build_graph()
    graph.save("module-feedforward.blog")
    bloq.compile(graph, distance=3).validate()
