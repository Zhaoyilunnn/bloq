//! Clifford lattice-surgery synthesis from boundary stabilizers.

mod sat;

use std::collections::HashSet;
use std::time::Duration;

use bloq_graph::{BlockGraph, BlockGraphError, MeasureTarget, StabilizerGenerators};
use bloq_utils::{Direction, Pauli, PauliString, PhasedPauliString, PortRole, UDirection};
use glam::IVec3;
use thiserror::Error;

use crate::Deadline;
use crate::executor::block_on;
use crate::monitor::SynthesisMonitor;

/// A dangling boundary one cell outside the synthesis volume.
///
/// `direction` points into the volume; `z_basis` identifies logical Z.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub(crate) position: IVec3,
    pub(crate) direction: Direction,
    pub(crate) z_basis: UDirection,
    pub(crate) role: PortRole,
    pub(crate) tag: Option<String>,
}

impl Port {
    /// Creates an untagged exterior port.
    #[must_use]
    pub fn new(position: IVec3, direction: Direction, z_basis: UDirection) -> Self {
        Self {
            position,
            direction,
            z_basis,
            role: PortRole::Auto,
            tag: None,
        }
    }

    /// Assigns the logical role required by a spatial port.
    #[must_use]
    pub const fn with_role(mut self, role: PortRole) -> Self {
        self.role = role;
        self
    }

    /// Attaches the tag that will be copied onto the emitted boundary block.
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    /// Returns the boundary-node position.
    #[must_use]
    pub const fn position(&self) -> IVec3 {
        self.position
    }

    /// Returns the direction from the boundary node into the volume.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// Returns the axis normal to the logical Z boundary.
    #[must_use]
    pub const fn z_basis(&self) -> UDirection {
        self.z_basis
    }

    /// Returns the port's logical input/output role.
    #[must_use]
    pub const fn role(&self) -> PortRole {
        self.role
    }

    /// Returns the optional Bloq tag.
    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }

    /// The pipe this port attaches to, or `None` if the inward neighbor
    /// coordinate overflows.
    pub(crate) fn pipe_key(&self) -> Option<PipeKey> {
        let axis = self.direction.as_udirection();
        let position = if matches!(
            self.direction,
            Direction::XPLUS | Direction::YPLUS | Direction::ZPLUS
        ) {
            self.position
        } else {
            self.position.checked_add(self.direction.to_ivec3())?
        };
        Some(PipeKey { position, axis })
    }
}

/// The pipe running from `position` one cell along `axis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PipeKey {
    pub(crate) position: IVec3,
    pub(crate) axis: UDirection,
}

/// A fixed-volume Clifford synthesis problem.
///
/// Dimensions must be positive and each axis's edge-grid size must fit in an `i32`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesisProblem {
    pub(crate) size: IVec3,
    pub(crate) ports: Vec<Port>,
    pub(crate) stabilizers: Vec<PauliString>,
    pub(crate) measurement_rows: Vec<usize>,
    pub(crate) allow_spatial_hadamard: bool,
}

impl SynthesisProblem {
    /// Creates a synthesis problem over a `size.x × size.y × size.z` interior.
    ///
    /// Every port must sit one cell outside that box and point inward.
    #[must_use]
    pub fn new(
        size: IVec3,
        ports: impl IntoIterator<Item = Port>,
        stabilizers: impl IntoIterator<Item = PauliString>,
    ) -> Self {
        Self {
            size,
            ports: ports.into_iter().collect(),
            stabilizers: stabilizers.into_iter().collect(),
            measurement_rows: Vec::new(),
            allow_spatial_hadamard: false,
        }
    }

    pub(crate) fn with_measurement_rows(mut self, rows: impl IntoIterator<Item = usize>) -> Self {
        for row in rows {
            assert!(
                row < self.stabilizers.len(),
                "canonical T parity rows must refer to existing stabilizers"
            );
            self.measurement_rows.push(row);
        }
        self
    }

    /// Allows Hadamard-decorated spatial pipes. Disabled by default.
    #[must_use]
    pub const fn with_spatial_hadamard(mut self, allow: bool) -> Self {
        self.allow_spatial_hadamard = allow;
        self
    }

    /// Returns whether spatial Hadamard pipes are allowed.
    #[must_use]
    pub const fn allows_spatial_hadamard(&self) -> bool {
        self.allow_spatial_hadamard
    }

    /// Returns the synthesis-volume dimensions.
    #[must_use]
    pub const fn size(&self) -> IVec3 {
        self.size
    }

    /// Returns the open ports in stabilizer-column order.
    #[must_use]
    pub fn ports(&self) -> &[Port] {
        &self.ports
    }

    /// Returns the requested phaseless boundary stabilizers.
    #[must_use]
    pub fn stabilizers(&self) -> &[PauliString] {
        &self.stabilizers
    }

    fn validate(&self) -> Result<(), SynthesisError> {
        if !valid_size(self.size) {
            return Err(SynthesisError::InvalidSize(self.size));
        }
        if self.stabilizers.len() > self.ports.len() {
            return Err(SynthesisError::TooManyStabilizers {
                stabilizers: self.stabilizers.len(),
                ports: self.ports.len(),
            });
        }
        for (index, stabilizer) in self.stabilizers.iter().enumerate() {
            if stabilizer.len() != self.ports.len() {
                return Err(SynthesisError::StabilizerWidth {
                    index,
                    width: stabilizer.len(),
                    ports: self.ports.len(),
                });
            }
            for (other_index, other) in self.stabilizers[..index].iter().enumerate() {
                if !stabilizer.commutes_with(other) {
                    return Err(SynthesisError::NonCommutingStabilizers {
                        first: other_index,
                        second: index,
                    });
                }
            }
        }

        let mut positions = HashSet::with_capacity(self.ports.len());
        let mut pipes = HashSet::with_capacity(self.ports.len());
        for (index, port) in self.ports.iter().enumerate() {
            if port.direction.is_spatial() && port.role == PortRole::Auto {
                return Err(SynthesisError::InvalidPort {
                    index,
                    reason: "spatial ports must declare an input or output role".into(),
                });
            }
            if port.direction.as_udirection() == port.z_basis {
                return Err(SynthesisError::InvalidPort {
                    index,
                    reason: "pipe and Z-boundary axes must differ".into(),
                });
            }
            if !positions.insert(port.position) {
                return Err(SynthesisError::InvalidPort {
                    index,
                    reason: "another port occupies the same position".into(),
                });
            }
            let Some(inward) = port.position.checked_add(port.direction.to_ivec3()) else {
                return Err(SynthesisError::InvalidPort {
                    index,
                    reason: "inward neighbor coordinate overflows".into(),
                });
            };
            if in_bounds(self.size, port.position) {
                return Err(SynthesisError::InvalidPort {
                    index,
                    reason: "port must be one cell outside the synthesis volume".into(),
                });
            }
            if !in_bounds(self.size, inward) {
                return Err(SynthesisError::InvalidPort {
                    index,
                    reason: format!("inward neighbor {inward} is outside the volume"),
                });
            }
            let pipe = port
                .pipe_key()
                .expect("the inward neighbor coordinate was checked above");
            if !pipes.insert(pipe) {
                return Err(SynthesisError::InvalidPort {
                    index,
                    reason: "ports cannot share a pipe".into(),
                });
            }
        }

        Ok(())
    }
}

pub(crate) fn valid_size(size: IVec3) -> bool {
    if size.min_element() <= 0 {
        return false;
    }
    let dimensions = size.to_array().map(i64::from);
    (0..3).all(|axis| {
        let mut pipe_dimensions = dimensions;
        pipe_dimensions[axis] += 1;
        pipe_dimensions
            .into_iter()
            .try_fold(1_i64, i64::checked_mul)
            .is_some_and(|count| count <= i64::from(i32::MAX))
    })
}

/// Errors returned by Clifford synthesis.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SynthesisError {
    /// A volume dimension is not positive or its edge-variable count is too large.
    #[error(
        "invalid synthesis volume {0}: dimensions must be positive and edge count must fit i32"
    )]
    InvalidSize(IVec3),
    /// More independent surfaces were requested than there are ports.
    #[error("{stabilizers} stabilizers cannot fit on {ports} ports")]
    TooManyStabilizers {
        /// Number of requested independent stabilizers.
        stabilizers: usize,
        /// Number of boundary ports available to support them.
        ports: usize,
    },
    /// A stabilizer does not have one Pauli per port.
    #[error("stabilizer {index} has width {width}, expected {ports}")]
    StabilizerWidth {
        /// Zero-based index of the malformed stabilizer.
        index: usize,
        /// Actual number of Pauli entries.
        width: usize,
        /// Required number of Pauli entries.
        ports: usize,
    },
    /// Boundary stabilizers must commute.
    #[error("stabilizers {first} and {second} anticommute")]
    NonCommutingStabilizers {
        /// Index of the first anticommuting stabilizer.
        first: usize,
        /// Index of the second anticommuting stabilizer.
        second: usize,
    },
    /// A port is malformed or cannot point into the volume.
    #[error("invalid port {index}: {reason}")]
    InvalidPort {
        /// Index of the malformed port.
        index: usize,
        /// Why the port is invalid.
        reason: String,
    },
    /// A distinguished measurement row has no internal spatial owner candidate
    /// beside a supported boundary port.
    #[error(
        "measurement row {row} has no supported port-adjacent owner candidate in synthesis volume {size}"
    )]
    NoMeasurementAnchor {
        /// Zero-based index of the measurement row.
        row: usize,
        /// Fixed volume searched for supported owner candidates.
        size: IVec3,
    },
    /// No graph satisfies the requested constraints.
    #[error("synthesis problem is unsatisfiable")]
    Unsatisfiable,
    /// The SAT solver stopped before proving SAT or UNSAT.
    #[error("SAT solving was interrupted")]
    Interrupted,
    /// The configured wall-clock limit elapsed.
    #[error("synthesis timed out")]
    TimedOut,
    /// The caller cancelled the run through its [`SynthesisMonitor`](crate::SynthesisMonitor).
    #[error("synthesis was cancelled")]
    Cancelled,
    /// The in-process SAT backend failed or exhausted its variable resources.
    #[error("SAT backend failed: {0}")]
    Solver(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Bloq rejected the emitted graph.
    #[error("{0}")]
    Graph(#[from] BlockGraphError),
    /// Bloq's stabilizer engine did not find a requested boundary generator.
    #[error("emitted graph does not contain requested stabilizer {index}")]
    StabilizerMismatch {
        /// Index of the boundary generator that was not found.
        index: usize,
    },
}

/// Synthesizes one fixed-volume problem directly into a Bloq block graph.
///
/// # Errors
///
/// Returns [`SynthesisError::Unsatisfiable`] when no valid topology exists,
/// or a validation/backend error for malformed input or solver failure.
pub fn synthesize(problem: &SynthesisProblem) -> Result<BlockGraph, SynthesisError> {
    block_on(synthesize_async(
        problem,
        Deadline::none(),
        &SynthesisMonitor::new(),
    ))
}

async fn synthesize_async(
    problem: &SynthesisProblem,
    deadline: Deadline,
    monitor: &SynthesisMonitor,
) -> Result<BlockGraph, SynthesisError> {
    let graph = synthesize_analyzed_async(problem, deadline.clone(), monitor)
        .await?
        .0;
    deadline.check(monitor)?;
    Ok(graph)
}

pub(crate) async fn synthesize_analyzed_async(
    problem: &SynthesisProblem,
    deadline: Deadline,
    monitor: &SynthesisMonitor,
) -> Result<(BlockGraph, StabilizerGenerators, Vec<Vec<MeasureTarget>>), SynthesisError> {
    deadline.check(monitor)?;
    problem.validate()?;
    let solution = sat::solve(problem, deadline.clone(), monitor).await?;
    deadline.check(monitor)?;
    let generated = solution.graph.stabilizers()?;
    deadline.check(monitor)?;
    verify_requested_stabilizers(problem, &generated)?;
    deadline.check(monitor)?;
    Ok((solution.graph, generated, solution.witnesses))
}

/// Synthesizes one fixed-volume problem with a cooperative wall-clock limit.
///
/// # Errors
///
/// Returns validation, solver, timeout, or graph-construction errors.
pub fn synthesize_with_timeout(
    problem: &SynthesisProblem,
    timeout: Duration,
) -> Result<BlockGraph, SynthesisError> {
    block_on(synthesize_with_timeout_async(
        problem,
        timeout,
        &SynthesisMonitor::new(),
    ))
}

/// Native fixed-volume synthesis with cancellation and a cooperative time limit.
///
/// Runs synchronously when polled. Execute it on a worker thread.
/// Graph reconstruction and analysis are checked between synchronous calls.
///
/// # Errors
///
/// Returns validation, solver, cancellation, timeout, or graph-construction errors.
pub async fn synthesize_with_timeout_async(
    problem: &SynthesisProblem,
    timeout: Duration,
    monitor: &SynthesisMonitor,
) -> Result<BlockGraph, SynthesisError> {
    synthesize_async(problem, Deadline::after(timeout), monitor).await
}

fn verify_requested_stabilizers(
    problem: &SynthesisProblem,
    generated: &StabilizerGenerators,
) -> Result<(), SynthesisError> {
    let rows = boundary_rows(generated, &problem.ports)
        .into_iter()
        .map(|row| row.paulis)
        .collect::<Vec<_>>();
    for (index, target) in problem.stabilizers.iter().enumerate() {
        if pauli_span_combination(target, &rows).is_none() {
            return Err(SynthesisError::StabilizerMismatch { index });
        }
    }
    Ok(())
}

pub(crate) fn boundary_rows(
    generated: &StabilizerGenerators,
    ports: &[Port],
) -> Vec<PhasedPauliString> {
    generated
        .generators
        .iter()
        .map(|generator| {
            let stabilizer = &generator.stabilizer;
            PhasedPauliString::new(
                PauliString::from_terms(
                    ports.len(),
                    ports.iter().enumerate().filter_map(|(index, port)| {
                        stabilizer
                            .port_stabilizer
                            .get(&port.position)
                            .copied()
                            .filter(|pauli| *pauli != Pauli::I)
                            .map(|pauli| (index, pauli))
                    }),
                ),
                u8::from(stabilizer.sign) * 2,
            )
        })
        .collect()
}

pub(crate) fn pauli_span_combination(
    target: &PauliString,
    rows: &[PauliString],
) -> Option<Vec<usize>> {
    let mut rows = rows.to_vec();
    let mut target = target.clone();
    let mut combinations = (0..rows.len())
        .map(|index| {
            let mut combination = vec![false; rows.len()];
            combination[index] = true;
            combination
        })
        .collect::<Vec<_>>();
    let mut combination = vec![false; rows.len()];
    let mut pivot = 0;
    for component in 0..target.len() * 2 {
        let Some(found) = (pivot..rows.len()).find(|&row| component_set(&rows[row], component))
        else {
            continue;
        };
        rows.swap(pivot, found);
        combinations.swap(pivot, found);
        if component_set(&target, component) {
            target ^= &rows[pivot];
            xor_bits(&mut combination, &combinations[pivot]);
        }
        let (leading_rows, remaining_rows) = rows.split_at_mut(pivot + 1);
        let leading_row = &leading_rows[pivot];
        let (leading_combinations, remaining_combinations) = combinations.split_at_mut(pivot + 1);
        let leading_combination = &leading_combinations[pivot];
        for (row, row_combination) in remaining_rows.iter_mut().zip(remaining_combinations) {
            if component_set(row, component) {
                *row ^= leading_row;
                xor_bits(row_combination, leading_combination);
            }
        }
        pivot += 1;
    }
    (target.weight() == 0).then(|| {
        combination
            .into_iter()
            .enumerate()
            .filter_map(|(index, selected)| selected.then_some(index))
            .collect()
    })
}

fn xor_bits(target: &mut [bool], source: &[bool]) {
    target.iter_mut().zip(source).for_each(|(a, b)| *a ^= b);
}

pub(crate) fn component_set(paulis: &PauliString, component: usize) -> bool {
    let (index, x_component) = if component < paulis.len() {
        (component, true)
    } else {
        (component - paulis.len(), false)
    };
    matches!(
        (paulis.get(index), x_component),
        (Pauli::X | Pauli::Y, true) | (Pauli::Z | Pauli::Y, false)
    )
}

pub(crate) fn in_bounds(size: IVec3, position: IVec3) -> bool {
    position.cmpge(IVec3::ZERO).all() && position.cmplt(size).all()
}
