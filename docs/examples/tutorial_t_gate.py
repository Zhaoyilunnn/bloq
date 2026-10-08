"""Compile and execute a logical T gate with ideal decoder decisions."""

import math
from pathlib import Path

import bloq

# [source-start]
graph = bloq.GalleryItem.T_GATE.load()
graph.validate()
graph.save("t-gate.blog")
print(graph.to_text())
# [source-end]

# [compile-start]
ir = bloq.compile(graph, distance=3)
ir.validate()
ir.save("t-gate.bloqir")
Path("t-gate.svg").write_text(ir.to_svg(include_classical=True), encoding="utf-8")
# [compile-end]

# [execute-start]
program = bloq.lower_vm(ir, noise=0.0, decoder_latency_rounds=3)
result = program.run(
    seed=17, input_state="plus", decoder_acceptance=1.0,
    accepted_accuracy=1.0, rejected_accuracy=1.0,
)
assert not result.discarded
expected = (math.sqrt(0.5), math.sqrt(0.5), 0.0)
actual = result.logical_bloch()
assert all(math.isclose(a, b, abs_tol=1e-9) for a, b in zip(actual, expected))
Path("t-gate.trace.json").write_text(result.to_json(indent=2), encoding="utf-8")
print("logical Bloch vector:", actual)
# [execute-end]

# [retry-start]
rejected = program.run(
    seed=18, accepted_accuracy=0.0, rejected_accuracy=0.0, max_attempts=1,
)
assert rejected.discarded
print("attempt-limited shot discarded:", rejected.discarded)
# [retry-end]
