//! Whole-component Clifford+T synthesis through a correlation-surface SAT problem.

mod zx;

use std::collections::HashSet;
use std::time::Duration;

use bloq_graph::{
    Action, BlockGraph, BlockKind, Expr, FeedbackTarget, MeasureTarget, PauliBasis, SelectiveKind,
    StabilizerGenerators,
};
use bloq_utils::{Direction, Pauli, PauliString, PhasedPauliString, PortRole, UDirection};
use glam::IVec3;
use thiserror::Error;

use self::zx::CanonicalComponent;
use crate::Deadline;
use crate::executor::block_on;
use crate::lassynth::{
    Port, SynthesisError, SynthesisProblem, boundary_rows, pauli_span_combination,
    synthesize_analyzed_async, valid_size,
};
use crate::monitor::SynthesisMonitor;
pub use bloq_graph::verify::{ParsedComponent, QasmError, parse_component};

/// Fixed geometry and wall-clock limit for one circuit component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentOptions {
    size: IVec3,
    inputs: Vec<Port>,
    outputs: Vec<Port>,
    time_limit: Duration,
    allow_spatial_hadamard: bool,
}

impl ComponentOptions {
    /// Creates a fixed-box layout with exterior ports.
    ///
    /// Reset inputs and measured outputs are omitted.
    pub fn new(
        size: IVec3,
        inputs: impl IntoIterator<Item = Port>,
        outputs: impl IntoIterator<Item = Port>,
        time_limit: Duration,
    ) -> Self {
        Self {
            size,
            inputs: inputs.into_iter().collect(),
            outputs: outputs.into_iter().collect(),
            time_limit,
            allow_spatial_hadamard: false,
        }
    }

    /// Allows Hadamard-decorated spatial pipes. Disabled by default.
    #[must_use]
    pub const fn with_spatial_hadamard(mut self, allow: bool) -> Self {
        self.allow_spatial_hadamard = allow;
        self
    }

    /// Returns the exact SAT volume.
    pub const fn size(&self) -> IVec3 {
        self.size
    }

    /// Returns unreset inputs in ascending circuit-qubit order.
    pub fn inputs(&self) -> &[Port] {
        &self.inputs
    }

    /// Returns unmeasured outputs in ascending circuit-qubit order.
    pub fn outputs(&self) -> &[Port] {
        &self.outputs
    }

    /// Returns the cooperative wall-clock limit, including preprocessing.
    /// Parsing, QuiZX simplification, and graph analysis cannot be interrupted mid-call.
    pub const fn time_limit(&self) -> Duration {
        self.time_limit
    }

    /// Returns whether spatial Hadamard pipes are allowed.
    pub const fn allows_spatial_hadamard(&self) -> bool {
        self.allow_spatial_hadamard
    }
}

/// Errors from whole-component synthesis.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CliffordTError {
    /// QASM parsing failed.
    #[error("{0}")]
    Qasm(#[from] QasmError),
    /// Circuit and ZX boundary port counts do not agree.
    #[error(
        "layout has {inputs} inputs and {outputs} outputs; expected {expected_inputs} and {expected_outputs} open ZX boundaries"
    )]
    PortCount {
        /// Expected number of input ports.
        expected_inputs: usize,
        /// Expected number of output ports.
        expected_outputs: usize,
        /// Actual number of input ports.
        inputs: usize,
        /// Actual number of output ports.
        outputs: usize,
    },
    /// The fixed box cannot host a required magic-state injection.
    #[error("the fixed box has no free temporal port pair for T injection {injection}")]
    NoMagicPort {
        /// Zero-based injection index.
        injection: usize,
    },
    /// QuiZX simplification produced unsupported topology or phases.
    #[error("QuiZX produced a diagram outside the supported Clifford+T form")]
    UnsupportedSimplifiedZx,
    /// Reset and measurement boundaries describe the zero map.
    #[error("the reset and measurement boundaries select a zero map")]
    ZeroMap,
    /// A T injection has no parity row that closes in its past.
    #[error("T injection {injection} has no past-closing parity row")]
    NonCausalTMeasurement {
        /// Zero-based injection index.
        injection: usize,
    },
    /// A T injection has no spatial correlation witness to measure.
    #[error("T injection {injection} has no measurable spatial correlation witness")]
    MissingMeasurementWitness {
        /// Zero-based injection index.
        injection: usize,
    },
    /// The synthesized relation requires a non-Pauli constant correction.
    #[error("the synthesized relation needs a non-Pauli constant correction")]
    InvalidCorrection,
    /// Fixed-volume Clifford synthesis failed.
    #[error("{0}")]
    Synthesis(#[from] SynthesisError),
    /// The synthesized block graph was rejected.
    #[error("{0}")]
    Graph(#[from] bloq_graph::BlockGraphError),
}

/// Synthesizes one complete circuit component in its caller-fixed box.
///
/// # Errors
///
/// Returns an error if QASM parsing, canonicalization, synthesis, or graph
/// construction fails.
pub fn synthesize_qasm(
    input: &str,
    options: &ComponentOptions,
) -> Result<BlockGraph, CliffordTError> {
    block_on(synthesize_qasm_async(
        input,
        options,
        &SynthesisMonitor::new(),
    ))
}

/// Native component synthesis with cancellation and progress reporting.
///
/// Runs synchronously when polled. Execute it on a worker thread.
///
/// # Errors
///
/// Returns the same errors as [`synthesize_qasm`], plus cancellation or timeout.
pub async fn synthesize_qasm_async(
    input: &str,
    options: &ComponentOptions,
    monitor: &SynthesisMonitor,
) -> Result<BlockGraph, CliffordTError> {
    let deadline = Deadline::after(options.time_limit);
    deadline.check(monitor)?;
    let parsed = parse_component(input)?;
    deadline.check(monitor)?;
    let expected_inputs = parsed.reset_inputs.iter().filter(|reset| !**reset).count();
    let expected_outputs = parsed
        .measured_outputs
        .iter()
        .filter(|measured| !**measured)
        .count();
    if options.inputs.len() != expected_inputs || options.outputs.len() != expected_outputs {
        return Err(CliffordTError::PortCount {
            expected_inputs,
            expected_outputs,
            inputs: options.inputs.len(),
            outputs: options.outputs.len(),
        });
    }
    deadline.check(monitor)?;
    let canonical = zx::canonicalize(
        &parsed.circuit,
        &parsed.reset_inputs,
        &parsed.measured_outputs,
    )?;
    deadline.check(monitor)?;
    let prepared = prepare_problem(
        options,
        &parsed.reset_inputs,
        &parsed.measured_outputs,
        canonical,
    )?;
    deadline.check(monitor)?;
    let (graph, generated, witnesses) =
        synthesize_analyzed_async(&prepared.problem, deadline.clone(), monitor).await?;
    deadline.check(monitor)?;
    let graph = finish_graph(graph, &generated, &witnesses, prepared)?;
    deadline.check(monitor)?;
    Ok(graph)
}

struct Prepared {
    problem: SynthesisProblem,
    canonical: CanonicalComponent,
    /// Lower temporal ports of each injection, to become `T` blocks.
    magic: Vec<IVec3>,
    /// Upper temporal ports of each injection, to become XY selective blocks.
    selective: Vec<IVec3>,
    /// Every wire a Pauli byproduct may be pushed onto, as its stabilizer
    /// column paired with the port position that carries the correction.
    corrections: Vec<(usize, IVec3)>,
}

fn prepare_problem(
    options: &ComponentOptions,
    zero_inputs: &[bool],
    measured_outputs: &[bool],
    canonical: CanonicalComponent,
) -> Result<Prepared, CliffordTError> {
    let inputs = zero_inputs
        .iter()
        .enumerate()
        .filter(|(_, reset)| !**reset)
        .zip(options.inputs.iter().cloned())
        .map(|((qubit, _), port)| {
            with_spatial_role(port, PortRole::Input).with_tag(format!("input_{qubit}"))
        })
        .collect::<Vec<_>>();
    let outputs = measured_outputs
        .iter()
        .enumerate()
        .filter(|(_, measured)| !**measured)
        .zip(options.outputs.iter().cloned())
        .map(|((qubit, _), port)| {
            with_spatial_role(port, PortRole::Output).with_tag(format!("output_{qubit}"))
        })
        .collect::<Vec<_>>();
    let (magic_ports, selective_ports) = auxiliary_ports(options, canonical.t_count)?;
    let magic = magic_ports.iter().map(Port::position).collect();
    let selective = selective_ports.iter().map(Port::position).collect();
    let output_offset = inputs.len() + canonical.t_count;
    let corrections = inputs
        .iter()
        .enumerate()
        .map(|(qubit, port)| (qubit, port.position()))
        .chain(
            outputs
                .iter()
                // A closed data output can leave a sign supported only on an
                // injection's selective output. Correct that wire before its cap.
                .chain(&selective_ports)
                .enumerate()
                .map(|(qubit, port)| (output_offset + qubit, port.position())),
        )
        .collect();
    let ports = inputs
        .into_iter()
        .chain(magic_ports)
        .chain(outputs)
        .chain(selective_ports)
        .collect::<Vec<_>>();
    let problem = SynthesisProblem::new(
        options.size,
        ports,
        canonical.stabilizers.iter().map(|row| row.paulis.clone()),
    )
    .with_measurement_rows(canonical.parity_rows.iter().copied())
    .with_spatial_hadamard(options.allow_spatial_hadamard);
    Ok(Prepared {
        problem,
        canonical,
        magic,
        selective,
        corrections,
    })
}

fn with_spatial_role(port: Port, role: PortRole) -> Port {
    if port.direction().is_spatial() {
        port.with_role(role)
    } else {
        port
    }
}

fn auxiliary_ports(
    options: &ComponentOptions,
    count: usize,
) -> Result<(Vec<Port>, Vec<Port>), CliffordTError> {
    if !valid_size(options.size) {
        return Err(SynthesisError::InvalidSize(options.size).into());
    }
    let mut positions = HashSet::new();
    let mut pipes = HashSet::new();
    for (index, port) in options.inputs.iter().chain(&options.outputs).enumerate() {
        positions.insert(port.position());
        let pipe = port.pipe_key().ok_or_else(|| SynthesisError::InvalidPort {
            index,
            reason: "inward neighbor coordinate overflows".into(),
        })?;
        pipes.insert(pipe);
    }
    let mut magic = Vec::with_capacity(count);
    let mut selective = Vec::with_capacity(count);
    for injection in 0..count {
        let pair = (0..options.size.x).find_map(|x| {
            (0..options.size.y).find_map(|y| {
                let lower = Port::new(IVec3::new(x, y, -1), Direction::ZPLUS, UDirection::Y);
                let upper = Port::new(
                    IVec3::new(x, y, options.size.z),
                    Direction::ZMINUS,
                    UDirection::Y,
                );
                let new_positions = [lower.position(), upper.position()];
                let new_pipes = [
                    lower
                        .pipe_key()
                        .expect("generated temporal port is in range"),
                    upper
                        .pipe_key()
                        .expect("generated temporal port is in range"),
                ];
                (new_positions
                    .iter()
                    .all(|position| !positions.contains(position))
                    && new_pipes[0] != new_pipes[1]
                    && new_pipes.iter().all(|pipe| !pipes.contains(pipe)))
                .then_some((lower, upper, new_positions, new_pipes))
            })
        });
        let Some((lower, upper, new_positions, new_pipes)) = pair else {
            return Err(CliffordTError::NoMagicPort { injection });
        };
        positions.extend(new_positions);
        pipes.extend(new_pipes);
        magic.push(lower.with_tag(format!("t_{injection}_magic")));
        selective.push(upper.with_tag(format!("t_{injection}_selective")));
    }
    Ok((magic, selective))
}

fn finish_graph(
    mut graph: BlockGraph,
    generated: &StabilizerGenerators,
    witnesses: &[Vec<MeasureTarget>],
    prepared: Prepared,
) -> Result<BlockGraph, CliffordTError> {
    let signed = boundary_rows(generated, prepared.problem.ports());
    let rows = signed
        .iter()
        .map(|row| row.paulis.clone())
        .collect::<Vec<_>>();
    let realized = prepared
        .canonical
        .stabilizers
        .iter()
        .map(|row| realize(&signed, &rows, row))
        .collect::<Result<Vec<_>, _>>()?;
    let (correction, parity_flips) = constant_correction(&prepared, &realized)?;
    let mut actions = Vec::with_capacity(prepared.magic.len() * 2 + 1);
    let mut measured = HashSet::new();
    for (injection, (&row, &selective)) in prepared
        .canonical
        .parity_rows
        .iter()
        .zip(&prepared.selective)
        .enumerate()
    {
        let target = witnesses[row]
            .iter()
            .copied()
            .find(|target| !measured.contains(target))
            .ok_or(CliffordTError::MissingMeasurementWitness { injection })?;
        measured.insert(target);
        let name = format!("t_{injection}_mzz");
        actions.push(Action::Measure {
            target,
            name: name.clone(),
        });
        let variable = Expr::Var(name);
        let same_sign = realized[row] ^ parity_flips[injection]
            == prepared.canonical.stabilizers[row]
                .hermitian_sign()
                .expect("canonical stabilizers are Hermitian");
        actions.push(Action::Resolve {
            target: selective,
            condition: if same_sign {
                Expr::Not(Box::new(variable))
            } else {
                variable
            },
        });
    }
    if !correction.is_empty() {
        actions.push(Action::Feedback {
            targets: correction,
            condition: None,
        });
    }
    for position in prepared.magic {
        graph.set_block_kind(position, BlockKind::T)?;
    }
    for position in prepared.selective {
        graph.set_block_kind(position, BlockKind::Selective(SelectiveKind::XY))?;
    }
    graph.set_actions(actions)?;
    Ok(graph)
}

fn realize(
    signed: &[PhasedPauliString],
    rows: &[PauliString],
    target: &PhasedPauliString,
) -> Result<bool, CliffordTError> {
    let combination =
        pauli_span_combination(&target.paulis, rows).ok_or(CliffordTError::InvalidCorrection)?;
    // Boundary products carry Pauli phases even though geometric surface
    // supports use phaseless multiplication inside bloq_graph.
    let mut product = PhasedPauliString::positive(PauliString::new(target.paulis.len()));
    for index in combination {
        product.multiply_assign(&signed[index]);
    }
    Ok(product
        .hermitian_sign()
        .expect("boundary stabilizers commute, so their product is Hermitian"))
}

fn constant_correction(
    prepared: &Prepared,
    realized: &[bool],
) -> Result<(Vec<FeedbackTarget>, Vec<bool>), CliffordTError> {
    let equations = (0..prepared.canonical.stabilizers.len())
        .filter(|index| !prepared.canonical.parity_rows.contains(index))
        .collect::<Vec<_>>();
    let rhs = PauliString::from_terms(
        equations.len(),
        equations.iter().enumerate().filter_map(|(equation, &row)| {
            (prepared.canonical.stabilizers[row]
                .hermitian_sign()
                .expect("canonical stabilizers are Hermitian")
                != realized[row])
                .then_some((equation, Pauli::X))
        }),
    );
    let columns = prepared
        .corrections
        .iter()
        .flat_map(|&(column, _)| {
            [Pauli::X, Pauli::Z].map(|correction| {
                PauliString::from_terms(
                    equations.len(),
                    equations.iter().enumerate().filter_map(|(equation, &row)| {
                        correction
                            .anticommutes(prepared.canonical.stabilizers[row].paulis.get(column))
                            .then_some((equation, Pauli::X))
                    }),
                )
            })
        })
        .collect::<Vec<_>>();
    let selected =
        pauli_span_combination(&rhs, &columns).ok_or(CliffordTError::InvalidCorrection)?;
    // `columns` interleaves the X and Z correction of each wire, so a selected
    // variable names its wire by halving and its basis by parity.
    let mut corrections = vec![Pauli::I; prepared.corrections.len()];
    for variable in selected {
        corrections[variable / 2] = corrections[variable / 2]
            | if variable % 2 == 0 {
                Pauli::X
            } else {
                Pauli::Z
            };
    }
    let parity_flips = prepared
        .canonical
        .parity_rows
        .iter()
        .map(|&row| {
            corrections
                .iter()
                .zip(&prepared.corrections)
                .filter(|(correction, (column, _))| {
                    correction.anticommutes(prepared.canonical.stabilizers[row].paulis.get(*column))
                })
                .count()
                % 2
                != 0
        })
        .collect();
    let targets = corrections
        .into_iter()
        .zip(&prepared.corrections)
        .filter_map(|(pauli, &(_, target))| {
            let pauli = match pauli {
                Pauli::I => return None,
                Pauli::X => PauliBasis::X,
                Pauli::Y => PauliBasis::Y,
                Pauli::Z => PauliBasis::Z,
            };
            Some(FeedbackTarget {
                pauli,
                target,
                direction: None,
            })
        })
        .collect();
    Ok((targets, parity_flips))
}
