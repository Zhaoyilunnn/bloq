# Classical data and evaluation

This work implements shared classical storage and evaluation before changing
module-transfer algebra. Source planning, signed correlations and causal readout
selection keep their current semantics.

## Literature and applicable lessons

| Source | Mechanism | Application to Bloq |
| --- | --- | --- |
| [Deq](https://github.com/microsoft/qdk-ec/tree/main/deq), inspected at `d3085b431d09b4c9ab90158fc00ff52fd0570e2b` | Gadget definitions and instances are separate. The Rust tracker loads correction/readout matrices, waits for raw and decoded inputs, and applies them to runtime frames. | Share immutable classical definitions; keep readiness and shot state outside them. Its current sparse-to-dense conversion and recursive propagation are implementation choices, not a scalability proof to copy. |
| [CUDA-Q Logical](https://arxiv.org/html/2609.13388v1), especially B.4–B.6 and D.3 | Physical records have stable identities; detector data accompanies executable operations. Compact plans retain dependencies, ownership and provenance. | Moving data out of graph nodes must preserve owner identity and decoder/scheduling contracts. Stored and executed work need separate metrics. |
| [CUDA-Q compiler IRs](https://nvidia.github.io/cuda-quantum/latest/using/extending/compiler/cudaq_ir.html) | Classical computation, quantum operations and lower-level execution coexist in a progressively lowered representation. | Reuse Bloq's existing classical semantics instead of introducing a source language merely for storage. |
| [HUGR](https://quantum-compilers.github.io/iwqc2024/papers/IWQC2024_paper_14.pdf) | Typed dataflow, nested graphs and explicit function references distinguish definitions from invocations. | A shared function body does not merge its invocations or their inputs. Quantum ownership remains separate from copyable classical data. |
| [MLIR resources](https://mlir.llvm.org/docs/Dialects/Builtin/#denseresourceelementsattr) and [sparse tensors](https://mlir.llvm.org/docs/Dialects/SparseTensorOps/) | Operations can reference separately stored immutable data; storage encoding is distinct from mathematical meaning. | Intern exact payloads, retain ordered occurrences and serialize shared data once. |
| [Cranelift](https://cranelift.dev/) and its [list pools](https://docs.rs/cranelift-entity/latest/cranelift_entity/struct.ListPool.html) | Dense identifiers and pooled data reduce allocation overhead; executable code generation is a separate service. | Start with the existing `Arc`, vectors and `rustc-hash`. A JIT is useful only if measured evaluation cost justifies it. |

Deq is an informative runtime design, not a substitute for Bloq's complete signed
source relation. Its [frame tracker](https://github.com/microsoft/qdk-ec/blob/d3085b431d09b4c9ab90158fc00ff52fd0570e2b/deq/deq_runtime/src/misc/pauli_frame_tracker.rs)
and [matrix conversion](https://github.com/microsoft/qdk-ec/blob/d3085b431d09b4c9ab90158fc00ff52fd0570e2b/deq/deq_runtime/src/misc/bit_matrix.rs)
were read directly. No Deq code is copied into this implementation.

## First implementation

1. Share immutable classical payloads, independently of node activation,
   provenance, edges and outputs. Identical expression bodies or exact ordered
   record/operator payloads can share storage even when their invocations have
   different inputs or guards. Mutable access detaches only the edited payload.
2. Encode definitions once per graph level in binary IR and refer to them from
   nodes. Loading restores sharing. Human-readable IR keeps its inspectable
   operation spelling. Region boundaries and node ids remain unchanged.
3. Lower reusable Boolean bodies to shared VM functions with invocation-local
   register bindings. Evaluate every argument, including unused arguments, unselected arms
   and cancelling parity occurrences. Values and timestamps remain per attempt;
   only code and data are shared. Reuse exact bound measurement parities too.

This first step deliberately keeps the dependency graph and source planner.
It tests whether sharing reduces stored bytes, preparation allocations and
evaluation overhead without changing causality. It makes no bounded-compilation
claim. Splitting payloads into shared local chunks is a subsequent storage
iteration if whole-payload pooling leaves material duplication. Composed local
transfer operations are a later algorithmic stage, after these contracts pass.

## Verification and measurements

Require unchanged canonical text, exact binary round trips including shared
definitions, independent copy-on-write edits, pin/slice/memory behavior, strict
unknown-input propagation, timestamps, activation, retries and decoder pairing.
Run physical fidelity and the cross-crate CI checks. Compare frozen binaries in
separate Cargo targets for cold compile, encode/decode, VM preparation, stored
payloads and peak RSS. Report graph counts separately; fewer payload copies do
not imply fewer operations or edges.

Three alternating fresh-process pairs on an Apple M5 Pro, macOS 27.0.1, used
matching empty-feature release builds at d3. The 128-bit adder medians were:

| Metric | Before | Shared definitions |
| --- | ---: | ---: |
| Cold compilation | 6.173 s | 6.209 s |
| Binary size | 30,062,046 bytes | 19,619,571 bytes |
| Binary encoding | 35.53 ms | 28.13 ms |
| Binary decoding | 93.24 ms | 82.59 ms |
| Distinct classical definitions | 609,463 | 12,360 |
| Stored readout terms | 3,934,967 | 599,275 |
| Peak process RSS | 2.138 GB | 2.177 GB |

This reduces binary size 34.7% and stored readout terms 84.8%, with encoding
20.8% faster and decoding 11.4% faster. Compilation is 0.6% slower and peak RSS
1.8% higher in this pass; no compile-time or peak-memory improvement is claimed.
RSS includes the complete probe, including codecs and optional text comparison.
At 33/64 bits the binary shrinks from 2.46/9.44 MB to 1.82/6.30 MB.
Tiny CNOT/T/THTH binaries grow by 2/11/16 bytes from the pool overhead.

The classical-only VM probe uses 50,000 independent invocations and 50 shots,
checking every output against its known Boolean result. Median preparation falls
from 32.50 to 29.17 ms (10.2%), execution from 551.76 to 407.89 ms (26.1%), and
process RSS from 314.1 to 289.0 MB (8.0%). This isolates interpreter allocation
and evaluation; it is not a physical-workload speedup. Supported CNOT/T/THTH
preparation is also exercised, without inferring a speedup from sub-millisecond
samples. General unpinned adders and phase-gradient preparation report their
existing typed limitations instead of producing timing claims.

The initial implementation reduced payloads but made encoding slower by repeatedly
hashing shared bodies. The retained iteration uses temporary allocation-identity
lookups within serialization and content keys only for new definitions. Canonical
output remains independent of allocation identity. Boolean emission shares its
small body vocabulary immediately, and optimization avoids detaching bodies whose
affine children are already compact. Review also removed repeated recursive
interning during nested JSON decoding.

All six paired compile cases retain byte-identical canonical text. Exact binary
round trips, malformed references, copy-on-write isolation, strict unknown inputs,
unused function arguments, timestamps and fresh evaluation are covered by focused
checks. Generated samples, hashes and source snapshots remain local under
`target/classical-ir-storage/`; the reusable probes are
`profile_classical_storage` and `profile_classical_eval`.

Validation passed: `rtk just ci` (including Rust, Python and documentation checks),
`rtk just fidelity` (17 exact cases and the native Choi suite), and
`rtk just py-stub` (no generated API changes). The measurements precede the
final cleanup, which avoids detaching unchanged classical payloads during
observable remapping and memory edits.

The existing ecosystem already supplies the required ownership, hashing and
encoding primitives. `binar` is already used by Bloq's leaf algebra and is suited
to dense binary algebra when needed. Neither a floating-point sparse-matrix
package nor a new JIT dependency is required to share exact sparse parity lists.
