"""Build two memory stages with one shared module definition."""

# [build-start]
import bloq


def build_graph():
    body = bloq.BlockGraph()
    body.add_block(bloq.Block((0, 0, -1), "Port", role="input", tag="q_in"))
    body.add_block(bloq.Block((0, 0, 0), "ZXZ"))
    body.add_block(bloq.Block((0, 0, 1), "Port", role="output", tag="q_out"))
    body.add_pipe(bloq.Pipe((0, 0, -1), "+Z"))
    body.add_pipe(bloq.Pipe((0, 0, 0), "+Z"))
    stage = bloq.BlockGraph.definition(
        "Stage", body, inputs={"q_in": (0, 0, -1)}, outputs={"q_out": (0, 0, 1)},
    )
    graph = bloq.BlockGraph.from_definitions([stage, bloq.BlockGraph()])
    graph.place_module("child0", "Stage")
    graph.place_module("child1", "Stage", rotation=("Z", 180))
    graph.connect_modules("child0.q_out", "child1.q_in")
    return graph
# [build-end]


if __name__ == "__main__":
    graph = build_graph()
    # [exchange-start]
    text = graph.to_text()
    restored = bloq.BlockGraph.from_text(text)
    restored.validate()
    assert set(restored.module_names) == {"main", "Stage"}

    graph.save("modular-memory.blog")
    loaded = bloq.BlockGraph.load("modular-memory.blog")
    loaded.validate()
    assert loaded.to_text() == restored.to_text()
    # [exchange-end]

    bloq.compile(loaded, distance=3).validate()
