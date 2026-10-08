# Existing Works

This chapter compares tools that compile surface code computations into physical
circuits for simulation. Bloq compiles authored geometry and classical control
into Bloq IR, which supports circuit emission and physical verification in its VM.

## Physical Circuit Compiler

Our study covers implemented tools that lower surface code operations or whole
computations into physical circuits that can be simulated. We compare their
input representations, supported physical building blocks, and measurement
dependent control. Layout optimizers, resource estimators, and standalone
simulators are outside this scope. Native hardware instruction generators are
included when their surface code circuits have been simulated.

| Tool | Starting point | Main output | Limitations |
| --- | --- | --- | --- |
| [Bloq](#what-bloq-adds) | Block graph + actions | Bloq IR | Adaptive execution requires a dynamic backend. |
| [TQEC](https://github.com/tqec/tqec)[^work-tqec] | Static block graph | Stim circuit | Static Clifford computation only. No built in Y basis, walking, or patch rotation blocks. |
| [CircLS](https://github.com/John-YuehanZhang/CircLS)[^work-circls] | Clifford QASM / PPMs | Stim circuit | Static Clifford computation only. PPM based compilation with ancilla routing overhead. |
| [Deltakit](https://github.com/Deltakit/deltakit-compile)[^work-deltakit] | Logical QEC assembly | Stim circuit | Physical patch lowering handles memory operations. Movement, rotation, and multi patch PPMs are unsupported. |
| [Loom](https://github.com/entropicalabs/el-loom)[^work-loom] | Code blocks + operations | Multiple circuit formats | Stim output is Clifford only. No built in T cultivation block. |
| [TISCC](https://github.com/ORNL-QCI/TISCC)[^work-tiscc] | Patch operations | Ion gates + transport schedule | Targets trapped ions, with static schedules and transversal Hadamard and Pauli operations. |

## What Bloq adds

Bloq focuses on **representing and compiling dynamic Clifford+T surface code
computations**. The [BLOG format](../graphs/blog.md) describes block graph
geometry together with [actions](../graphs/actions.md) for logical measurements,
classical variables, feedback, measurement selection, and structural branches.
This keeps the spacetime layout and classical control semantics in one source.

Its **physical building blocks** include Y basis initialization and measurement,
sliding and gliding, and patch rotations. [T blocks](../../bloq_compile/src/block/fixed_bulk/t/mod.rs#L1)
compile into physical cultivation and lattice surgery escape circuits, with
acceptance checks and retries retained in the executable program.

Bloq extends **signed correlation surfaces to dynamic block graphs**. It derives
logical readout recipes and Pauli byproduct corrections that depend on the
selected branches and measurement outcomes. These preserve consistent logical
observables, classical decisions, and corrected outputs during execution. See
[correlation surfaces](correlation-surfaces.md).

The output **[Bloq IR](../backends/ir.md)** specifies physical quantum circuits,
detectors, logical observables, classical variables, control flow, and execution
dependencies. It provides an explicit program contract for simulation and
hardware backend integration.

The **[Rust compiler](../../bloq_compile/src/lib.rs#L1)** is designed with
performance and scalability in mind, using reusable modules and shared physical
circuit templates.

## References

[^work-tqec]: A. Suau et al., [tqec: A Python package for topological quantum error correction](https://doi.org/10.21105/joss.09142), *JOSS* **11**, 9142 (2026). [Implementation](https://github.com/tqec/tqec).

[^work-circls]: J. Y. Zhang, [CircLS: Compiling Lattice Surgery to Physical Circuits with Dynamic Allocation](https://arxiv.org/abs/2608.23819), arXiv (2026). Sections 2.2 and 3 describe PPM lowering and allocation during compilation, and Section 6.1 describes Clifford proxies for T resources.

[^work-deltakit]: [Deltakit logical assembly documentation](https://deltakit-docs.riverlane.com/en/stable/deltakit_compile/logical_assembly_api/index.html) and [compiler](https://github.com/Deltakit/deltakit-compile). The inspected [patch lowering pass](https://github.com/Deltakit/deltakit-compile/blob/e242562f1387b7efdff23d2bc7ea351f8b6edd03/src/deltakit_compile/passes/patch_lowering/rotated_surface/patch_to_plaquettes.py#L419) explicitly rejects unsupported logical assembly operations. The [default pipeline](https://github.com/Deltakit/deltakit-compile/blob/e242562f1387b7efdff23d2bc7ea351f8b6edd03/src/deltakit_compile/passes/logical_assembly/pipeline.py#L69) contains no preceding movement or surgery decomposition.

[^work-loom]: Entropica Labs, [Loom documentation](https://loom-api-docs.entropicalabs.com/) and [implementation](https://github.com/entropicalabs/el-loom). Its [OpenQASM exporter](https://github.com/entropicalabs/el-loom/blob/4eae58039ab8d31236c728bcb26ddf4ed699c5e1/src/loom/executor/eka_to_qasm_converter.py#L73) supports T gates and classical branches. The [surface code operation library](https://github.com/entropicalabs/el-loom/blob/4eae58039ab8d31236c728bcb26ddf4ed699c5e1/src/loom_rotated_surface_code/applicator/rsc_applicator.py#L51) includes state injection, movement, and rotation, with no built in cultivation block.

[^work-tiscc]: T. LeBlond et al., [TISCC: A Surface Code Compiler and Resource Estimator for Trapped Ion Processors](https://arxiv.org/abs/2311.10687), *SC W* (2023). [Implementation](https://github.com/ORNL-QCI/TISCC), including [state injection](https://github.com/ORNL-QCI/TISCC/blob/1212aecf58b8cad7b3cd0a8069d596d147af37af/src/logicalqubit.cpp#L860) and [Stim export](https://github.com/ORNL-QCI/TISCC/blob/1212aecf58b8cad7b3cd0a8069d596d147af37af/include/TISCC/instruction.hpp#L157). The native instruction set includes a transversal Hadamard, while T states require injection.
