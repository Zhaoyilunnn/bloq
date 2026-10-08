"""Reuse one CNOT definition three times on the same two logical qubits."""

# [build-start]
import bloq


def build_graph():
    cnot = bloq.BlockGraph.definition("CNOT", bloq.GalleryItem.CNOT.load())
    graph = bloq.BlockGraph.from_definitions([cnot, bloq.BlockGraph()])
    for index in range(3):
        graph.place_module(f"child{index}", "CNOT")
    graph.connect_modules("child0.control_out", "child1.control_in")
    graph.connect_modules("child1.control_out", "child2.control_in")
    return graph
# [build-end]


if __name__ == "__main__":
    graph = build_graph()
    graph.save("module-three-cnots.blog")
    bloq.compile(graph, distance=3).validate()
