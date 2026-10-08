//! Symbolic output-correction equations for logical verification.

use std::collections::{BTreeMap, BTreeSet};

use glam::IVec3;
use quizx::fscalar::{FScalar, Zero};
use quizx::graph::{GraphLike, VType};
use quizx::params::{Parity, Var};
use quizx::phase::Phase;
use rustc_hash::FxHashMap;

use crate::zx::{CoeffVec, solve_coeff_combination};
use crate::{
    Action, ActionNode, NodeKind, OutputCorrectionError, Pauli, PauliBasis, RuntimeStabilizerBasis,
    ZXGraph,
};

use super::feedback::{FeedbackInferenceError, checked_count};
use super::parity::{phase_constant, phase_value};
use super::{
    MeasurementKey, QuizxGraph, VerifyLogicalError, replay_fill, solve_evolved_correction,
    stabilizer_supports_measurement,
};

#[derive(Debug, Clone)]
pub(super) struct InternalCorrection {
    measurement_vars: BTreeMap<MeasurementKey, Parity>,
    sampled: Vec<(MeasurementKey, Var)>,
    initial_basis: RuntimeStabilizerBasis,
    named_vars: BTreeMap<String, Var>,
    readouts: BTreeMap<String, crate::StabilizerGenerator>,
}

pub(super) struct ResolvedCorrection {
    pub(super) basis: RuntimeStabilizerBasis,
    pub(super) phases: BTreeMap<MeasurementKey, Parity>,
    readouts: Vec<crate::Stabilizer>,
}

impl InternalCorrection {
    /// Cover anonymous coordinates by complete corrected boundary-sign classes.
    ///
    /// With resource legs open, exposing the native phase bits as outcome legs
    /// gives one Clifford stabilizer tensor. The resolved basis must span its
    /// full correlation space, including private rows: its closed kernel then
    /// gives complete affine support, and nonzero outcome slices have equal
    /// norm. Complete boundary signs identify their rays; equal coordinate
    /// fibers therefore have equal Kraus weight, not just equal directions.
    /// A fixed resource contraction preserves within-class unit-phase equality,
    /// while different classes may acquire different norms or vanish.
    pub(super) fn representatives(
        &self,
        outputs: &[IVec3],
        actions: &[ActionNode],
        values: &FxHashMap<Var, bool>,
        feedback_values: &FxHashMap<usize, bool>,
        resolved: &ResolvedCorrection,
        limit: usize,
    ) -> Result<Vec<FxHashMap<Var, bool>>, FeedbackInferenceError> {
        let ResolvedCorrection {
            basis,
            phases,
            readouts,
        } = resolved;
        let surfaces = basis.resolved_surfaces();
        let columns = surfaces
            .iter()
            .map(|surface| pauli_coeffs(&surface.paulis))
            .collect::<Vec<_>>();
        // Validate the actual readout recipe, not merely the existence of some
        // C0 witness. A later fill must preserve its signed Clifford relation.
        for (name, readout) in self.named_vars.keys().zip(readouts) {
            if solve_coeff_combination(&columns, &pauli_coeffs(&readout.paulis)).is_none()
                || basis
                    .zx_graph()
                    .materialize_stabilizer(readout.paulis.clone())
                    .sign
                    != readout.sign
                || basis
                    .zx_graph()
                    .nodes()
                    .iter()
                    .filter(|node| node.is_output_port(basis.zx_graph()))
                    .any(|node| {
                        let pauli = readout.paulis.get(node.id);
                        if node.role == crate::PortRole::Multiplex {
                            pauli & Pauli::X
                        } else {
                            pauli != Pauli::I
                        }
                    })
            {
                return Err(FeedbackInferenceError::ReadoutConventionUnavailable(
                    name.clone(),
                ));
            }
        }
        let variables = self.sampled_vars().collect::<Vec<_>>();
        let variable_set = variables.iter().copied().collect::<BTreeSet<_>>();
        let named = phases
            .iter()
            .map(|(key, parity)| {
                (
                    key.clone(),
                    phase_value(parity, |variable| {
                        !variable_set.contains(&variable) && values[&variable]
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let closed = basis.closed_stabilizer_surfaces();
        let constraints = self.parity_columns(&closed, phases);
        let mut rhs = CoeffVec::zeros(closed.len());
        for (row, surface) in closed.iter().enumerate() {
            let parity = named.iter().fold(surface.sign, |odd, (key, value)| {
                odd ^ (*value && stabilizer_supports_measurement(surface, key))
            }) ^ feedback_sign(surface, basis.zx_graph(), actions, feedback_values);
            rhs.set_bit(row, parity);
        }
        let Some(witness) = solve_coeff_combination(&constraints, &rhs) else {
            return Ok(Vec::new());
        };
        let directions = kernel_directions(&constraints);
        let reference = self.apply_coordinate(phases, named.clone(), &witness);
        let (correction_surfaces, correction) =
            solve_evolved_correction(basis, outputs, |surface| {
                phases
                    .keys()
                    .any(|key| stabilizer_supports_measurement(surface, key))
            })?;
        let correction_columns = self.parity_columns(&correction_surfaces, phases);
        let mut constant = CoeffVec::zeros(correction_surfaces.len());
        for (row, surface) in correction_surfaces.iter().enumerate() {
            constant.set_bit(
                row,
                named.iter().fold(false, |odd, (key, value)| {
                    odd ^ ((*value ^ reference[key])
                        && stabilizer_supports_measurement(surface, key))
                }),
            );
        }
        // Discharge every runtime consistency condition on the affine domain,
        // without treating a failed correction as a physically impossible shot.
        let witness_rhs = combine_columns(&correction_columns, &witness, constant.clone());
        correction.evaluate(
            &(0..witness_rhs.len())
                .map(|bit| witness_rhs.bit(bit))
                .collect::<Vec<_>>(),
        )?;
        for direction in &directions {
            let test = combine_columns(&correction_columns, direction, witness_rhs.clone());
            correction.evaluate(&(0..test.len()).map(|bit| test.bit(bit)).collect::<Vec<_>>())?;
        }
        if directions.is_empty() {
            checked_count(0, limit, "anonymous boundary classes")?;
            return Ok(vec![
                variables
                    .iter()
                    .enumerate()
                    .map(|(index, &variable)| (variable, witness.bit(index)))
                    .collect(),
            ]);
        }

        let full_columns = self.parity_columns(&surfaces, phases);
        let (bodies, extra_outputs, boundary_qubits) = boundary_bodies(basis, &surfaces, outputs)?;
        let mut independent = Vec::<CoeffVec>::new();
        for body in &bodies {
            let column = pauli_coeffs(body);
            if solve_coeff_combination(&independent, &column).is_none() {
                independent.push(column);
            }
        }
        if independent.len() != boundary_qubits
            || bodies.iter().enumerate().any(|(index, body)| {
                bodies[..index]
                    .iter()
                    .any(|other| !body.commutes_with(other))
            })
        {
            return Err(FeedbackInferenceError::IncompleteCertificate {
                rank: independent.len(),
                expected: boundary_qubits,
            });
        }

        let mut signatures = Vec::<CoeffVec>::new();
        let mut selected = Vec::new();
        for direction in directions {
            let changes = combine_columns(
                &correction_columns,
                &direction,
                CoeffVec::zeros(correction_surfaces.len()),
            );
            let frames = correction
                .frames()
                .iter()
                .map(|pair| {
                    let x = pair
                        .x
                        .iter()
                        .fold(false, |odd, &row| odd ^ changes.bit(row));
                    let z = pair
                        .z
                        .iter()
                        .fold(false, |odd, &row| odd ^ changes.bit(row));
                    match (x, z) {
                        (false, false) => Pauli::I,
                        (true, false) => Pauli::X,
                        (false, true) => Pauli::Z,
                        (true, true) => Pauli::Y,
                    }
                })
                .collect::<Vec<_>>();
            let native =
                combine_columns(&full_columns, &direction, CoeffVec::zeros(surfaces.len()));
            let mut signature = CoeffVec::zeros(bodies.len());
            for (row, surface) in surfaces.iter().enumerate() {
                let correction =
                    outputs
                        .iter()
                        .zip(&frames)
                        .fold(false, |odd, (position, frame)| {
                            let node = basis
                                .zx_graph()
                                .node_at(*position)
                                .expect("validated output");
                            odd ^ frame.anticommutes(surface.paulis.get(node.id))
                        });
                signature.set_bit(row, native.bit(row) ^ correction);
            }
            for (extra, &output) in extra_outputs.iter().enumerate() {
                signature.set_bit(
                    surfaces.len() + extra,
                    frames[output].anticommutes(Pauli::Z),
                );
            }
            if solve_coeff_combination(&signatures, &signature).is_none() {
                signatures.push(signature);
                selected.push(direction);
                checked_count(selected.len(), limit, "anonymous boundary classes")?;
            }
        }
        let count = checked_count(selected.len(), limit, "anonymous boundary classes")?;
        Ok((0..count)
            .map(|mask| {
                let mut coordinate = witness.clone();
                for (bit, direction) in selected.iter().enumerate() {
                    if mask & (1 << bit) != 0 {
                        coordinate.xor_assign(direction);
                    }
                }
                variables
                    .iter()
                    .enumerate()
                    .map(|(index, &variable)| (variable, coordinate.bit(index)))
                    .collect()
            })
            .collect())
    }

    fn parity_columns(
        &self,
        surfaces: &[crate::Stabilizer],
        phases: &BTreeMap<MeasurementKey, Parity>,
    ) -> Vec<CoeffVec> {
        self.sampled_vars()
            .map(|variable| {
                let mut column = CoeffVec::zeros(surfaces.len());
                for (row, surface) in surfaces.iter().enumerate() {
                    column.set_bit(
                        row,
                        phases
                            .iter()
                            .filter(|(_, parity)| parity.iter().any(|value| value == variable))
                            .fold(false, |odd, (key, _)| {
                                odd ^ stabilizer_supports_measurement(surface, key)
                            }),
                    );
                }
                column
            })
            .collect()
    }
    pub(super) fn new(
        zx: &ZXGraph,
        named_phases: &BTreeMap<MeasurementKey, Parity>,
        named_vars: &BTreeMap<String, Var>,
        next_var: &mut usize,
    ) -> Result<Self, VerifyLogicalError> {
        let keys = measurement_keys(zx);
        let mut measurement_vars = named_phases.clone();
        let mut sampled = Vec::new();
        for key in keys {
            if measurement_vars.contains_key(&key)
                || matches!(&key, MeasurementKey::Node { pos, .. }
                if measurement_vars.keys().any(|named| matches!(named, MeasurementKey::Node { pos: named, .. } if named == pos)))
            {
                continue;
            }
            let variable = fresh_var(next_var)?;
            measurement_vars.insert(key.clone(), Parity::single(variable));
            sampled.push((key, variable));
        }

        let (initial_basis, readouts) = if named_vars.is_empty() {
            let basis = RuntimeStabilizerBasis::from_zx_graph(zx.clone()).map_err(|source| {
                VerifyLogicalError::RuntimeBasisInitializationFailed { source }
            })?;
            (basis, BTreeMap::new())
        } else {
            let mut stabilizers = zx.stabilizers().map_err(|source| {
                VerifyLogicalError::RuntimeBasisInitializationFailed {
                    source: source.into(),
                }
            })?;
            let initial_basis = RuntimeStabilizerBasis::from_generators(&stabilizers);
            // ZX conversion starts with syntax dependencies. Restore the same
            // selected witnesses and fixing closure used by physical lowering.
            let mut dag = zx.action_graph().clone();
            dag.attach_readout_dependencies(&stabilizers.generators, Some(&stabilizers.zx_graph))?;
            stabilizers
                .prepare_analysis_readouts(&dag)
                .map_err(
                    |source| VerifyLogicalError::RuntimeBasisInitializationFailed { source },
                )?;
            let readouts = stabilizers
                .generators
                .into_iter()
                .filter_map(|generator| {
                    let name = generator.measurement_name()?.to_owned();
                    Some((name, generator))
                })
                .collect();
            (initial_basis, readouts)
        };
        if !sampled.is_empty() && !named_vars.is_empty() {
            // A free native phase can cross a named row. Compensate through the
            // same right-inverse that realizes named outcomes, so the action still
            // reads its supplied outcome after unnamed measurements are sampled.
            for (name, generator) in &readouts {
                let named = named_vars[name];
                for (key, variable) in &sampled {
                    if !stabilizer_supports_measurement(&generator.stabilizer, key) {
                        continue;
                    }
                    for (target, parity) in named_phases {
                        if parity.iter().any(|value| value == named) {
                            let target = measurement_vars
                                .get_mut(target)
                                .expect("named phase exists");
                            *target = &*target + &Parity::single(*variable);
                        }
                    }
                }
            }
        }
        Ok(Self {
            measurement_vars,
            sampled,
            initial_basis,
            named_vars: named_vars.clone(),
            readouts,
        })
    }

    pub(super) fn measurement_vars(&self) -> &BTreeMap<MeasurementKey, Parity> {
        &self.measurement_vars
    }

    pub(super) fn sampled_vars(&self) -> impl Iterator<Item = Var> + '_ {
        self.sampled.iter().map(|(_, variable)| *variable)
    }

    fn resolved_measurement_vars(
        &self,
        selectives: &FxHashMap<IVec3, MeasurementKey>,
        rows: &[crate::Stabilizer],
    ) -> Result<BTreeMap<MeasurementKey, Parity>, VerifyLogicalError> {
        let mut phases = BTreeMap::<MeasurementKey, Parity>::new();
        for (key, parity) in &self.measurement_vars {
            let target = match key {
                MeasurementKey::Node { pos, .. } => selectives.get(pos).unwrap_or(key),
                MeasurementKey::Edge { .. } => key,
            };
            let entry = phases.entry(target.clone()).or_default();
            *entry = &*entry + parity;
        }
        if self.named_vars.is_empty() {
            return Ok(phases);
        }
        let columns = phases
            .keys()
            .map(|key| {
                let mut column = CoeffVec::zeros(rows.len());
                for (row, surface) in rows.iter().enumerate() {
                    column.set_bit(row, stabilizer_supports_measurement(surface, key));
                }
                column
            })
            .collect::<Vec<_>>();
        // A fill can move a later named readout onto different native records.
        // Reparameterize the whole affine fiber, rather than preserving an
        // obsolete pre-fill compensation or losing an anonymous direction.
        let sampled = self.sampled_vars().collect::<BTreeSet<_>>();
        for parity in phases.values_mut() {
            *parity = Parity::new(
                parity
                    .iter()
                    .filter(|variable| !sampled.contains(variable))
                    .collect::<Vec<_>>(),
                phase_constant(parity),
            );
        }
        for (row, (name, &variable)) in self.named_vars.iter().enumerate() {
            let mut rhs = CoeffVec::singleton(row, rows.len());
            for (column, parity) in columns.iter().zip(phases.values()) {
                if parity.iter().any(|value| value == variable) {
                    rhs.xor_assign(column);
                }
            }
            let adjustment = solve_coeff_combination(&columns, &rhs)
                .ok_or_else(|| VerifyLogicalError::MeasurementPhaseUnavailable(name.clone()))?;
            for (index, parity) in phases.values_mut().enumerate() {
                if adjustment.bit(index) {
                    *parity = &*parity + &Parity::single(variable);
                }
            }
        }
        let mut constant = CoeffVec::zeros(rows.len());
        for (column, parity) in columns.iter().zip(phases.values()) {
            if phase_constant(parity) {
                constant.xor_assign(column);
            }
        }
        let adjustment = solve_coeff_combination(&columns, &constant).ok_or_else(|| {
            VerifyLogicalError::MeasurementPhaseUnavailable("affine readout offset".into())
        })?;
        for (index, parity) in phases.values_mut().enumerate() {
            if adjustment.bit(index) {
                *parity = parity.negated();
            }
        }
        let directions = kernel_directions(&columns);
        if directions.len() > self.sampled.len() {
            return Err(VerifyLogicalError::MeasurementPhaseUnavailable(
                "resolved anonymous coordinates".into(),
            ));
        }
        for (direction, (_, variable)) in directions.iter().zip(&self.sampled) {
            for (index, parity) in phases.values_mut().enumerate() {
                if direction.bit(index) {
                    *parity = &*parity + &Parity::single(*variable);
                }
            }
        }
        Ok(phases)
    }

    pub(super) fn zx_graph(&self) -> &ZXGraph {
        self.initial_basis.zx_graph()
    }

    pub(super) fn resolve(
        &self,
        actions: &[ActionNode],
        selectives: &FxHashMap<IVec3, PauliBasis>,
    ) -> Result<ResolvedCorrection, VerifyLogicalError> {
        let mut basis = self.initial_basis.clone();
        let mut keys = FxHashMap::default();
        let mut resolves = actions
            .iter()
            .filter_map(|action| match action.action {
                Action::Resolve { target, .. } => Some((target.z, action.ordinal, target)),
                _ => None,
            })
            .collect::<Vec<_>>();
        resolves.sort_unstable_by_key(|&(z, ordinal, _)| (z, ordinal));
        for (_, _, target) in resolves {
            let (updated, key) = replay_fill(&basis, target, selectives[&target])?;
            basis = updated;
            keys.insert(target, key);
        }
        // Match physical readout lowering: keep the authored signed row when
        // it needs no fills; otherwise use precisely its causal fills and
        // accepted deadline. A fresh all-fills witness can change the label.
        // SEM-READ reports the physical parity of these particular records:
        // transport signs remain in the ZX tensor/closed constraints. Adding
        // surface.sign again would invert signed readouts. The convention is
        // anchored here, rather than assuming every equivalent witness has
        // zero affine offset relative to the authored record.
        let readouts = self
            .readouts
            .iter()
            .map(|(name, generator)| {
                if let Some(plan) = generator.readout_plan()
                    && plan.sites.iter().any(|site| !selectives.contains_key(site))
                {
                    return Err(VerifyLogicalError::MeasurementPhaseUnavailable(
                        name.clone(),
                    ));
                }
                Ok(generator
                    .named_readout()
                    .expect("named row")
                    .select(|site| {
                        let NodeKind::Selective(kind) =
                            self.zx_graph().node_at(site).expect("resolve node").kind
                        else {
                            unreachable!("resolve target is selective")
                        };
                        selectives[&site] == kind.pauli_if_true()
                    })
                    .clone())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let phases = self.resolved_measurement_vars(&keys, &readouts)?;
        Ok(ResolvedCorrection {
            basis,
            phases,
            readouts,
        })
    }

    pub(super) fn apply(
        &self,
        graph: &mut QuizxGraph,
        outputs: &[IVec3],
        values: &FxHashMap<Var, bool>,
        resolved: &ResolvedCorrection,
        actions: &[ActionNode],
        feedback_values: &FxHashMap<usize, bool>,
    ) -> Result<(), VerifyLogicalError> {
        let ResolvedCorrection {
            basis,
            phases: measurement_vars,
            ..
        } = resolved;
        let measured = measurement_vars
            .iter()
            .map(|(key, parity)| {
                (
                    key.clone(),
                    phase_value(parity, |variable| values[&variable]),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let sampled = self.sampled_vars().collect::<BTreeSet<_>>();
        let reference = measurement_vars
            .iter()
            .map(|(key, parity)| {
                let value = phase_value(parity, |variable| {
                    !sampled.contains(&variable) && values[&variable]
                });
                (key.clone(), value)
            })
            .collect::<BTreeMap<_, _>>();

        let Some(reference) =
            self.reference_outcomes(basis, measurement_vars, reference, actions, feedback_values)
        else {
            *graph.scalar_mut() = FScalar::zero();
            return Ok(());
        };
        let (surfaces, correction) = solve_evolved_correction(basis, outputs, |surface| {
            measured
                .keys()
                .any(|key| stabilizer_supports_measurement(surface, key))
        })?;
        let flips = surfaces
            .iter()
            .map(|surface| {
                measured.iter().fold(false, |parity, (key, value)| {
                    parity
                        ^ ((*value ^ reference[key])
                            && stabilizer_supports_measurement(surface, key))
                })
            })
            .collect::<Vec<_>>();
        match correction.evaluate(&flips) {
            Ok(corrections) => insert_output_corrections(graph, outputs, &corrections),
            Err(OutputCorrectionError::Inconsistent) => *graph.scalar_mut() = FScalar::zero(),
            Err(
                error @ (OutputCorrectionError::RhsLengthMismatch { .. }
                | OutputCorrectionError::UnknownOutput { .. }),
            ) => return Err(error.into()),
        }
        Ok(())
    }

    /// Correct within the authored named branch. Its all-zero unnamed outcome
    /// assignment can be impossible: repeated merges must agree with the named
    /// parity. Closed surfaces find a consistent reference with free bits zero.
    fn reference_outcomes(
        &self,
        basis: &RuntimeStabilizerBasis,
        measurement_vars: &BTreeMap<MeasurementKey, Parity>,
        reference: BTreeMap<MeasurementKey, bool>,
        actions: &[ActionNode],
        feedback_values: &FxHashMap<usize, bool>,
    ) -> Option<BTreeMap<MeasurementKey, bool>> {
        let surfaces = basis.closed_stabilizer_surfaces();
        let mut rhs = CoeffVec::zeros(surfaces.len());
        for (index, surface) in surfaces.iter().enumerate() {
            let parity = reference
                .iter()
                .fold(surface.sign, |parity, (key, &value)| {
                    parity ^ (value && stabilizer_supports_measurement(surface, key))
                })
                ^ feedback_sign(surface, basis.zx_graph(), actions, feedback_values);
            rhs.set_bit(index, parity);
        }
        // Each sampled variable includes its compensating native phases.
        // Solving individual phase coordinates would change the named branch.
        let columns = self.parity_columns(&surfaces, measurement_vars);
        let solution = solve_coeff_combination(&columns, &rhs)?;
        Some(self.apply_coordinate(measurement_vars, reference, &solution))
    }

    fn apply_coordinate(
        &self,
        phases: &BTreeMap<MeasurementKey, Parity>,
        mut outcomes: BTreeMap<MeasurementKey, bool>,
        coordinate: &CoeffVec,
    ) -> BTreeMap<MeasurementKey, bool> {
        for (key, value) in &mut outcomes {
            let parity = &phases[key];
            for (index, (_, variable)) in self.sampled.iter().enumerate() {
                *value ^= coordinate.bit(index) && parity.iter().any(|value| value == *variable);
            }
        }
        outcomes
    }
}

fn combine_columns(columns: &[CoeffVec], selected: &CoeffVec, mut result: CoeffVec) -> CoeffVec {
    for column in selected.iter_ones() {
        result.xor_assign(&columns[column]);
    }
    result
}

fn pauli_coeffs(paulis: &crate::PauliString) -> CoeffVec {
    let mut coefficients = CoeffVec::zeros(2 * paulis.len());
    for (column, pauli) in paulis.iter_support() {
        coefficients.set_bit(2 * column, pauli & Pauli::X);
        coefficients.set_bit(2 * column + 1, pauli & Pauli::Z);
    }
    coefficients
}

fn kernel_directions(columns: &[CoeffVec]) -> Vec<CoeffVec> {
    let mut basis = Vec::<CoeffVec>::new();
    let mut selected = Vec::new();
    let mut kernel = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        if let Some(combination) = solve_coeff_combination(&basis, column) {
            let mut direction = CoeffVec::singleton(index, columns.len());
            for bit in combination.iter_ones() {
                direction.set_bit(selected[bit], true);
            }
            kernel.push(direction);
        } else {
            selected.push(index);
            basis.push(column.clone());
        }
    }
    kernel
}

fn feedback_sign(
    surface: &crate::Stabilizer,
    zx: &ZXGraph,
    actions: &[ActionNode],
    values: &FxHashMap<usize, bool>,
) -> bool {
    actions
        .iter()
        .filter(|action| values.get(&action.ordinal) == Some(&true))
        .fold(false, |odd, action| {
            let Action::Feedback { targets, .. } = &action.action else {
                return odd;
            };
            targets.iter().fold(odd, |odd, target| {
                let Some((column, pauli)) = zx.feedback_column(target) else {
                    return odd;
                };
                odd ^ surface.paulis.get(column).anticommutes(pauli)
            })
        })
}

fn boundary_bodies(
    basis: &RuntimeStabilizerBasis,
    surfaces: &[crate::Stabilizer],
    outputs: &[IVec3],
) -> Result<(Vec<crate::PauliString>, Vec<usize>, usize), FeedbackInferenceError> {
    let zx = basis.zx_graph();
    let open = zx
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Port | NodeKind::T))
        .collect::<Vec<_>>();
    let width = open
        .iter()
        .map(|node| {
            if node.role == crate::PortRole::Multiplex {
                2
            } else {
                1
            }
        })
        .sum();
    let mut bodies = Vec::with_capacity(surfaces.len() + open.len());
    for surface in surfaces {
        let mut body = crate::PauliString::new(width);
        let mut column = 0;
        for node in &open {
            let pauli = surface.paulis.get(node.id);
            if node.role == crate::PortRole::Multiplex {
                // The Z-copy isometry maps X -> X⊗X, Z -> I⊗Z and adds Z⊗Z.
                body.set(column, if pauli & Pauli::X { Pauli::X } else { Pauli::I });
                column += 1;
            }
            body.set(column, pauli);
            column += 1;
        }
        bodies.push(body);
    }
    let mut extras = Vec::new();
    let mut column = 0;
    for node in open {
        if node.role == crate::PortRole::Multiplex {
            let output = outputs
                .iter()
                .position(|&position| position == node.pos)
                .ok_or_else(|| {
                    FeedbackInferenceError::Binding(
                        "multiplex boundary missing from output order".into(),
                    )
                })?;
            let mut body = crate::PauliString::new(width);
            body.set(column, Pauli::Z);
            body.set(column + 1, Pauli::Z);
            bodies.push(body);
            extras.push(output);
            column += 1;
        }
        column += 1;
    }
    Ok((bodies, extras, width))
}

pub(super) fn fresh_var(next: &mut usize) -> Result<Var, VerifyLogicalError> {
    let variable = Var::try_from(*next).map_err(|_| VerifyLogicalError::TooManySymbols)?;
    *next += 1;
    Ok(variable)
}

fn measurement_keys(zx: &ZXGraph) -> Vec<MeasurementKey> {
    let mut keys = BTreeSet::new();
    for node in zx.nodes() {
        let pauli = match node.kind {
            NodeKind::X => Pauli::Z,
            NodeKind::Y => Pauli::Y,
            NodeKind::Z => Pauli::X,
            NodeKind::Selective(kind) => Pauli::from(kind.pauli_if_false()),
            _ => continue,
        };
        let continues = zx.neighbors(node.id).is_some_and(|neighbors| {
            neighbors.iter().any(|&neighbor| {
                zx.nodes()[neighbor].pos.z > node.pos.z || zx.nodes()[neighbor].is_output_port(zx)
            })
        });
        if !continues {
            keys.insert(MeasurementKey::Node {
                pos: node.pos,
                pauli,
            });
        }
    }
    for edge in zx.edges().iter().filter(|edge| edge.n1 < edge.n2) {
        let lhs = zx.nodes()[edge.n1];
        let rhs = zx.nodes()[edge.n2];
        if lhs.pos.z != rhs.pos.z
            || !matches!(lhs.kind, NodeKind::X | NodeKind::Z)
            || !matches!(rhs.kind, NodeKind::X | NodeKind::Z)
        {
            continue;
        }
        if (lhs.kind == NodeKind::X) == ((rhs.kind == NodeKind::X) ^ edge.hadamard) {
            keys.insert(MeasurementKey::Edge {
                src: lhs.pos,
                dst: rhs.pos,
                pauli: if lhs.kind == NodeKind::X {
                    Pauli::X
                } else {
                    Pauli::Z
                },
            });
        }
    }
    keys.into_iter().collect()
}

fn insert_output_corrections(
    graph: &mut QuizxGraph,
    outputs: &[IVec3],
    corrections: &[(IVec3, Pauli)],
) {
    for &(position, pauli) in corrections {
        let output = outputs
            .iter()
            .position(|candidate| *candidate == position)
            .expect("correction names a requested output");
        let boundary = graph.outputs()[output];
        let vertex_types: &[VType] = match pauli {
            Pauli::I => &[],
            Pauli::X => &[VType::X],
            Pauli::Y => &[VType::Z, VType::X],
            Pauli::Z => &[VType::Z],
        };
        for &vertex_type in vertex_types {
            let (neighbor, edge_type) = graph
                .incident_edges(boundary)
                .next()
                .expect("an output boundary has one wire");
            graph.remove_edge(boundary, neighbor);
            let correction = graph.add_vertex_with_phase(vertex_type, Phase::from(1_i64));
            graph.add_edge(boundary, correction);
            graph.add_edge_with_type(correction, neighbor, edge_type);
        }
    }
}
