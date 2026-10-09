//! Standard QIR 2.1 Adaptive emission from typed Bloq VM programs.
//!
//! The emitter retains classical control and uses the decoder's external
//! reset, enqueue, correction and nonblocking readiness interfaces. Device
//! durations are assigned downstream; nonzero source releases and explicit
//! idle intervals are rejected. LLVM 21 tools promote private construction
//! registers to SSA and verify the final module before returning text/bitcode.
mod emit;
mod quantum;

use std::path::PathBuf;

use bloq_vm::instruction::{BitId, MemoryCycle, RecordId, RecordParity, TaskId};

/// Binding of one observable to a configured syndrome decoder.
#[derive(Debug, Clone)]
pub struct DecoderBinding {
    /// Observable task whose Corrected and Flip requests share a solve.
    pub observable: TaskId,
    /// Runtime decoder identifier, independent of the emitter's name.
    pub decoder: u32,
    /// Ordered syndrome bits, packed least-significant bit first.
    pub syndrome: Box<[RecordParity]>,
    /// Width of the configured decoder's correction mask (1–64).
    pub correction_count: u32,
    /// Correction-mask bit for this observable.
    pub correction_bit: u32,
}

/// Options for standard Adaptive emission.
#[derive(Debug, Clone)]
pub struct QirOptions {
    /// Explicit model/window bindings; the VM's stochastic mock is not exported.
    pub decoders: Vec<DecoderBinding>,
    /// Terminal classical bit outputs, in recording order.
    pub output_bits: Vec<BitId>,
    /// Terminal measurement outputs, captured before result-slot reuse.
    pub output_records: Vec<RecordId>,
    /// Bound on source-isolated retries; exhaustion returns exit code 1.
    pub max_attempts: u32,
    /// Bound on protection rounds; exhaustion returns exit code 2.
    pub max_wait_rounds: u32,
    /// LLVM 21 optimizer used for SSA promotion and module verification.
    pub optimizer: PathBuf,
    /// LLVM 21 assembler used for verified bitcode output.
    pub assembler: PathBuf,
}

impl Default for QirOptions {
    fn default() -> Self {
        Self {
            decoders: Vec::new(),
            output_bits: Vec::new(),
            output_records: Vec::new(),
            max_attempts: 100,
            max_wait_rounds: 100_000,
            optimizer: "opt-21".into(),
            assembler: "llvm-as-21".into(),
        }
    }
}

/// Verified standard Adaptive module and its resource requirements.
#[derive(Debug, Clone)]
pub struct QirArtifact {
    /// LLVM text with SSA classical values and QIR 2.1 flags.
    pub llvm_ir: String,
    /// LLVM bitcode of the same verified module.
    pub bitcode: Vec<u8>,
    /// Physical resources including the QND measurement scratch ancilla.
    pub qubit_count: u32,
    /// Statically allocated measurement sites, including scratch results.
    pub result_count: u32,
}

/// Failures at the typed VM-to-QIR boundary.
#[derive(Debug, thiserror::Error)]
pub enum QirEmissionError {
    /// A malformed identifier, value requirement or control structure.
    #[error("invalid QIR input: {0}")]
    InvalidProgram(String),
    /// A source behavior lacks an implemented lowering contract.
    #[error("unsupported QIR behavior: {0}")]
    Unsupported(String),
    /// A native LLVM tool could not be started or communicated with.
    #[error("LLVM tool I/O: {0}")]
    ToolIo(#[from] std::io::Error),
    /// LLVM rejected the generated module.
    #[error("LLVM tool failed: {0}")]
    Llvm(String),
}

/// Emit and verify standard QIR 2.1 Adaptive text and bitcode.
///
/// Input/output patch initialization must be authored in the physical program;
/// simulator postselection and cloned-state probes are not exported. Bindings
/// name decoder model outputs rather than inheriting VM mock decisions.
///
/// # Errors
///
/// Rejects malformed programs, unsupported timing/noise/patch I/O, missing
/// decoder bindings, and native tool or LLVM verification failures.
pub fn emit_program_qir(
    program: &bloq_vm::Program,
    options: &QirOptions,
) -> Result<QirArtifact, QirEmissionError> {
    emit::emit(program, options)
}

// Shared by the emitter's pending solves and protection-loop construction.
#[derive(Debug, Clone)]
struct PendingSolve {
    binding: DecoderBinding,
    raw: String,
    tasks: Vec<TaskId>,
}

fn invalid(message: impl Into<String>) -> QirEmissionError {
    QirEmissionError::InvalidProgram(message.into())
}

fn unsupported(message: impl Into<String>) -> QirEmissionError {
    QirEmissionError::Unsupported(message.into())
}

fn cycle_streams(
    cycles: &[MemoryCycle],
) -> impl Iterator<Item = &bloq_vm::instruction::QuantumStream> {
    cycles.iter().map(|cycle| &cycle.stream)
}
