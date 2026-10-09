# Adaptive QIR

`bloq_qir::emit_program_qir` emits QIR 2.1 Adaptive LLVM text and bitcode from a
`bloq_vm::Program`. The `bloq` facade exposes this API under its optional `qir`
feature. Install LLVM 21's `opt-21` and `llvm-as-21` to emit; ordinary Rust builds
and the default facade do not link LLVM or require those executables.

The emitter validates task operands and RUS isolation using the VM validator.
It constructs private classical registers, promotes them to SSA with
LLVM's `mem2reg`, verifies the module, and assembles bitcode. Both outputs use
an `i64` entry point, static resources, declared integer/loop/return capabilities,
and terminal Boolean output records with constant labels. Output ordering follows
`QirOptions::output_records`, then `output_bits`. Result aliases are captured as
classical values before a measurement slot is reused.

## Emit a compiled memory

Enable facade features `vm,qir`, compile the graph, lower once, and select the
raw classical readout to export:

```rust
use bloq::{graph::GalleryItem, qir::{QirOptions, emit_program_qir}, vm};

fn main() -> bloq::Result<()> {
    let ir = bloq::compile::compile(&GalleryItem::XMemory.build(), 3)?;
    let program = vm::lower(&ir, &vm::LoweringConfig::default())?;
    let raw = program.tasks.iter().find_map(|task| {
        matches!(task.instruction, vm::instruction::Instruction::Observable { .. })
            .then_some(task.output).flatten()
    }).ok_or_else(|| std::io::Error::other("memory has no observable"))?;
    let options = QirOptions { output_bits: vec![raw], ..QirOptions::default() };
    let artifact = emit_program_qir(&program, &options)?;
    std::fs::write("memory.ll", artifact.llvm_ir)?;
    std::fs::write("memory.bc", artifact.bitcode)?;
    Ok(())
}
```

This example exports the raw readout. A corrected readout additionally needs an
explicit decoder binding and target decoder configuration. VM mock-decoder
probabilities do not define a hardware syndrome model.

## Decoder contract

The emitter uses these external interfaces:

```llvm
declare void @reset_decoder_ui64(i64)
declare void @enqueue_syndromes_ui64(i64, i64, i64, i64)
declare i64 @get_corrections_ui64(i64, i64, i64)
declare i1 @decoder_ready_ui64(i64)
```

A `DecoderBinding` names the observable task, decoder ID, ordered syndrome
parities, correction-mask width and selected correction bit. Packet payloads
contain up to 64 bits, with the first parity in the least-significant position.
The target model must use exactly that bit order and complete window length.
Corrected and Flip requests for one observable share one submission and one
consumed correction mask. Corrected is raw XOR the selected flip.
The emitter masks the selected bit before converting it to a Boolean.

`decoder_ready_ui64` is nonblocking and non-consuming. A result is ready only
when an unread correction exists and that session has no outstanding work.
`get_corrections_ui64` remains blocking; its final argument selects consumption,
not blocking versus nonblocking behavior.

For an admitted `WaitFor` on pending decoder tasks, the emitter generates a
readiness loop with the authored protection circuits as its body. It completes
an in-flight round before checking readiness again. The compiler assigns device
operation timing. Protection-round exhaustion returns code 2 rather than an
accepted logical output.

A false activation skips the protection wait without discarding pending decoder
requests. Later consumers retrieve the result. A completed solve retains its
correction across branch joins, so subsequent requests for the same observable
do not consume the decoder result again. Conditional RUS entry follows the same
rule for decoder work submitted before the branch.

## Supported execution

| Behavior | Contract |
| --- | --- |
| Single-qubit Cliffords and Pauli-axis controlled gates | Decompose to H/S/X/Z/CX, preserving signed images |
| Pauli-axis T and adjoint T | Basis changes around T/T† |
| Signed multi-Pauli measurements | QND ancilla parity measurement, inverse basis changes and shared aliases |
| Reset, record-controlled Paulis and alternatives | Physical reset/preparation and explicit branches |
| Boolean evaluation and parity | Strict input validity, including unused Call arguments and both Select arms |
| Observable/readout recipes | Raw classical parity; no cloned-simulator boundary probes |
| Static memory | Execute the already-expanded stream once |
| Decoder pairs and protection waits | Explicit model binding, readiness loop and shared correction |
| Source-isolated RUS | Sequential retries, authored body resets, attempt-local validity and bounded termination |
| Discard/exhaustion/unavailable input | Nonzero entry status; no accepted output is emitted |

The initial backend rejects logical patch input/output tables, nonzero source
release times, explicit idle durations, nonzero noise, activated decoder requests,
nested RUS, explicit retry preparation, factory GAP timing, dynamic detector
frontiers, overlapping solves on one decoder session and protection waits with
unrelated pending decoder sessions. These need additional
lowering contracts. Independent tasks are emitted in dependency order; this does
not reproduce the VM's parallel timing model. Non-overlapping quantum alternatives
are checked before emission. Downstream resource limits remain independent of the
QIR format.

In particular, the current controller has 32 measurement-register indices. The
d3 X-memory example fits with 17 physical qubits, while the d3 CNOT/T galleries
use 65/59. A generated QIR module does not remove that hardware limit.

## End-to-end verification

Build the exporter from the Bloq workspace:

```sh
cargo build --locked -p bloq_qir --example export
```

Configure the compiler with `QSBIT_BLOQ_EXPORTER` pointing to the built exporter,
`QSBIT_SIM_EXECUTABLE` pointing to the simulator, and both
`QSBIT_TEST_QEC=ON` and `QSBIT_TEST_AER=ON`. Select
a Python interpreter with the simulator's QEC and Aer extras. Run registered
CTest `integration.bloq`.

That test invokes the Rust exporter and consumes its verified text and bitcode.
It checks Bloq-compiled d3 memory against an independent Stim and PyMatching
reference, all three single-error repetition-code corrections, extra protection
measurements during long decoder latency, one solve for paired decoder outputs,
numerical execution of S and T gates, signed QND products, early-restart
evaluation, retry and wait exhaustion, and program exit-status reporting.
Decoder regressions cover both activation paths for conditional waits and RUS
entry, repeated observable queries, output reassignment and multibit masks.
The required QIR CI job runs this compiler-to-simulator test.
Generated modules, traces and targets live in a temporary directory.
