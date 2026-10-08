# bloq_lassynth

An optional companion Rust port of
[Python LaSsynth](https://github.com/quantumlib/Stim/tree/main/glue/lattice_surgery)
for small, fixed-box lattice-surgery layout searches. Bloq's core compiler
consumes block graphs and module programs; this port is a separate authoring aid.

```rust
use std::time::Duration;

use bloq_lassynth::{ComponentOptions, Port, synthesize_qasm};
use bloq_utils::{Direction, UDirection};
use glam::IVec3;

let port = |position, direction| Port::new(position, direction, UDirection::Y);
let options = ComponentOptions::new(
    IVec3::new(2, 2, 3),
    [
        port(IVec3::new(1, 0, -1), Direction::ZPLUS),
        port(IVec3::new(0, 1, -1), Direction::ZPLUS),
    ],
    [
        port(IVec3::new(1, 0, 3), Direction::ZMINUS),
        port(IVec3::new(0, 1, 3), Direction::ZMINUS),
    ],
    Duration::from_secs(10),
);
let graph = synthesize_qasm("OPENQASM 2.0; qreg q[2]; cx q[0],q[1];", &options)?;
assert_eq!(graph.blocks().filter(|block| block.kind().is_port()).count(), 4);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`ComponentOptions` assigns spatial roles from its input/output lists. Direct
`SynthesisProblem` spatial ports must call `Port::with_role` explicitly.
Temporal roles remain geometry-inferred. Port blocks sit one cell outside the
fixed box and point inward. The box dimensions count only synthesis cells.

QuiZX simplifies the whole Clifford+T component. Odd phases become magic-state
parity records and adaptive X/Y measurements, leaving one Clifford surface
table. One SAT solve maps that table into the caller's exact box and
fixed logical ports. A named record column binds every T parity to one physical
spacelike edge. Source ZX vertices and edges are not placed.

Rotation parameters must be exact multiples of pi/4. Expressions support
integers, `pi`, addition, subtraction, negation, and rational scaling/division.
Decimal literals, functions, nonlinear pi arithmetic, and conditionals are rejected.

Leading `reset` and terminal measurement are plugged into QuiZX as ordinary Z0
spiders before full simplification. They may fuse or disappear and are never
ports. `ComponentOptions` contains only the surviving open boundaries.
Components whose fixed boundaries select a zero map are rejected.
Generated T/selective terminals use free temporal boundary sites.
Synthesis is native-only and uses Kissat.
Cancellation and timeout are checked before each phase and throughout SAT
allocation, encoding, and loading. Parsing, QuiZX simplification, and graph
reconstruction/analysis are synchronous and checked between calls, so the limit is cooperative, not a hard
bound on total return time.

Spatial Hadamard pipes are forbidden by default. Opt in with
`ComponentOptions::with_spatial_hadamard(true)`. Synthesis accepts a result
after stabilizer and target checks. Call `BlockGraph::validate` for a full
graph audit. Compiler preflight is a separate operation.

`SynthesisProblem` and `synthesize_with_timeout` expose the lower-level
fixed-volume Clifford correlation-surface solver.
