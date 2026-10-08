"""Module authoring retains native source contracts and independent definitions."""

from pathlib import Path
import pickle
import runpy

import bloq
import pytest


@pytest.mark.parametrize("name", ["module_three_cnots", "module_feedforward", "module_memory"])
def test_api_module_examples_compile_and_preserve_hierarchy(name):
    graph = runpy.run_path(str(Path(__file__).parents[1] / "examples" / f"{name}.py"))["build_graph"]()
    text = graph.to_text()
    canonical = bloq.BlockGraph.from_text(text).to_text()
    assert bloq.BlockGraph.from_text(canonical).to_text() == canonical
    assert pickle.loads(pickle.dumps(graph)).to_text() == canonical
    bloq.compile(graph, distance=3).validate()
    assert graph.to_text() == text
    assert graph.has_module_structure
    if name == "module_three_cnots":
        assert graph.module_names == ["CNOT", "main"]
        assert text.count(": CNOT @") == 3
        flat = graph.flatten()
        assert not any(pipe.hadamard for pipe in flat.pipes())
        assert all(str(block.kind) in {"Port", "ZXZ", "ZXX"} for block in flat.blocks())
    if name == "module_feedforward":
        read = graph.module("ReadParity").flatten()
        measurement = next(row for row in read.stabilizers() if row.measurement_name == "mz")
        assert measurement.stabilizer.port_stabilizer == {
            (0, 0, 0): bloq.Pauli.Z, (1, 1, 0): bloq.Pauli.Z,
        }
        dag = graph.analyze_action_graph()
        assert any(reason == "Classical" for _, _, reason in dag.dependencies())
        assert {path for _, path in dag.owners()} >= {"read", "correct"}



def test_controlled_adder_example_matches_bulk_stage_fixture():
    graph = runpy.run_path(str(Path(__file__).parents[1] / "examples/module_controlled_adder.py"))["build_graph"]()
    graph.validate()
    text = graph.to_text()
    assert graph.module_names == ["AND", "MAJ", "UMA", "main"]
    assert all(f"child{index}: {name} @" in text for index, name in enumerate(["AND", "MAJ", "UMA"]))
    canonical = bloq.BlockGraph.from_text(text).to_text()
    assert bloq.BlockGraph.from_text(canonical).to_text() == canonical
    flat = graph.flatten()
    reference = bloq.BlockGraph.load(Path(__file__).parents[2] / "bloq_test/assets/one_bit_adder.blog").flatten()
    assert sorted((block.pos, str(block.kind)) for block in flat.blocks()) == sorted(
        (block.pos, str(block.kind)) for block in reference.blocks()
    )
    assert sorted((pipe.src, pipe.dst, pipe.hadamard) for pipe in flat.pipes()) == sorted(
        (pipe.src, pipe.dst, pipe.hadamard) for pipe in reference.pipes()
    )
    actions = [str(action) for action in flat.actions()]
    for index, name in enumerate(["and", "maj", "uma"]):
        actions = [action.replace(f"child{index}__", f"{name}__") for action in actions]
    assert actions == [str(action) for action in reference.actions()]


def test_definition_port_order_resources_and_copying():
    body = bloq.GalleryItem.CNOT.load().flatten()
    before = body.to_text()
    module = bloq.BlockGraph.definition(
        "main", body,
        inputs={"target": (1, 1, 0), "control": (0, 0, 0)},
        outputs={"target_out": (1, 1, 3), "control_out": (0, 0, 3)},
        resources={"target": "ancilla", "target_out": "ancilla"},
    )
    graph = bloq.BlockGraph.from_definitions([module])
    text = graph.to_text()
    assert text.index("in target: ancilla") < text.index("in control: data")
    assert text.index("out target_out: ancilla") < text.index("out control_out: data")
    module.remove_block((0, 0, 1))
    assert graph.to_text() == text and body.to_text() == before
    with pytest.raises(bloq.BlockGraphError):
        bloq.BlockGraph.from_definitions([module])
    with pytest.raises(bloq.BlockGraphError):
        bloq.BlockGraph.from_definitions([graph], limits={"max_expanded_blocks": 0})


def test_invalid_construction_does_not_edit_the_parent_or_body():
    graph = bloq.BlockGraph()
    before = graph.to_text()
    for operation in [
        lambda: graph.add_instance("a", "Gate", rotation=("Z", 45)),
        lambda: graph.connect("missing-dot", "a.q"),
        lambda: graph.connect("a.q.extra", "b.q"),
        lambda: graph.connect((0, 0, 0), (0, 0, 1)),
        lambda: graph.bind("a.m", "missing-dot"),
        lambda: bloq.BlockGraph.definition("main", graph, resources={"missing": "data"}),
    ]:
        with pytest.raises(bloq.InvalidArgumentError):
            operation()
        assert graph.to_text() == before
    graph.add_instance("a", "Missing")
    authored = graph.to_text()
    with pytest.raises(bloq.BlockGraphError):
        bloq.BlockGraph.from_definitions([graph])
    assert graph.to_text() == authored


def test_convenient_module_edits_are_atomic_and_move_connected_groups():
    cnot = bloq.BlockGraph.definition("CNOT", bloq.GalleryItem.CNOT.load())
    graph = bloq.BlockGraph.from_definitions([cnot, bloq.BlockGraph()])
    for index in range(2):
        graph.place_module(f"child{index}", "CNOT")
    assert graph.connect_modules("child0.control_out", "child1.control_in") == 2
    before = graph.to_text()
    for output, input in [("child0.control_in", "child1.target_out"), ("unknown.port", "child1.target_in")]:
        with pytest.raises(bloq.BloqError):
            graph.connect_modules(output, input)
        assert graph.to_text() == before
    assert graph.translate_module("child0", (0, 3, 0)) == (0, 3, 0)
    assert {block.pos[1] for block in graph.flatten().blocks()} == {3, 4}


def test_automatic_classical_binding_rejects_bad_sources_atomically():
    graph = runpy.run_path(str(Path(__file__).parents[1] / "examples/module_feedforward.py"))["build_graph"]()
    before = graph.to_text()
    assert "in correct_flip" not in before
    with pytest.raises(bloq.BloqError):
        graph.bind_modules("unknown.bit", "correct.flip")
    assert graph.to_text() == before


@pytest.mark.parametrize("replace_actions", [False, True])
def test_definition_retains_child_values_in_parent_actions(replace_actions):
    graph = bloq.BlockGraph.from_text("""BLOG 1.0
module Read {
  in q: data = 0
  out result = m
  0: Port [0, 0, 0] role=input
  1: ZXZ [0, 0, 1]
  2: Z [0, 0, 2]
  0 -> +Z
  1 -> +Z
  m = measure 2
}
module main {
  in q: data = 0
  read: Read @ [0, 0, 0]
  0: Port [0, 0, 0] role=input
  0 -> read.q
  ready = read.result
}
""")
    actions = [getattr(bloq.Action, "let")("replacement", "read.result")] if replace_actions else None
    copied = bloq.BlockGraph.definition("main", graph, actions=actions)
    expected = graph.to_text().replace("ready =", "replacement =") if replace_actions else graph.to_text()
    assert copied.to_text() == expected
    assert copied.module_names == graph.module_names
    assert bloq.BlockGraph.from_text(copied.to_text()).to_text() == expected
    copied.validate()
    bloq.compile(copied, distance=3).validate()


@pytest.mark.parametrize("consumer", ["", "local_use = correct_flip", "out mirror = correct_flip"])
def test_classical_binding_prunes_only_unused_inputs_after_roundtrip(consumer):
    original = runpy.run_path(str(Path(__file__).parents[1] / "examples/module_feedforward.py"))["build_graph"]()
    canonical = bloq.BlockGraph.from_text(original.to_text()).to_text()
    source = canonical.replace("module main {", f"module main {{\n  in correct_flip\n  {consumer}")
    source = source.replace("read.result => correct.flip", "correct_flip => correct.flip")
    graph = bloq.BlockGraph.from_text(source)
    graph.bind_modules("read.result", "correct.flip")
    graph.validate()
    assert ("in correct_flip" in graph.to_text()) == bool(consumer)
    if not consumer:
        assert graph.to_text() == canonical
        bloq.compile(graph, distance=3).validate()
