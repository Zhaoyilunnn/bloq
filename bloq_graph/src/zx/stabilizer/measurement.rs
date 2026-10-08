//! Measurement-surface constraints, selection, and canonicalization.

use std::collections::{HashMap, HashSet};

use bloq_utils::{Pauli, PauliString};

use super::super::ZXGraph;
use super::basis::{
    CoeffVec, TrackedBasis, axis_pivot_constraints, gaussian_elimination_with_tracking,
    solve_coeff_combination, solve_pauli_combination,
};
use super::selective::{SelectiveConstraint, collect_selective_constraints};
use super::{
    SearchBudget, Stabilizer, StabilizerError, StabilizerGenerator, StabilizerGenerators,
    StabilizerRowKind,
};
use crate::{MeasurementObservable, PauliBasis};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MeasurementSurfaceRow {
    pub(crate) name: String,
    /// First layer at which this surface's parity can be decoded, derived from
    /// the *accepted* row's support (see
    /// [`accepted_surface_deadline`]). This can exceed the spec's requested
    /// `minimum_support_z + 1` when early limits fail as
    /// entangled/unavailable and the surface is accepted late. A selective
    /// branch controlled by this parity is scheduled after the decode even
    /// when its graph site has an earlier z coordinate.
    pub(crate) deadline: i64,
    pub(crate) row: PauliString,
    pub(crate) combination: CoeffVec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MeasurementSpec {
    pub(super) name: String,
    pub(super) minimum_support_z: i32,
    pub(super) deadline: i64,
}

enum MeasurementConstructionAttempt {
    Accepted {
        basis: TrackedBasis,
        surface: MeasurementSurfaceRow,
    },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MeasurementMetadata {
    cols: HashMap<String, usize>,
    observables: HashMap<String, MeasurementObservable>,
    /// Records read on a pipe rather than at a node face.
    edge_records: HashSet<String>,
}

impl MeasurementMetadata {
    pub(crate) fn collect(zx: &ZXGraph) -> Self {
        let mut cols = HashMap::new();
        let mut observables = HashMap::new();
        let mut edge_records = HashSet::new();

        for node in zx.action_graph().ordered_nodes() {
            let crate::Action::Measure { name, target } = &node.action else {
                continue;
            };
            let Some(col) = zx.measurement_column(target) else {
                continue;
            };
            cols.insert(name.clone(), col);
            if let Some(observable) = node.measurement {
                observables.insert(name.clone(), observable);
            }
            if matches!(target, crate::MeasureTarget::Edge { .. }) {
                edge_records.insert(name.clone());
            }
        }

        Self {
            cols,
            observables,
            edge_records,
        }
    }

    pub(super) fn sorted_columns(&self) -> Vec<usize> {
        let mut cols = self.cols.values().copied().collect::<Vec<_>>();
        cols.sort_unstable();
        cols
    }

    pub(super) fn column(&self, name: &str) -> usize {
        self.cols[name]
    }

    fn basis_overlap(&self, name: &str) -> Option<Pauli> {
        match self.observables.get(name).copied() {
            Some(MeasurementObservable::Concrete(PauliBasis::X)) => Some(Pauli::X),
            Some(MeasurementObservable::Concrete(PauliBasis::Z)) => Some(Pauli::Z),
            Some(MeasurementObservable::Concrete(PauliBasis::Y))
            | Some(MeasurementObservable::Selective(_)) => Some(Pauli::Y),
            None => None,
        }
    }

    fn support_conflicts(&self, name: &str, pauli: Pauli) -> bool {
        pauli != Pauli::I && self.basis_overlap(name).is_some_and(|basis| pauli & basis)
    }

    /// Whether the record's column carries its measured component. On an edge,
    /// this includes `Y` carrying Z parity plus X transport (§2 of input model).
    pub(super) fn support_matches(&self, name: &str, pauli: Pauli) -> bool {
        match self.basis_overlap(name) {
            Some(Pauli::Y) => pauli != Pauli::I,
            Some(basis) if self.edge_records.contains(name) => pauli & basis,
            Some(basis) => pauli == basis,
            None => false,
        }
    }

    /// Exact Pauli values admitted by `support_matches`.
    pub(crate) fn matching_paulis(&self, name: &str) -> Vec<Pauli> {
        match self.basis_overlap(name) {
            Some(Pauli::Y) => vec![Pauli::Y, Pauli::X, Pauli::Z],
            Some(basis) if self.edge_records.contains(name) => vec![basis, Pauli::Y],
            Some(basis) => vec![basis],
            None => Vec::new(),
        }
    }

    /// `support_matches` without edge transport.
    fn support_matches_exactly(&self, name: &str, pauli: Pauli) -> bool {
        match self.basis_overlap(name) {
            Some(Pauli::Y) => pauli != Pauli::I,
            Some(basis) => pauli == basis,
            None => false,
        }
    }

    fn support_is_isolated(&self, name: &str, row: &PauliString) -> bool {
        self.support_matches(name, row.get(self.column(name)))
            && self.cols.iter().all(|(other_name, &col)| {
                !self.support_conflicts(other_name, row.get(col)) || other_name == name
            })
    }
}

/// Readers that would close a cycle if a candidate preceded them.
#[derive(Default)]
struct SelfReaders {
    /// Per reachable feedback, its targets as `(node column, Pauli)`.
    feedbacks: Vec<Vec<(usize, Pauli)>>,
    /// Reachable resolve targets, as cap columns.
    caps: Vec<usize>,
    /// Columns owned by each reachable terminal branch region.
    branches: Vec<Vec<usize>>,
}

impl SelfReaders {
    fn collect(
        zx: &ZXGraph,
        name: &str,
        prior_surfaces: &[MeasurementSurfaceRow],
        selective_constraints: &[SelectiveConstraint],
        fixings: &[(PauliString, StabilizerRowKind)],
    ) -> Self {
        let dag = zx.action_graph();
        let Some(origin) = dag
            .ordered_nodes()
            .find(|node| {
                matches!(&node.action, crate::Action::Measure { name: measured, .. } if measured == name)
            })
            .map(|node| node.ordinal)
        else {
            return Self::default();
        };

        let mut successors = vec![Vec::new(); dag.ordered_nodes().count()];
        for (from, to, _) in dag.dependencies() {
            successors[from].push(to);
        }
        for surface in prior_surfaces {
            let Some(measurement) = dag
                .ordered_nodes()
                .find(|node| {
                    matches!(&node.action, crate::Action::Measure { name, .. } if name == &surface.name)
                })
                .map(|node| node.ordinal)
            else {
                continue;
            };
            let (reached, active_fixings) =
                reachable_fixing_support(&surface.row, zx, selective_constraints, fixings);
            for node in dag.ordered_nodes() {
                let precedes = match &node.action {
                    crate::Action::Feedback { targets, .. } => feedback_perturbs_row(
                        &surface.row,
                        &feedback_columns(zx, targets),
                        fixings,
                        &active_fixings,
                    ),
                    crate::Action::Resolve { target, .. } => zx
                        .node_at(*target)
                        .is_some_and(|node| reached.contains(&node.id)),
                    crate::Action::Branch { target, .. } => {
                        dag.branch_region(*target).is_some_and(|region| {
                            row_or_active_fixings_touches_columns(
                                &surface.row,
                                &branch_region_columns(zx, region),
                                fixings,
                                &active_fixings,
                            )
                        })
                    }
                    _ => false,
                };
                if precedes {
                    successors[node.ordinal].push(measurement);
                }
            }
        }

        let mut reachable = vec![false; successors.len()];
        let mut pending = vec![origin];
        while let Some(from) = pending.pop() {
            for &reader in &successors[from] {
                if !reachable[reader] {
                    reachable[reader] = true;
                    pending.push(reader);
                }
            }
        }

        let mut readers = Self::default();
        for node in dag.ordered_nodes().filter(|node| reachable[node.ordinal]) {
            match &node.action {
                crate::Action::Feedback { targets, .. } => {
                    readers.feedbacks.push(feedback_columns(zx, targets));
                }
                crate::Action::Resolve { target, .. } => {
                    if let Some(node) = zx.node_at(*target) {
                        readers.caps.push(node.id);
                    }
                }
                crate::Action::Branch { target, .. } => {
                    if let Some(region) = dag.branch_region(*target) {
                        readers.branches.push(branch_region_columns(zx, region));
                    }
                }
                _ => {}
            }
        }
        readers
    }

    fn is_empty(&self) -> bool {
        self.feedbacks.is_empty() && self.caps.is_empty() && self.branches.is_empty()
    }

    /// Whether `row` — cross centres reconstructed — would add an implicit edge
    /// from a reachable action back to this record and close a cycle.
    fn traps(
        &self,
        row: &PauliString,
        zx: &ZXGraph,
        selective_constraints: &[SelectiveConstraint],
        fixings: &[(PauliString, StabilizerRowKind)],
    ) -> bool {
        let (reached, active_fixings) =
            reachable_fixing_support(row, zx, selective_constraints, fixings);

        self.caps.iter().any(|col| reached.contains(col))
            || self.branches.iter().any(|columns| {
                row_or_active_fixings_touches_columns(row, columns, fixings, &active_fixings)
            })
            || self
                .feedbacks
                .iter()
                .any(|targets| feedback_perturbs_row(row, targets, fixings, &active_fixings))
    }
}

fn feedback_columns(zx: &ZXGraph, targets: &[crate::FeedbackTarget]) -> Vec<(usize, Pauli)> {
    targets
        .iter()
        .filter_map(|target| zx.feedback_column(target))
        .collect()
}

fn odd_anticommutes(row: &PauliString, targets: &[(usize, Pauli)]) -> bool {
    targets
        .iter()
        .filter(|&&(col, pauli)| row.get(col).anticommutes(pauli))
        .count()
        % 2
        == 1
}

fn feedback_perturbs_row(
    row: &PauliString,
    targets: &[(usize, Pauli)],
    fixings: &[(PauliString, StabilizerRowKind)],
    active_fixings: &[bool],
) -> bool {
    odd_anticommutes(row, targets)
        || fixings
            .iter()
            .zip(active_fixings)
            .any(|((fixing_row, _), &active)| active && odd_anticommutes(fixing_row, targets))
}

fn reachable_fixing_support(
    row: &PauliString,
    zx: &ZXGraph,
    selective_constraints: &[SelectiveConstraint],
    fixings: &[(PauliString, StabilizerRowKind)],
) -> (HashSet<usize>, Vec<bool>) {
    let mut reached = selective_constraints
        .iter()
        .filter(|constraint| row.get(constraint.col) != Pauli::I)
        .map(|constraint| constraint.col)
        .collect::<HashSet<_>>();
    let mut active_fixings = vec![false; fixings.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for (index, (fixing_row, kind)) in fixings.iter().enumerate() {
            if active_fixings[index]
                || !kind.selective_fixing_targets().iter().any(|target| {
                    zx.node_at(target.pos)
                        .is_some_and(|node| reached.contains(&node.id))
                })
            {
                continue;
            }
            active_fixings[index] = true;
            changed = true;
            reached.extend(
                selective_constraints
                    .iter()
                    .filter(|constraint| fixing_row.get(constraint.col) != Pauli::I)
                    .map(|constraint| constraint.col),
            );
        }
    }
    (reached, active_fixings)
}

fn branch_region_columns(zx: &ZXGraph, region: &crate::BranchRegion) -> Vec<usize> {
    let mut columns = zx
        .nodes
        .iter()
        .filter(|node| region.contains_block(node.pos))
        .map(|node| node.id)
        .collect::<Vec<_>>();
    columns.extend(zx.edges.iter().filter_map(|edge| {
        let first = zx.nodes[edge.n1].pos;
        let second = zx.nodes[edge.n2].pos;
        (region.contains_block(first) || region.contains_block(second)).then_some(edge.id)
    }));
    columns.sort_unstable();
    columns.dedup();
    columns
}

fn row_or_active_fixings_touches_columns(
    row: &PauliString,
    columns: &[usize],
    fixings: &[(PauliString, StabilizerRowKind)],
    active_fixings: &[bool],
) -> bool {
    columns.iter().any(|&column| row.get(column) != Pauli::I)
        || fixings
            .iter()
            .zip(active_fixings)
            .any(|((fixing, _), &active)| {
                active && columns.iter().any(|&column| fixing.get(column) != Pauli::I)
            })
}

struct MeasurementAttemptContext<'a> {
    metadata: &'a MeasurementMetadata,
    self_readers: &'a SelfReaders,
    self_reader_fixings: &'a [(PauliString, StabilizerRowKind)],
    /// Prefer witnesses that do not close a cycle through a reachable action.
    avoid_self_readers: bool,
    selective_constraints: &'a [SelectiveConstraint],
    name: &'a str,
    limit_z: i32,
    support_forbidden: &'a [usize],
    /// Pivots for other measurement columns.
    conflicting_pivots: &'a [(usize, Pauli)],
    require_isolated: bool,
    /// Repair selective support to an allowed Pauli with kernel rows.
    repair_selective: bool,
    /// Admit parity plus transport on an edge record.
    component_match: bool,
}

struct SelfReaderContext<'a> {
    fixings: &'a [(PauliString, StabilizerRowKind)],
    prior_surfaces: &'a [MeasurementSurfaceRow],
}

pub(super) fn measurement_specs(zx: &ZXGraph) -> Vec<MeasurementSpec> {
    let mut specs = zx
        .action_graph()
        .ordered_nodes()
        .filter_map(|node| match &node.action {
            crate::Action::Measure { target, name } if zx.measurement_column(target).is_some() => {
                let minimum_support_z = match target {
                    crate::MeasureTarget::Node(pos) => pos.z,
                    crate::MeasureTarget::Edge { src, .. } => src.z,
                };
                Some(MeasurementSpec {
                    name: name.clone(),
                    minimum_support_z,
                    deadline: i64::from(minimum_support_z) + 1,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    specs.sort_by(|lhs, rhs| {
        lhs.deadline
            .cmp(&rhs.deadline)
            .then_with(|| lhs.name.cmp(&rhs.name))
    });
    specs
}

pub(super) fn extract_measurement_prefix_with_constraints(
    zx: &ZXGraph,
    basis: &mut TrackedBasis,
    metadata: &MeasurementMetadata,
    selective_constraints: &[SelectiveConstraint],
    self_reader_fixings: &[(PauliString, StabilizerRowKind)],
    specs: &[MeasurementSpec],
    budget: &mut SearchBudget,
) -> Result<Vec<MeasurementSurfaceRow>, StabilizerError> {
    let mut surfaces = Vec::with_capacity(specs.len());

    for spec in specs {
        let prefix_len = surfaces.len();
        let surface = construct_measurement_surface_row_from_basis(
            basis,
            prefix_len,
            zx,
            metadata,
            selective_constraints,
            &SelfReaderContext {
                fixings: self_reader_fixings,
                prior_surfaces: &surfaces,
            },
            spec,
            budget,
        )?;
        surfaces.push(surface);
    }

    Ok(surfaces)
}

/// Extracts measurement rows from one time-ordered factorization.
///
/// Temporal pivots are ordered by their decode gate. At each record deadline,
/// their suffix is exactly the subspace with no future physical support. A
/// chosen row is consumed from that live suffix, so it may carry later records
/// without ever extending past its own deadline. Any failed certificate leaves
/// the exact sequential search to the caller.
pub(super) fn extract_temporal_measurement_prefix(
    zx: &ZXGraph,
    basis_rows: &[PauliString],
    metadata: &MeasurementMetadata,
    selective_constraints: &[SelectiveConstraint],
    specs: &[MeasurementSpec],
    budget: &mut SearchBudget,
) -> Result<Option<Vec<MeasurementSurfaceRow>>, StabilizerError> {
    budget.check_matrix(
        basis_rows.len().saturating_mul(2),
        zx.total_ids(),
        basis_rows.len(),
        basis_rows.len(),
    )?;
    budget.visit("temporal measurement search states")?;
    let mut basis = TrackedBasis::new(basis_rows.to_vec());
    let constraints = temporal_axis_constraints(zx, selective_constraints);
    let mut release_gates = prefactor_temporal_basis(&mut basis, &constraints);
    let mut surfaces = Vec::with_capacity(specs.len());

    for spec in specs {
        budget.visit("temporal measurement search states")?;
        let prefix_len = surfaces.len();
        let consumed = release_gates[prefix_len..]
            .iter()
            .take_while(|&&gate| gate > spec.deadline)
            .count();
        let live = prefix_len + consumed;
        let target_rows = target_support_candidates_in_place(
            &mut basis.rows[live..],
            &mut basis.coeffs[live..],
            metadata,
            &spec.name,
            true,
        );
        let mut accepted = None;
        for (candidate, replacement) in target_rows.candidates {
            let SurfaceCandidate {
                mut row,
                mut combination,
            } = candidate;
            let needs_repair = selective_constraints
                .iter()
                .any(|constraint| row.get(constraint.col) == constraint.forbidden);
            if needs_repair
                && !repair_selective_support_in_place(
                    &mut row,
                    &mut combination,
                    &mut basis.rows[live + target_rows.count..],
                    &mut basis.coeffs[live + target_rows.count..],
                    selective_constraints,
                )
            {
                continue;
            }
            zx.reconstruct_cross_center(std::slice::from_mut(&mut row));
            if !row_respects_measurement_support_constraints(&row, &[], selective_constraints)
                || !measurement_surface_row_matches_acceptance(
                    zx, metadata, &spec.name, &row, false,
                )
            {
                continue;
            }
            let deadline = accepted_surface_deadline(zx, selective_constraints, spec, &row);
            if deadline > spec.deadline {
                continue;
            }
            accepted = Some((row, combination, deadline, replacement));
            break;
        }
        let Some((row, combination, deadline, replacement)) = accepted else {
            return Ok(None);
        };
        let replace_index = consumed + replacement;
        basis.rows[prefix_len + replace_index] = row.clone();
        basis.coeffs[prefix_len + replace_index] = combination.clone();
        release_gates[prefix_len + replace_index] = deadline;
        if replace_index != 0 {
            basis.rows[prefix_len..=prefix_len + replace_index].rotate_right(1);
            basis.coeffs[prefix_len..=prefix_len + replace_index].rotate_right(1);
            release_gates[prefix_len..=prefix_len + replace_index].rotate_right(1);
        }
        isolate_measurement_target_in_suffix(
            &mut basis.rows[prefix_len..],
            &mut basis.coeffs[prefix_len..],
            metadata.column(&spec.name),
            metadata.basis_overlap(&spec.name).unwrap_or(Pauli::Y),
        );
        surfaces.push(MeasurementSurfaceRow {
            name: spec.name.clone(),
            deadline,
            row,
            combination,
        });
    }

    Ok(Some(surfaces))
}

pub(super) fn temporal_projection_layout(
    zx: &ZXGraph,
    metadata: &MeasurementMetadata,
    selective_constraints: &[SelectiveConstraint],
) -> (Vec<usize>, Vec<Option<i64>>) {
    let mut semantic_columns = zx
        .nodes
        .iter()
        .filter(|node| node.kind.is_boundary())
        .map(|node| node.id)
        .chain(metadata.cols.values().copied())
        .chain(
            selective_constraints
                .iter()
                .map(|constraint| constraint.col),
        )
        .collect::<Vec<_>>();
    semantic_columns.sort_unstable();
    semantic_columns.dedup();
    (
        semantic_columns,
        temporal_node_gates(zx, selective_constraints),
    )
}

fn prefactor_temporal_basis(
    basis: &mut TrackedBasis,
    constraints: &[(i64, usize, Pauli)],
) -> Vec<i64> {
    let mut solved = 0;
    let mut release_gates = Vec::with_capacity(basis.rows.len());
    for &(gate, col, axis) in constraints {
        let Some(pivot_index) =
            (solved..basis.rows.len()).find(|&index| basis.rows[index].get(col) & axis)
        else {
            continue;
        };
        if pivot_index != solved {
            basis.rows.swap(pivot_index, solved);
            basis.coeffs.swap(pivot_index, solved);
        }
        let (before, pivot_and_after) = basis.rows.split_at_mut(solved);
        let (pivot_row, after) = pivot_and_after
            .split_first_mut()
            .expect("solved pivot is in bounds");
        let (before_coeffs, pivot_and_after_coeffs) = basis.coeffs.split_at_mut(solved);
        let (pivot_coeff, after_coeffs) = pivot_and_after_coeffs
            .split_first_mut()
            .expect("tracked pivot is in bounds");
        for (row, coeff) in before
            .iter_mut()
            .zip(before_coeffs)
            .chain(after.iter_mut().zip(after_coeffs))
        {
            if row.get(col) & axis {
                *row ^= &*pivot_row;
                coeff.xor_assign(pivot_coeff);
            }
        }
        release_gates.push(gate);
        solved += 1;
    }
    release_gates.resize(basis.rows.len(), i64::MIN);
    release_gates
}

fn temporal_axis_constraints(
    zx: &ZXGraph,
    selective_constraints: &[SelectiveConstraint],
) -> Vec<(i64, usize, Pauli)> {
    let node_gates = temporal_node_gates(zx, selective_constraints);
    let columns = zx
        .nodes
        .iter()
        .filter_map(|node| node_gates[node.id].map(|gate| (gate, node.id)))
        .chain(zx.edges.iter().filter_map(|edge| {
            node_gates[edge.n1]
                .into_iter()
                .chain(node_gates[edge.n2])
                .max()
                .map(|gate| (gate, edge.id))
        }));
    let mut constraints = columns
        .flat_map(|(gate, col)| [Pauli::X, Pauli::Z].map(move |axis| (gate, col, axis)))
        .collect::<Vec<_>>();
    constraints.sort_unstable_by(|lhs, rhs| {
        rhs.0
            .cmp(&lhs.0)
            .then_with(|| lhs.1.cmp(&rhs.1))
            .then_with(|| u8::from(lhs.2).cmp(&u8::from(rhs.2)))
    });
    constraints
}

fn temporal_node_gates(
    zx: &ZXGraph,
    selective_constraints: &[SelectiveConstraint],
) -> Vec<Option<i64>> {
    let selective_cols = selective_constraints
        .iter()
        .map(|constraint| constraint.col)
        .collect::<HashSet<_>>();
    zx.nodes
        .iter()
        .map(|node| {
            (!node.is_output_port(zx)).then(|| {
                let z = i64::from(node.pos.z);
                if selective_cols.contains(&node.id) {
                    z
                } else {
                    z + 1
                }
            })
        })
        .collect()
}

#[expect(
    clippy::too_many_arguments,
    reason = "surface construction needs the independent search inputs"
)]
fn construct_measurement_surface_row_from_basis(
    basis: &mut TrackedBasis,
    prefix_len: usize,
    zx: &ZXGraph,
    metadata: &MeasurementMetadata,
    selective_constraints: &[SelectiveConstraint],
    self_reader_context: &SelfReaderContext<'_>,
    spec: &MeasurementSpec,
    budget: &mut SearchBudget,
) -> Result<MeasurementSurfaceRow, StabilizerError> {
    let max_limit_z = zx.nodes.iter().map(|node| node.pos.z).max().unwrap_or(0);
    // Forbidden node/edge columns change only at an endpoint's z. Keep the
    // original first attempt and skip empty coordinate gaps between changes.
    let mut support_limits = zx
        .nodes
        .iter()
        .map(|node| node.pos.z)
        .filter(|&z| z >= spec.minimum_support_z)
        .collect::<Vec<_>>();
    if spec.minimum_support_z <= max_limit_z {
        support_limits.push(spec.minimum_support_z);
    }
    support_limits.sort_unstable();
    support_limits.dedup();
    // Depends only on the metadata and this measurement's name, so it is
    // invariant across the support-limit sweep below.
    let conflicting_pivots = conflicting_measurement_pivots(metadata, &spec.name);
    let self_readers = SelfReaders::collect(
        zx,
        &spec.name,
        self_reader_context.prior_surfaces,
        selective_constraints,
        self_reader_context.fixings,
    );

    // Prefer cycle-safe, exact-edge, output-free, isolated, early surfaces, in
    // that order. The last cycle sweep preserves `DependencyCycle` diagnostics.
    let self_reader_sweeps = if self_readers.is_empty() { 1 } else { 2 };
    for avoid_self_readers in [true, false].into_iter().take(self_reader_sweeps) {
        for component_match in [false, true] {
            for allow_output_support in [false, true] {
                for require_isolated in [true, false] {
                    for &limit_z in &support_limits {
                        let support_forbidden =
                            support_forbidden_columns(zx, limit_z, allow_output_support);
                        let mut attempt = |support_forbidden: &[usize], repair_selective: bool| {
                            attempt_measurement_surface_row(
                                basis,
                                prefix_len,
                                zx,
                                &MeasurementAttemptContext {
                                    metadata,
                                    self_readers: &self_readers,
                                    self_reader_fixings: self_reader_context.fixings,
                                    avoid_self_readers,
                                    selective_constraints,
                                    name: &spec.name,
                                    limit_z,
                                    support_forbidden,
                                    conflicting_pivots: &conflicting_pivots,
                                    require_isolated,
                                    repair_selective,
                                    component_match,
                                },
                                budget,
                            )
                        };
                        // Try selective support unconstrained, pivoted to I, then repaired.
                        let mut result = attempt(&support_forbidden, false)?;
                        if matches!(result, MeasurementConstructionAttempt::Unavailable) {
                            let mut selective_free = support_forbidden.clone();
                            selective_free.extend(
                                selective_constraints
                                    .iter()
                                    .map(|constraint| constraint.col),
                            );
                            selective_free.sort_unstable();
                            selective_free.dedup();
                            if selective_free != support_forbidden {
                                result = attempt(&selective_free, false)?;
                            }
                        }
                        if matches!(result, MeasurementConstructionAttempt::Unavailable)
                            && !selective_constraints.is_empty()
                        {
                            result = attempt(&support_forbidden, true)?;
                        }
                        match result {
                            MeasurementConstructionAttempt::Accepted {
                                basis: accepted_basis,
                                mut surface,
                            } => {
                                surface.deadline = accepted_surface_deadline(
                                    zx,
                                    selective_constraints,
                                    spec,
                                    &surface.row,
                                );
                                *basis = accepted_basis;
                                return Ok(surface);
                            }
                            MeasurementConstructionAttempt::Unavailable => {}
                        }
                    }
                }
            }
        }
    }

    Err(StabilizerError::MeasurementSurfaceUnavailable {
        mvar: spec.name.to_string(),
    })
}

/// Decode deadline from actual support, floored by `spec.deadline`. Ordinary
/// nodes gate at z+1, selective sites at z, and edges use both endpoints.
pub(super) fn accepted_surface_deadline(
    zx: &ZXGraph,
    selective_constraints: &[SelectiveConstraint],
    spec: &MeasurementSpec,
    row: &PauliString,
) -> i64 {
    let selective_cols = selective_constraints
        .iter()
        .map(|constraint| constraint.col)
        .collect::<std::collections::HashSet<_>>();
    // Output anchors become frame constraints, not awaited records.
    let node_gate = |node_id: usize| {
        let node = &zx.nodes[node_id];
        if node.is_output_port(zx) {
            return None;
        }
        let z = i64::from(node.pos.z);
        Some(if selective_cols.contains(&node_id) {
            z
        } else {
            z + 1
        })
    };

    let mut deadline = spec.deadline;
    for (col, _) in row.iter_support() {
        if col < zx.nodes.len() {
            if let Some(gate) = node_gate(col) {
                deadline = deadline.max(gate);
            }
        } else {
            let edge = zx.edge_by_id(col);
            for gate in [node_gate(edge.n1), node_gate(edge.n2)]
                .into_iter()
                .flatten()
            {
                deadline = deadline.max(gate);
            }
        }
    }
    deadline
}

fn attempt_measurement_surface_row(
    committed_basis: &TrackedBasis,
    prefix_len: usize,
    zx: &ZXGraph,
    context: &MeasurementAttemptContext<'_>,
    budget: &mut SearchBudget,
) -> Result<MeasurementConstructionAttempt, StabilizerError> {
    budget.visit("measurement surface search states")?;
    budget.check_matrix(
        committed_basis.rows.len().saturating_mul(4),
        zx.total_ids(),
        committed_basis.coeffs.len().saturating_mul(4),
        committed_basis.coeffs.first().map_or(0, CoeffVec::len),
    )?;
    let mut scratch_rows = committed_basis.rows[prefix_len..].to_vec();
    let mut scratch_coeffs = committed_basis.coeffs[prefix_len..].to_vec();
    if scratch_coeffs.is_empty() {
        return Ok(MeasurementConstructionAttempt::Unavailable);
    }

    let mut pivot_constraints = axis_pivot_constraints(context.support_forbidden.iter().copied());
    if context.require_isolated {
        pivot_constraints.extend(context.conflicting_pivots.iter().copied());
    }
    let consumed = gaussian_elimination_with_tracking(
        &mut scratch_rows,
        &mut scratch_coeffs,
        pivot_constraints,
        |row, &(col, basis)| row.get(col) & basis,
        None,
    );

    let search = target_support_candidates(
        scratch_rows[consumed..].to_vec(),
        scratch_coeffs[consumed..].to_vec(),
        context.metadata,
        context.name,
        context.component_match,
    );
    if search.candidates.is_empty() {
        return Ok(MeasurementConstructionAttempt::Unavailable);
    }

    for candidate in search.candidates {
        budget.visit("measurement surface search states")?;
        let SurfaceCandidate {
            mut row,
            mut combination,
        } = candidate;
        if context.repair_selective
            && !repair_selective_support(
                &mut row,
                &mut combination,
                &search.kernel_rows,
                &search.kernel_coeffs,
                context.selective_constraints,
            )
        {
            continue;
        }
        zx.reconstruct_cross_center(std::slice::from_mut(&mut row));
        if (!context.avoid_self_readers
            || !context.self_readers.traps(
                &row,
                zx,
                context.selective_constraints,
                context.self_reader_fixings,
            ))
            && row_respects_measurement_support_constraints(
                &row,
                context.support_forbidden,
                context.selective_constraints,
            )
            && measurement_surface_row_matches_acceptance(
                zx,
                context.metadata,
                context.name,
                &row,
                context.require_isolated,
            )
        {
            let Some(replace_index) = replace_row_with_candidate_preserving_rank(
                &mut scratch_rows,
                &mut scratch_coeffs,
                consumed,
                &row,
                &combination,
            ) else {
                continue;
            };

            if replace_index != 0 {
                scratch_rows.swap(replace_index, 0);
                scratch_coeffs.swap(replace_index, 0);
            }

            isolate_measurement_target_in_suffix(
                &mut scratch_rows,
                &mut scratch_coeffs,
                context.metadata.column(context.name),
                context
                    .metadata
                    .basis_overlap(context.name)
                    .unwrap_or(Pauli::Y),
            );

            let mut accepted_basis = committed_basis.clone();
            accepted_basis.replace_suffix(prefix_len, &scratch_rows, &scratch_coeffs);
            return Ok(MeasurementConstructionAttempt::Accepted {
                basis: accepted_basis,
                surface: MeasurementSurfaceRow {
                    name: context.name.to_string(),
                    // Refined from the accepted row by the caller.
                    deadline: i64::from(context.limit_z) + 1,
                    row,
                    combination,
                },
            });
        }
    }

    Ok(MeasurementConstructionAttempt::Unavailable)
}

fn row_respects_measurement_support_constraints(
    row: &PauliString,
    support_forbidden: &[usize],
    selective_constraints: &[SelectiveConstraint],
) -> bool {
    support_forbidden
        .iter()
        .all(|&col| row.get(col) == Pauli::I)
        && selective_constraints
            .iter()
            .all(|constraint| row.get(constraint.col) != constraint.forbidden)
}

fn support_forbidden_columns(zx: &ZXGraph, limit_z: i32, allow_output_support: bool) -> Vec<usize> {
    let mut forbidden = Vec::new();

    for node in &zx.nodes {
        if node.is_output_port(zx) && !allow_output_support {
            forbidden.push(node.id);
            continue;
        }
        if node.pos.z > limit_z {
            forbidden.push(node.id);
        }
    }

    for edge in &zx.edges {
        let n1_pos = zx.nodes[edge.n1].pos;
        let n2_pos = zx.nodes[edge.n2].pos;
        if n1_pos.z > limit_z || n2_pos.z > limit_z {
            forbidden.push(edge.id);
        }
    }

    forbidden.sort_unstable();
    forbidden.dedup();
    forbidden
}

#[derive(Debug, Clone)]
struct SurfaceCandidate {
    row: PauliString,
    combination: CoeffVec,
}

/// The at most three rows that carry the record, plus the kernel they are free
/// to move in.
struct TargetSupportSearch {
    candidates: Vec<SurfaceCandidate>,
    /// Rows with no support at the target column. Adding any of them leaves the
    /// record intact, which is the freedom `repair_selective_support` spends.
    kernel_rows: Vec<PauliString>,
    kernel_coeffs: Vec<CoeffVec>,
}

struct TemporalTargetSupportSearch {
    candidates: Vec<(SurfaceCandidate, usize)>,
    count: usize,
}

fn target_support_candidates_in_place(
    rows: &mut [PauliString],
    combinations: &mut [CoeffVec],
    metadata: &MeasurementMetadata,
    name: &str,
    component_match: bool,
) -> TemporalTargetSupportSearch {
    let target_col = metadata.column(name);
    let count = gaussian_elimination_with_tracking(
        rows,
        combinations,
        axis_pivot_constraints([target_col]),
        |row, &(col, basis)| row.get(col) & basis,
        None,
    );
    debug_assert!(count <= 2, "one Pauli column has at most X/Z rank");

    let make_candidate = |row: &PauliString, combination: &CoeffVec| {
        target_support_candidate(
            metadata,
            name,
            target_col,
            row,
            combination,
            component_match,
        )
    };
    let mut candidates = Vec::new();
    for (replacement, (row, combination)) in
        rows[..count].iter().zip(&combinations[..count]).enumerate()
    {
        if let Some(candidate) = make_candidate(row, combination) {
            candidates.push((candidate, replacement));
        }
    }
    if count == 2 {
        let mut row = rows[0].clone();
        row ^= &rows[1];
        let mut combination = combinations[0].clone();
        combination.xor_assign(&combinations[1]);
        if let Some(candidate) = make_candidate(&row, &combination) {
            candidates.push((candidate, 0));
        }
    }
    TemporalTargetSupportSearch { candidates, count }
}
fn target_support_candidates(
    rows: Vec<PauliString>,
    combinations: Vec<CoeffVec>,
    metadata: &MeasurementMetadata,
    name: &str,
    component_match: bool,
) -> TargetSupportSearch {
    let mut rows = rows;
    let mut combinations = combinations;
    let search = target_support_candidates_in_place(
        &mut rows,
        &mut combinations,
        metadata,
        name,
        component_match,
    );
    let kernel_rows = rows.split_off(search.count);
    let kernel_coeffs = combinations.split_off(search.count);
    TargetSupportSearch {
        candidates: search
            .candidates
            .into_iter()
            .map(|(candidate, _)| candidate)
            .collect(),
        kernel_rows,
        kernel_coeffs,
    }
}

/// Adds kernel rows to `row` until no selective column carries its forbidden
/// Pauli, mirroring the additions on `combination`. Returns `false` when a
/// column that needs repair has no kernel freedom left.
///
/// Each pass repairs one violated column and then pivots that column out of the
/// remaining pool, so later passes are `I` there and cannot undo it. Columns
/// that are already satisfied keep their freedom: locking them up front would
/// reject rows that only need a *tolerated* Pauli there. Since every pass locks
/// a distinct column, one pass per constraint is enough.
///
/// Any pivot is non-`I` at its own column, and XOR-ing a non-`I` Pauli onto the
/// forbidden one lands on one of the other two, so the first pivot always
/// repairs the column and no search over pivot subsets is needed.
///
/// This is not a complete decision procedure: which pivot a pass picks, and in
/// which order columns lock, can still strand a satisfiable row. It subsumes
/// the two earlier stages, which is what the caller's ladder needs.
fn repair_selective_support(
    row: &mut PauliString,
    combination: &mut CoeffVec,
    kernel_rows: &[PauliString],
    kernel_coeffs: &[CoeffVec],
    constraints: &[SelectiveConstraint],
) -> bool {
    let mut rows = kernel_rows.to_vec();
    let mut coeffs = kernel_coeffs.to_vec();
    repair_selective_support_in_place(row, combination, &mut rows, &mut coeffs, constraints)
}

fn repair_selective_support_in_place(
    row: &mut PauliString,
    combination: &mut CoeffVec,
    rows: &mut [PauliString],
    coeffs: &mut [CoeffVec],
    constraints: &[SelectiveConstraint],
) -> bool {
    let mut free = 0;

    for _ in 0..constraints.len() {
        let Some(col) = constraints
            .iter()
            .find(|constraint| row.get(constraint.col) == constraint.forbidden)
            .map(|constraint| constraint.col)
        else {
            return true;
        };

        let solved = gaussian_elimination_with_tracking(
            &mut rows[free..],
            &mut coeffs[free..],
            axis_pivot_constraints([col]),
            |kernel_row, &(pivot_col, basis)| kernel_row.get(pivot_col) & basis,
            None,
        );
        if solved == 0 {
            return false;
        }

        *row ^= &rows[free];
        combination.xor_assign(&coeffs[free]);
        free += solved;
    }

    constraints
        .iter()
        .all(|constraint| row.get(constraint.col) != constraint.forbidden)
}

fn target_support_candidate(
    metadata: &MeasurementMetadata,
    name: &str,
    target_col: usize,
    row: &PauliString,
    combination: &CoeffVec,
    component_match: bool,
) -> Option<SurfaceCandidate> {
    let carries = if component_match {
        metadata.support_matches(name, row.get(target_col))
    } else {
        metadata.support_matches_exactly(name, row.get(target_col))
    };
    if !carries {
        return None;
    }

    Some(SurfaceCandidate {
        row: row.clone(),
        combination: combination.clone(),
    })
}

/// Finds the first witness slot (from `witness_start`) that `candidate` may
/// replace while preserving both the coefficient rank and the Pauli-row rank,
/// installing it there.
fn replace_row_with_candidate_preserving_rank(
    rows: &mut [PauliString],
    combinations: &mut [CoeffVec],
    witness_start: usize,
    candidate: &PauliString,
    candidate_combination: &CoeffVec,
) -> Option<usize> {
    let index = replacement_index_preserving_rank(
        rows,
        combinations,
        witness_start,
        candidate,
        candidate_combination,
    )?;
    rows[index] = candidate.clone();
    combinations[index] = candidate_combination.clone();
    Some(index)
}

fn replacement_index_preserving_rank(
    rows: &[PauliString],
    combinations: &[CoeffVec],
    witness_start: usize,
    candidate: &PauliString,
    candidate_combination: &CoeffVec,
) -> Option<usize> {
    debug_assert_eq!(rows.len(), combinations.len());
    // `combinations` track an independent stabilizer basis. If the candidate
    // lies in that span, its unique coordinates name exactly the members it
    // may replace; a candidate outside the span may replace any member.
    let coefficient_coordinates = solve_coeff_combination(combinations, candidate_combination);
    let pauli_coordinates = solve_pauli_combination(rows, candidate);
    for index in witness_start..rows.len() {
        if coefficient_coordinates
            .as_ref()
            .is_some_and(|coordinates| !coordinates.bit(index))
            || pauli_coordinates
                .as_ref()
                .is_some_and(|coordinates| !coordinates.bit(index))
        {
            continue;
        }

        return Some(index);
    }
    None
}

fn isolate_measurement_target_in_suffix(
    rows: &mut [PauliString],
    combinations: &mut [CoeffVec],
    target_col: usize,
    target_basis: Pauli,
) {
    if rows.is_empty() {
        return;
    }

    let (pivot_row, rows) = rows.split_first_mut().expect("nonempty suffix");
    let (pivot_combination, combinations) = combinations
        .split_first_mut()
        .expect("tracked suffix has matching length");
    for (row, combination) in rows.iter_mut().zip(combinations) {
        if row.get(target_col) & target_basis {
            *row ^= &*pivot_row;
            combination.xor_assign(pivot_combination);
        }
    }
}

fn conflicting_measurement_pivots(
    metadata: &MeasurementMetadata,
    name: &str,
) -> Vec<(usize, Pauli)> {
    let mut pivots = Vec::new();

    for (other_name, &col) in &metadata.cols {
        if other_name.as_str() == name {
            continue;
        }

        let Some(basis) = metadata.basis_overlap(other_name.as_str()) else {
            continue;
        };

        pivots.extend(basis.iter_xz().map(|axis| (col, axis)));
    }

    pivots.sort_unstable_by_key(|&(col, basis)| (col, u8::from(basis)));
    pivots
}

pub(super) fn measurement_surface_row_matches_acceptance(
    zx: &ZXGraph,
    metadata: &MeasurementMetadata,
    name: &str,
    row: &PauliString,
    require_isolated: bool,
) -> bool {
    let mut independent = row.clone();
    zx.clear_cross_centers(std::slice::from_mut(&mut independent));
    let target_col = metadata.column(name);
    zx.row_has_external_edge_pair_support(row)
        && metadata.support_matches(name, independent.get(target_col))
        && (!require_isolated || metadata.support_is_isolated(name, &independent))
}

/// Diagonalizes every named measurement row when possible, leaving the basis
/// unchanged when the rows cannot be isolated without invalidating a selective
/// fixing row.
pub(super) fn diagonalize_measurement_columns(
    final_basis: &mut TrackedBasis,
    measurement_names: &[String],
    metadata: &MeasurementMetadata,
    selective_constraints: &[SelectiveConstraint],
    fixing_kinds: &[StabilizerRowKind],
) {
    if measurement_names.is_empty() {
        return;
    }
    let original = final_basis.clone();
    for (index, name) in measurement_names.iter().enumerate() {
        let Some(target_basis) = metadata.basis_overlap(name) else {
            continue;
        };
        let target_col = metadata.column(name);
        let (before, pivot_and_after) = final_basis.rows.split_at_mut(index);
        let (pivot_row, after) = pivot_and_after
            .split_first_mut()
            .expect("measurement pivot is in bounds");
        let (before_coeffs, pivot_and_after_coeffs) = final_basis.coeffs.split_at_mut(index);
        let (pivot_coeff, after_coeffs) = pivot_and_after_coeffs
            .split_first_mut()
            .expect("tracked measurement pivot is in bounds");
        for (row, coeff) in before
            .iter_mut()
            .zip(before_coeffs)
            .chain(after.iter_mut().zip(after_coeffs))
        {
            if row.get(target_col) & target_basis {
                *row ^= &*pivot_row;
                coeff.xor_assign(pivot_coeff);
            }
        }
    }

    let measurements_are_isolated = measurement_names
        .iter()
        .enumerate()
        .all(|(index, name)| metadata.support_is_isolated(name, &final_basis.rows[index]));
    // Each fixing row must still own the forbidden Pauli at every site it was
    // tagged for. Indexed by push order rather than by constraint, since one
    // joint row can fix several sites.
    let fixing_rows_are_valid = fixing_kinds.iter().enumerate().all(|(offset, kind)| {
        let row = &final_basis.rows[measurement_names.len() + offset];
        kind.selective_fixing_targets().iter().all(|target| {
            selective_constraints
                .iter()
                .find(|constraint| constraint.pos == target.pos)
                .is_some_and(|constraint| row.get(constraint.col) == constraint.forbidden)
        })
    });
    if !measurements_are_isolated || !fixing_rows_are_valid {
        *final_basis = original;
    }
}
pub(crate) fn materialize_measurement_first_basis(
    zx: &ZXGraph,
    rows: Vec<PauliString>,
    row_kinds: Vec<StabilizerRowKind>,
    public_len: usize,
    row_phases: Vec<u8>,
    specs: &[MeasurementSpec],
) -> Result<StabilizerGenerators, StabilizerError> {
    assert_eq!(
        rows.len(),
        row_kinds.len(),
        "every stabilizer row has one role"
    );

    // Accepted deadline per named row: the layer its surface promised to close
    // by (`accepted_surface_deadline`, written back into the specs by
    // canonical table construction. Used to check the materialized surfaces
    // still honour that promise.
    let deadlines = specs
        .iter()
        .map(|spec| (spec.name.as_str(), spec.deadline))
        .collect::<HashMap<_, _>>();
    // Selective-fixing sites are exempt from the extent bound: their arm is
    // pinned by their own `resolve` and the surface is re-derived against the
    // filled basis at runtime, so a surface routing through one stays legal at
    // its own deadline (mirrors `accepted_surface_deadline`'s `node_gate`).
    let selective_positions = collect_selective_constraints(zx)
        .iter()
        .map(|constraint| constraint.pos)
        .collect::<std::collections::HashSet<_>>();

    assert!(public_len <= rows.len(), "public rows form a valid prefix");
    assert_eq!(rows.len(), row_phases.len(), "every row has one phase");
    let mut generators = Vec::with_capacity(public_len);
    let mut auxiliary_rows = Vec::new();
    for (index, (row, kind)) in rows.into_iter().zip(row_kinds).enumerate() {
        if index >= public_len {
            auxiliary_rows.push(row);
            continue;
        }
        let stabilizer = zx.pauli_string_to_stabilizer_with_row_phase(row, row_phases[index]);
        if let StabilizerRowKind::Measurement { name } = &kind {
            let deadline = *deadlines
                .get(name.as_str())
                .expect("every materialized measurement name has an accepted deadline");
            check_measurement_extent(name, &stabilizer, deadline, &selective_positions)?;
        }
        generators.push(StabilizerGenerator::new(stabilizer, kind));
    }

    Ok(StabilizerGenerators::from_parts(
        zx.clone(),
        generators,
        auxiliary_rows,
    ))
}

/// Checks a materialized measurement surface still closes by the deadline its
/// construction accepted: interior support at layer z makes the parity readable
/// only from layer z + 1 on, so non-exempt support must stay strictly below the
/// deadline. The transformations that run after acceptance (fixing rows, basis
/// completion, diagonalization) are supposed to leave the frozen measurement
/// prefix alone; this catches it when one does not.
///
/// Support past the *measure* layer is fine — the deadline moved with it.
/// Consumers are dataflow-scheduled after the resulting decode, independent of
/// their graph z coordinate. Output-port anchors are unbounded in graph
/// analysis and are not interior nodes; compiler preflight rejects them.
/// Selective-fixing sites are excluded (see `selective_positions` above).
fn check_measurement_extent(
    name: &str,
    stabilizer: &Stabilizer,
    deadline: i64,
    selective_positions: &std::collections::HashSet<glam::IVec3>,
) -> Result<(), StabilizerError> {
    let Some(max_z) = stabilizer
        .interior_nodes
        .keys()
        .filter(|pos| {
            !selective_positions.contains(*pos) && !stabilizer.port_stabilizer.contains_key(*pos)
        })
        .map(|pos| pos.z)
        .max()
    else {
        return Ok(());
    };
    if i64::from(max_z) >= deadline {
        return Err(StabilizerError::MeasurementSurfaceExtendsPastDeadline {
            name: name.to_string(),
            max_z,
            deadline,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::basis::reduce_to_basis;
    use glam::IVec3;

    use super::*;

    use crate::{BlockGraph, GalleryItem, SelectiveKind};

    fn construct_measurement_surface_row(
        external_generators: &[PauliString],
        zx: &ZXGraph,
        metadata: &MeasurementMetadata,
        name: &str,
        minimum_limit_z: i32,
    ) -> Result<MeasurementSurfaceRow, StabilizerError> {
        let mut basis = TrackedBasis::new(external_generators.to_vec());
        let selective_constraints = collect_selective_constraints(zx);
        let spec = MeasurementSpec {
            name: name.to_string(),
            minimum_support_z: minimum_limit_z,
            deadline: i64::from(minimum_limit_z),
        };
        construct_measurement_surface_row_from_basis(
            &mut basis,
            0,
            zx,
            metadata,
            &selective_constraints,
            &SelfReaderContext {
                fixings: &[],
                prior_surfaces: &[],
            },
            &spec,
            &mut SearchBudget::new(usize::MAX),
        )
    }

    #[test]
    fn measurement_specs_track_support_limit_separately_from_deadline() {
        let mut graph = BlockGraph::from_blog_text(
            "BLOG 1.0\n\n  0: ZXZ [0,0,1]\n  1: Port [0,0,2]\n  [0,0,1] -> +Z\n",
        )
        .unwrap();
        graph
            .set_actions_lenient(vec![crate::Action::Measure {
                target: crate::MeasureTarget::Node(glam::ivec3(0, 0, 1)),
                name: "m".into(),
            }])
            .unwrap();
        let zx = ZXGraph::from_block_graph_for_analysis(&graph).unwrap();

        assert_eq!(
            measurement_specs(&zx),
            vec![MeasurementSpec {
                name: "m".to_string(),
                minimum_support_z: 1,
                deadline: 2,
            }],
        );
    }

    #[test]
    fn t_comparison_keeps_both_named_measurements() {
        let graph = GalleryItem::TComparison
            .build()
            .materialize_root_graph()
            .expect("gallery flat projection");
        let zx = ZXGraph::try_from(&graph).unwrap();
        let metadata = MeasurementMetadata::collect(&zx);
        let basis_rows = reduce_to_basis(&zx.to_external_generator_table(), zx.total_ids);
        let specs = measurement_specs(&zx);
        let temporal = extract_temporal_measurement_prefix(
            &zx,
            &basis_rows,
            &metadata,
            &collect_selective_constraints(&zx),
            &specs,
            &mut SearchBudget::new(usize::MAX),
        )
        .unwrap()
        .expect("independent T records diagonalize together");

        assert!(metadata.cols.contains_key("cmp"));
        assert!(metadata.cols.contains_key("parity"));
        assert_eq!(
            specs.into_iter().map(|spec| spec.name).collect::<Vec<_>>(),
            ["cmp", "parity"]
        );
        assert_eq!(
            temporal
                .into_iter()
                .map(|surface| surface.name)
                .collect::<Vec<_>>(),
            ["cmp", "parity"]
        );
        let stabilizers = zx
            .stabilizers()
            .expect("T boundaries remain open during stabilizer derivation");
        assert_eq!(
            stabilizers
                .generators
                .iter()
                .filter_map(StabilizerGenerator::measurement_name)
                .collect::<Vec<_>>(),
            ["cmp", "parity"]
        );
        let selective_pos = glam::ivec3(1, 0, 2);
        let fixing = stabilizers
            .generators
            .iter()
            .find(|generator| generator.kind.fixes_selective(selective_pos))
            .expect("selective measurement keeps its fixing row");
        assert_eq!(fixing.stabilizer.interior_nodes[&selective_pos], Pauli::Z);
    }

    #[test]
    fn supported_deadlines_match_all_column_reference() {
        for item in [
            GalleryItem::THTH,
            GalleryItem::CCZFactoryWithTels,
            GalleryItem::GHZ,
        ] {
            let zx = ZXGraph::try_from(
                &item
                    .build()
                    .materialize_root_graph()
                    .expect("gallery flat projection"),
            )
            .unwrap();
            let constraints = collect_selective_constraints(&zx);
            let gate = |id: usize| {
                let node = &zx.nodes[id];
                (!node.is_output_port(&zx)).then(|| {
                    i64::from(node.pos.z)
                        + i64::from(!constraints.iter().any(|constraint| constraint.col == id))
                })
            };
            let mut rows = (0..zx.total_ids)
                .map(|col| PauliString::from_terms(zx.total_ids, [(col, Pauli::Y)]))
                .collect::<Vec<_>>();
            rows.push(PauliString::new(zx.total_ids));
            rows.push(PauliString::from_terms(
                zx.total_ids,
                (0..zx.total_ids).map(|col| (col, Pauli::Y)),
            ));
            for deadline in [-5, 0, 7, i64::from(i32::MAX) + 1] {
                let spec = MeasurementSpec {
                    name: "m".into(),
                    minimum_support_z: 0,
                    deadline,
                };
                for row in &rows {
                    let expected = zx
                        .nodes
                        .iter()
                        .filter(|node| row.get(node.id) != Pauli::I)
                        .filter_map(|node| gate(node.id))
                        .chain(
                            zx.edges
                                .iter()
                                .filter(|edge| row.get(edge.id) != Pauli::I)
                                .flat_map(|edge| {
                                    [gate(edge.n1), gate(edge.n2)].into_iter().flatten()
                                }),
                        )
                        .fold(deadline, i64::max);
                    assert_eq!(
                        accepted_surface_deadline(&zx, &constraints, &spec, row),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn measurement_deadline_after_i32_max_layer_is_representable() {
        let position = glam::IVec3::new(0, 0, i32::MAX);
        let mut graph = BlockGraph::new();
        graph
            .try_add_block(crate::Block::new(
                position,
                crate::BlockKind::Cube(crate::CubeKind::ZXZ),
            ))
            .unwrap();
        graph
            .add_action(crate::Action::Measure {
                target: crate::MeasureTarget::Node(position),
                name: "m".into(),
            })
            .unwrap();
        let zx = ZXGraph::try_from(&graph).unwrap();
        let spec = measurement_specs(&zx).pop().unwrap();
        assert_eq!(spec.deadline, i64::from(i32::MAX) + 1);

        let node = zx.nodes.iter().find(|node| node.pos == position).unwrap();
        let mut row = PauliString::new(zx.total_ids);
        row.set(node.id, Pauli::Z);
        assert_eq!(
            accepted_surface_deadline(&zx, &[], &spec, &row),
            i64::from(i32::MAX) + 1
        );
    }

    #[test]
    fn measurement_surface_with_output_ports_is_unavailable() {
        let mut graph = BlockGraph::from_blog_text(
            "BLOG 1.0\n\n  0: Port [0,0,0]\n  1: ZXZ [0,0,1]\n  2: Port [0,0,2]\n  [0,0,0] -> +Z\n  [0,0,1] -> +Z\n",
        )
        .unwrap();
        graph
            .set_actions_lenient(vec![crate::Action::Measure {
                target: crate::MeasureTarget::Node(glam::ivec3(0, 0, 1)),
                name: "m".into(),
            }])
            .unwrap();
        let zx = ZXGraph::from_block_graph_for_analysis(&graph).unwrap();
        let metadata = MeasurementMetadata::collect(&zx);

        let err = construct_measurement_surface_row(&[], &zx, &metadata, "m", 2).unwrap_err();
        assert!(matches!(
            err,
            StabilizerError::MeasurementSurfaceUnavailable { ref mvar } if mvar == "m"
        ));
    }

    /// The accepted support, not the requested minimum, sets the deadline.
    #[test]
    fn accepted_surface_records_its_actual_deadline_when_early_limits_fail() {
        let graph = GalleryItem::CCZFactoryWithTels
            .build()
            .materialize_root_graph()
            .expect("gallery flat projection");
        let zx = ZXGraph::try_from(&graph).unwrap();
        let metadata = MeasurementMetadata::collect(&zx);
        let selective_constraints = collect_selective_constraints(&zx);

        // A plain node column at z == 3 that no other measurement or selective
        // constraint touches, so the late candidate is legal at limit_z >= 3.
        let measurement_cols = metadata.cols.values().copied().collect::<Vec<_>>();
        let hi_col = zx
            .nodes
            .iter()
            .find(|node| {
                node.pos.z == 3
                    && !node.is_output_port(&zx)
                    && !measurement_cols.contains(&node.id)
                    && selective_constraints
                        .iter()
                        .all(|constraint| constraint.col != node.id)
            })
            .expect("gallery graph has a plain node at z = 3")
            .id;

        // Measurement columns are directed edge columns; acceptance requires
        // equal Paulis on both halves of the pair.
        let partner = |col: usize| {
            zx.edges
                .iter()
                .find_map(|edge| match zx.edge_column_pair(edge) {
                    (a, b) if a == col => Some(b),
                    (a, b) if b == col => Some(a),
                    _ => None,
                })
                .expect("measurement column is a directed edge column")
        };
        let set_pair = |row: &mut PauliString, col: usize| {
            row.set(col, Pauli::Z);
            row.set(partner(col), Pauli::Z);
        };

        // At limit 2 the target's only support is entangled with mz0145; only
        // the z == 3 row combines it into a clean single-mvar surface.
        let mut entangled_row = PauliString::new(zx.total_ids);
        set_pair(&mut entangled_row, metadata.column("mz0126"));
        set_pair(&mut entangled_row, metadata.column("mz0145"));
        let mut late_row = PauliString::new(zx.total_ids);
        set_pair(&mut late_row, metadata.column("mz0145"));
        late_row.set(hi_col, Pauli::Z);

        let mut reference = None;
        for gap in [0, 1_000_000_000] {
            let mut shifted = zx.clone();
            let old_position = shifted.nodes[hi_col].pos;
            shifted.pos_to_node.remove(&old_position);
            shifted.nodes[hi_col].pos.z += gap;
            shifted
                .pos_to_node
                .insert(shifted.nodes[hi_col].pos, hi_col);
            let mut basis = TrackedBasis::new(vec![entangled_row.clone(), late_row.clone()]);
            let surface = construct_measurement_surface_row_from_basis(
                &mut basis,
                0,
                &shifted,
                &metadata,
                &selective_constraints,
                &SelfReaderContext {
                    fixings: &[],
                    prior_surfaces: &[],
                },
                &MeasurementSpec {
                    name: "mz0126".into(),
                    minimum_support_z: 2,
                    deadline: 2,
                },
                &mut SearchBudget::new(64),
            )
            .unwrap();
            assert_eq!(surface.deadline, i64::from(gap) + 4);
            if let Some(reference) = &reference {
                assert_eq!(&surface.row, reference);
            } else {
                reference = Some(surface.row);
            }
        }
    }

    /// Selective support is available at its resolve layer.
    #[test]
    fn selective_site_support_does_not_delay_the_recorded_deadline() {
        let graph = GalleryItem::THTH
            .build()
            .materialize_root_graph()
            .expect("gallery flat projection");
        let zx = ZXGraph::try_from(&graph).unwrap();
        let basis_rows = reduce_to_basis(&zx.to_external_generator_table(), zx.total_ids);
        let constraints = collect_selective_constraints(&zx);
        let mut basis = TrackedBasis::new(basis_rows);

        let surfaces = extract_measurement_prefix_with_constraints(
            &zx,
            &mut basis,
            &MeasurementMetadata::collect(&zx),
            &constraints,
            &[],
            &measurement_specs(&zx),
            &mut SearchBudget::new(usize::MAX),
        )
        .unwrap();

        let mzz2 = surfaces
            .iter()
            .find(|surface| surface.name == "mzz2")
            .expect("THTH names mzz2");
        assert_eq!(mzz2.deadline, 2);
    }

    #[test]
    fn measurement_surface_past_accepted_deadline_is_rejected() {
        use glam::ivec3;
        use std::collections::HashSet;

        let interior_nodes = [(ivec3(0, 0, 2), Pauli::Z), (ivec3(0, 0, 4), Pauli::Z)]
            .into_iter()
            .collect();
        let stabilizer = Stabilizer {
            paulis: PauliString::new(1),
            sign: false,
            port_stabilizer: Default::default(),
            interior_nodes,
            interior_edges: Default::default(),
        };
        let no_selective = HashSet::new();

        // Interior support reaches z = 4, so the parity is readable only from
        // layer 5 on: a surface that promised layer 4 is rejected.
        let err = check_measurement_extent("m", &stabilizer, 4, &no_selective).unwrap_err();
        assert!(matches!(
            err,
            StabilizerError::MeasurementSurfaceExtendsPastDeadline {
                ref name,
                max_z: 4,
                deadline: 4,
            } if name == "m"
        ));

        // The same surface honouring a deadline of 5: accepted.
        check_measurement_extent("m", &stabilizer, 5, &no_selective).unwrap();

        // The z = 4 support sits on a selective-fixing site: exempt, so the
        // remaining z = 2 support closes by layer 4 (THTH's mzz2 routing
        // through a same-/late-layer selective site).
        let selective = HashSet::from([ivec3(0, 0, 4)]);
        check_measurement_extent("m", &stabilizer, 4, &selective).unwrap();
    }

    #[test]
    fn entangled_measurement_row_is_retained_when_it_cannot_be_isolated() {
        let graph = GalleryItem::CCZFactoryWithTels
            .build()
            .materialize_root_graph()
            .expect("gallery flat projection");
        let zx = ZXGraph::try_from(&graph).unwrap();
        let metadata = MeasurementMetadata::collect(&zx);

        let mut entangled_row = PauliString::new(zx.total_ids);
        for name in ["mz0126", "mz0145"] {
            let col = metadata.column(name);
            let partner = zx
                .edges
                .iter()
                .find_map(|edge| match zx.edge_column_pair(edge) {
                    (a, b) if a == col => Some(b),
                    (a, b) if b == col => Some(a),
                    _ => None,
                })
                .unwrap();
            entangled_row.set(col, Pauli::Z);
            entangled_row.set(partner, Pauli::Z);
        }

        let surface = construct_measurement_surface_row(
            &[entangled_row.clone()],
            &zx,
            &metadata,
            "mz0126",
            2,
        )
        .expect("entanglement alone does not invalidate a measurement row");

        assert_eq!(surface.row, entangled_row);
        assert!(!metadata.support_is_isolated("mz0126", &surface.row));
    }

    #[test]
    fn failed_measurement_diagonalization_leaves_rows_unchanged() {
        let graph = GalleryItem::CCZFactoryWithTels
            .build()
            .materialize_root_graph()
            .expect("gallery flat projection");
        let zx = ZXGraph::try_from(&graph).unwrap();
        let metadata = MeasurementMetadata::collect(&zx);
        let names = ["mz0126".to_string(), "mz0145".to_string()];
        let first_col = metadata.column(&names[0]);
        let second_col = metadata.column(&names[1]);
        let spare_col = (0..zx.total_ids)
            .find(|col| *col != first_col && *col != second_col)
            .unwrap();

        let mut first = PauliString::new(zx.total_ids);
        first.set(first_col, Pauli::Z);
        first.set(second_col, Pauli::Z);
        let mut second = first.clone();
        second.set(spare_col, Pauli::X);
        let mut basis = TrackedBasis::new(vec![first, second]);
        let original = basis.clone();

        diagonalize_measurement_columns(&mut basis, &names, &metadata, &[], &[]);

        assert_eq!(basis, original);
    }

    /// The kernel repair must not undo a column it already fixed.
    ///
    /// Only `XX` moves `c1` off `Z`, and it flips `c2` onto `Z` on the way;
    /// `c2` then has to be cleaned up by a row that leaves `c1` alone. Drawing
    /// the second pivot from the whole kernel picks `XX` again and puts `c1`
    /// back on its forbidden Pauli — the sum `XI` is the only repair.
    #[test]
    fn selective_repair_keeps_earlier_columns_clear() {
        let (c1, c2) = (0, 1);
        let constraints = [
            SelectiveConstraint::new(IVec3::ZERO, SelectiveKind::XY, c1),
            SelectiveConstraint::new(IVec3::ONE, SelectiveKind::XY, c2),
        ];
        let kernel = [
            PauliString::from_terms(2, [(c1, Pauli::X), (c2, Pauli::X)]),
            PauliString::from_terms(2, [(c1, Pauli::X)]),
        ];
        let coeffs = [CoeffVec::singleton(0, 2), CoeffVec::singleton(1, 2)];

        let mut row = PauliString::from_terms(2, [(c1, Pauli::Z), (c2, Pauli::Y)]);
        let mut combination = CoeffVec::zeros(2);
        assert!(repair_selective_support(
            &mut row,
            &mut combination,
            &kernel,
            &coeffs,
            &constraints,
        ));

        assert_eq!(row.get(c1), Pauli::Y);
        assert_eq!(row.get(c2), Pauli::Y);
        assert_eq!(combination.to_indices(), vec![1]);
    }

    /// A satisfied column keeps its kernel freedom.
    ///
    /// `XX` is the only way to move `c2` off `Z`, and it lands `c1` on the
    /// tolerated `X`. Locking every column in order would have spent `XX` on
    /// `c1`, which needed nothing, and left `c2` stranded.
    #[test]
    fn selective_repair_spends_the_kernel_only_where_needed() {
        let (c1, c2) = (0, 1);
        let constraints = [
            SelectiveConstraint::new(IVec3::ZERO, SelectiveKind::XY, c1),
            SelectiveConstraint::new(IVec3::ONE, SelectiveKind::XY, c2),
        ];
        let kernel = [PauliString::from_terms(2, [(c1, Pauli::X), (c2, Pauli::X)])];
        let coeffs = [CoeffVec::singleton(0, 1)];

        let mut row = PauliString::from_terms(2, [(c2, Pauli::Z)]);
        let mut combination = CoeffVec::zeros(1);
        assert!(repair_selective_support(
            &mut row,
            &mut combination,
            &kernel,
            &coeffs,
            &constraints,
        ));

        assert_eq!(row.get(c1), Pauli::X);
        assert_eq!(row.get(c2), Pauli::Y);
    }

    /// A column with no kernel freedom left cannot be repaired.
    #[test]
    fn selective_repair_fails_without_kernel_freedom() {
        let constraints = [SelectiveConstraint::new(IVec3::ZERO, SelectiveKind::XY, 0)];
        let mut row = PauliString::from_terms(1, [(0, Pauli::Z)]);
        let mut combination = CoeffVec::zeros(0);
        assert!(!repair_selective_support(
            &mut row,
            &mut combination,
            &[],
            &[],
            &constraints,
        ));
    }
}
