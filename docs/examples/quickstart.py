"""Compile the gallery CNOT and dynamic T gate at code distance three."""

# [cnot-load-start]
from pathlib import Path

import bloq

cnot = bloq.GalleryItem.CNOT.load()
print(cnot.to_text())
# [cnot-load-end]

# [cnot-compile-start]
cnot_program = bloq.compile(cnot, distance=3)
cnot_program.save("cnot.bloqir")
# [cnot-compile-end]

Path("cnot.svg").write_text(cnot_program.to_svg(include_classical=True), encoding="utf-8")

# [cnot-emit-start]
circuit = bloq.emit_stim(cnot_program)
circuit.to_file("cnot.stim")
# [cnot-emit-end]

# [t-load-start]
t_gate = bloq.GalleryItem.T_GATE.load()
print(t_gate.to_text())
# [t-load-end]

# [t-compile-start]
t_program = bloq.compile(t_gate, distance=3)
t_program.save("t-gate.bloqir")
# [t-compile-end]

Path("t-gate.svg").write_text(t_program.to_svg(include_classical=True), encoding="utf-8")

# Keep the gallery source and explicitly audit the saved exchange files.
Path("quickstart-cnot.blog").write_text(
    cnot.to_text(), encoding="utf-8"
)
Path("quickstart-t.blog").write_text(
    t_gate.to_text(), encoding="utf-8"
)
for name, program in [("cnot", cnot_program), ("t-gate", t_program)]:
    restored = bloq.Bloq.load(f"{name}.bloqir")
    restored.validate()
    assert str(restored.stats()) == str(program.stats())
assert cnot_program.stats().is_static
assert not t_program.stats().is_static
try:
    bloq.emit_stim(t_program)
except bloq.StimEmissionError:
    pass
else:
    raise AssertionError("The dynamic T gate must require a dynamic backend")
