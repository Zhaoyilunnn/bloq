"""Compose the AND/MAJ/UMA bulk stage of a controlled adder."""

# [build-start]
import bloq


def build_graph():
    definitions = [
        bloq.BlockGraph.definition("AND", bloq.GalleryItem.CCZ_INJECTED_AND.load()),
        bloq.BlockGraph.definition("MAJ", bloq.GalleryItem.CCZ_INJECTED_MAJ.load()),
        bloq.BlockGraph.definition("UMA", bloq.GalleryItem.UMA.load()),
    ]
    graph = bloq.BlockGraph.from_definitions([*definitions, bloq.BlockGraph()])
    for index, name in enumerate(["AND", "MAJ", "UMA"]):
        graph.place_module(f"child{index}", name)
    graph.connect_modules("child0.qi_k", "child1.i_prime_k", compact=False)
    graph.connect_modules("child1.c_k_out", "child2.c_k")
    return graph
# [build-end]


if __name__ == "__main__":
    graph = build_graph()
    graph.validate()
    graph.save("module-controlled-adder.blog")
    graph.export_html_viewer("module-controlled-adder.html", module_view=True)
