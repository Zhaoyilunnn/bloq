//! The single stabilizer-basis engine: parallel generator rows and their row
//! kinds, plus the selective-support invariant operations that act on them.
//!
//! During canonicalization a basis carries coefficient provenance in a
//! [`TrackedBasis`](super::basis::TrackedBasis). Rank repair mutates rows
//! independently of those coefficients, so the repair boundary consumes the
//! `TrackedBasis` and yields a [`StabilizerBasis`], which has no coefficients:
//! reading stale provenance afterwards is unrepresentable, not just discouraged.
//!
//! Readable-row reconstruction and the two fill operations live here so their
//! selective-support rules share one basis type.

use std::collections::HashSet;

use bloq_utils::{Pauli, PauliString};

use super::affine::{
    ExactPauliConstraint, materialize_tracked_row, solve_affine_tracked, visit_affine_coefficients,
};
use super::basis::{CoeffVec, PauliSpan, TrackedBasis, gaussian_elimination_with_tracking};
use super::selective::{
    SelectiveConstraint, boundary_pivot_constraints, normalize_selective_support_in_place,
};
use super::{SearchBudget, StabilizerError, StabilizerRowKind};
use crate::{ActionDag, ModuleCertificationLimits, SelectiveKind, ZXGraph};
use glam::IVec3;

#[derive(Debug, Clone)]
struct JointSelectiveGroup {
    constraints: Vec<SelectiveConstraint>,
    permitted_support: Vec<Vec<Pauli>>,
}

impl JointSelectiveGroup {
    fn permits(&self, row: &PauliString) -> bool {
        self.constraints
            .iter()
            .all(|constraint| row.get(constraint.col) == Pauli::I)
            || self.permitted_support.iter().any(|support| {
                self.constraints
                    .iter()
                    .zip(support)
                    .all(|(constraint, &pauli)| row.get(constraint.col) == pauli)
            })
    }
}

struct ReadoutConstraints {
    width: usize,
    source: TrackedBasis,
    single_fixings: Vec<(SelectiveConstraint, PauliString)>,
    joint_groups: Vec<JointSelectiveGroup>,
}

/// Boundary-reduce logical and fixing rows without changing any selective-site
/// support. Logical rows that are identity on every selective column are safe
/// pivots: XORing one into any other row leaves its selective support unchanged.
fn canonicalize_boundaries_with_neutral_logicals(
    rows: &mut [PauliString],
    kinds: &[StabilizerRowKind],
    constraints: &[SelectiveConstraint],
    zx: &ZXGraph,
) {
    let mut neutral_indices = Vec::new();
    let mut active_indices = Vec::new();
    for (index, (row, kind)) in rows.iter().zip(kinds).enumerate() {
        match kind {
            StabilizerRowKind::Logical
                if constraints
                    .iter()
                    .all(|constraint| row.get(constraint.col) == Pauli::I) =>
            {
                neutral_indices.push(index);
            }
            StabilizerRowKind::Logical | StabilizerRowKind::SelectiveFixing { .. } => {
                active_indices.push(index);
            }
            StabilizerRowKind::Measurement { .. } => {}
        }
    }
    if neutral_indices.is_empty() {
        return;
    }

    let neutral_len = neutral_indices.len();
    let indices = neutral_indices
        .into_iter()
        .chain(active_indices)
        .collect::<Vec<_>>();
    let mut logical_rows = indices
        .iter()
        .map(|&index| rows[index].clone())
        .collect::<Vec<_>>();
    gaussian_elimination_with_tracking(
        &mut logical_rows,
        &mut [],
        boundary_pivot_constraints(zx),
        |row, &(col, basis)| row.get(col) & basis,
        Some(neutral_len),
    );
    for (index, row) in indices.into_iter().zip(logical_rows) {
        rows[index] = row;
    }
}

impl ReadoutConstraints {
    fn new(
        rows: &[PauliString],
        kinds: &[StabilizerRowKind],
        constraints: &[SelectiveConstraint],
        action_graph: &ActionDag,
        budget: &SearchBudget,
    ) -> Result<Self, StabilizerError> {
        let joint_groups = joint_selective_groups(kinds, constraints, action_graph, budget.limits)?;
        budget.check_matrix(
            rows.len().saturating_mul(2),
            rows.first().map_or(0, PauliString::len),
            rows.len(),
            rows.len(),
        )?;
        Ok(Self {
            width: rows.first().map(PauliString::len).unwrap_or(0),
            source: TrackedBasis::new(
                rows.iter()
                    .zip(kinds)
                    .filter(|(_, kind)| !matches!(kind, StabilizerRowKind::Measurement { .. }))
                    .map(|(row, _)| row.clone())
                    .collect(),
            ),
            single_fixings: single_selective_fixings(rows, kinds, constraints),
            joint_groups,
        })
    }

    fn normalize(&self, mut row: PauliString) -> PauliString {
        for (constraint, fixing) in &self.single_fixings {
            if row.get(constraint.col) == constraint.forbidden {
                row ^= fixing;
            }
        }
        row
    }

    fn permits(&self, row: &PauliString) -> bool {
        self.joint_groups.iter().all(|group| group.permits(row))
    }

    fn permits_measurement(&self, row: &PauliString) -> bool {
        self.joint_groups.iter().all(|group| {
            group
                .constraints
                .iter()
                .all(|constraint| row.get(constraint.col) == Pauli::I)
                || group.permitted_support.iter().any(|support| {
                    group
                        .constraints
                        .iter()
                        .zip(support)
                        .all(|(constraint, &basis)| {
                            let pauli = row.get(constraint.col);
                            pauli == Pauli::I || pauli == basis
                        })
                })
        })
    }

    fn materialize(&self, coeff: &CoeffVec) -> PauliString {
        materialize_tracked_row(coeff, &self.source.rows, self.width)
    }

    fn representative(
        &self,
        base: &PauliString,
        clear: &[usize],
        allow_normalized_fallback: bool,
        budget: &mut SearchBudget,
    ) -> Result<Option<PauliString>, StabilizerError> {
        let normalized = self.normalize(base.clone());
        if allow_normalized_fallback
            && !self.permits(&normalized)
            && self.permits_measurement(&normalized)
        {
            return Ok(Some(normalized));
        }
        if self.permits(&normalized) && clear.iter().all(|&col| normalized.get(col) == Pauli::I) {
            return Ok(Some(normalized));
        }

        if let Some(candidate) = self.representative_clearing(base, clear, budget)? {
            return Ok(Some(candidate));
        }
        if clear.is_empty() {
            Ok(None)
        } else {
            let fallback = self.representative_clearing(base, &[], budget)?;
            Ok(fallback.or_else(|| {
                (allow_normalized_fallback && self.permits(&normalized)).then_some(normalized)
            }))
        }
    }

    fn representative_clearing(
        &self,
        base: &PauliString,
        clear: &[usize],
        budget: &mut SearchBudget,
    ) -> Result<Option<PauliString>, StabilizerError> {
        let mut constraints = clear
            .iter()
            .map(|&col| ExactPauliConstraint {
                col,
                pauli: base.get(col),
            })
            .collect();
        self.representative_with_groups(base, clear, 0, &mut constraints, budget)
    }

    fn representative_with_groups(
        &self,
        base: &PauliString,
        clear: &[usize],
        index: usize,
        constraints: &mut Vec<ExactPauliConstraint>,
        budget: &mut SearchBudget,
    ) -> Result<Option<PauliString>, StabilizerError> {
        budget.visit("normalization search states")?;
        budget.check_affine(
            &self.source.rows,
            &self.source.coeffs,
            constraints.len().saturating_mul(2),
        )?;
        let Some(solution) =
            solve_affine_tracked(&self.source.rows, &self.source.coeffs, constraints)
        else {
            return Ok(None);
        };
        if let Some(group) = self.joint_groups.get(index) {
            // Only feasibility is needed before adding the next group's
            // constraints. Do not retain a dense kernel at every recursion level.
            drop(solution);
            let prior = constraints.len();
            for support in std::iter::once(None).chain(group.permitted_support.iter().map(Some)) {
                constraints.extend(group.constraints.iter().enumerate().map(
                    |(slot, constraint)| ExactPauliConstraint {
                        col: constraint.col,
                        pauli: base.get(constraint.col)
                            ^ support.map_or(Pauli::I, |support| support[slot]),
                    },
                ));
                if let Some(candidate) =
                    self.representative_with_groups(base, clear, index + 1, constraints, budget)?
                {
                    return Ok(Some(candidate));
                }
                constraints.truncate(prior);
            }
            return Ok(None);
        }
        // Only single-site fixings can change normalization after this solve.
        let mut projected_span = PauliSpan::new(self.width);
        let kernel = solution
            .kernel_coeffs
            .iter()
            .filter_map(|coeff| {
                let row = self.materialize(coeff);
                let mut projected = PauliString::new(self.width);
                for (constraint, _) in &self.single_fixings {
                    projected.set(constraint.col, row.get(constraint.col));
                }
                projected_span
                    .insert_if_independent(projected)
                    .then(|| coeff.clone())
            })
            .collect::<Vec<_>>();
        let mut result = None;
        visit_affine_coefficients(
            &solution.particular_coeff,
            &kernel,
            budget,
            "normalization search states",
            |combination, _| {
                let mut candidate = base.clone();
                candidate ^= &self.materialize(combination);
                let candidate = self.normalize(candidate);
                result = (self.permits(&candidate)
                    && clear.iter().all(|&col| candidate.get(col) == Pauli::I))
                .then_some(candidate);
                result.is_some()
            },
        )?;
        Ok(result)
    }

    fn insert_logical(
        &self,
        row: PauliString,
        span: &mut PauliSpan,
        rows: &mut Vec<PauliString>,
        kinds: &mut Vec<StabilizerRowKind>,
    ) {
        let row = self.normalize(row);
        if self.permits(&row) && span.insert_if_independent(row.clone()) {
            rows.push(row);
            kinds.push(StabilizerRowKind::Logical);
        }
    }

    fn error(&self, row: &PauliString, fallback: SelectiveConstraint) -> StabilizerError {
        let constraint = self
            .joint_groups
            .iter()
            .find(|group| !group.permits(row))
            .map(|group| group.constraints[0])
            .unwrap_or(fallback);
        StabilizerError::SelectiveSupportUnsatisfiable {
            pos: constraint.pos,
            kind: constraint.kind,
        }
    }
}

/// A stabilizer basis: Pauli-string generator rows paired with the role each
/// row plays. The shared core owned by the canonical table and the runtime
/// basis, so the selective-support invariant lives in exactly one type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StabilizerBasis {
    pub(crate) rows: Vec<PauliString>,
    pub(crate) kinds: Vec<StabilizerRowKind>,
    /// Public rows form a prefix; any suffix is internal rank completion.
    pub(crate) public_len: usize,
}

impl StabilizerBasis {
    pub(crate) fn new(rows: Vec<PauliString>, kinds: Vec<StabilizerRowKind>) -> Self {
        let public_len = rows.len();
        Self::with_public_len(rows, kinds, public_len)
    }

    pub(crate) fn with_public_len(
        rows: Vec<PauliString>,
        kinds: Vec<StabilizerRowKind>,
        public_len: usize,
    ) -> Self {
        assert_eq!(rows.len(), kinds.len(), "every stabilizer row has one role");
        assert!(public_len <= rows.len(), "public rows form a valid prefix");
        Self {
            rows,
            kinds,
            public_len,
        }
    }

    /// A basis of `rows` all playing the [`Logical`](StabilizerRowKind::Logical)
    /// role.
    pub(crate) fn all_logical(rows: Vec<PauliString>) -> Self {
        let kinds = vec![StabilizerRowKind::Logical; rows.len()];
        Self::new(rows, kinds)
    }

    /// Rebuild readable rows with permitted single-site and joint support.
    /// Missing full-rank directions remain as internal auxiliary rows.
    pub(crate) fn constrain_readout_selective_support(
        &mut self,
        constraints: &[SelectiveConstraint],
        zx: &ZXGraph,
        allow_normalized_fallback: bool,
        budget: &mut SearchBudget,
    ) -> Result<(), StabilizerError> {
        if constraints.is_empty() {
            return Ok(());
        }

        let target_rank = self.rows.len();
        let readout = ReadoutConstraints::new(
            &self.rows,
            &self.kinds,
            constraints,
            zx.action_graph(),
            budget,
        )?;

        let mut rows = Vec::with_capacity(target_rank);
        let mut kinds = Vec::with_capacity(target_rank);
        let mut span = PauliSpan::new(readout.width);
        let output_cols = zx
            .nodes()
            .iter()
            .filter(|node| node.is_output_port(zx))
            .map(|node| node.id)
            .collect::<Vec<_>>();

        for (row, kind) in self.rows.iter().zip(&self.kinds) {
            let StabilizerRowKind::Measurement { .. } = kind else {
                continue;
            };
            let candidate = readout
                .representative(row, &output_cols, allow_normalized_fallback, budget)?
                .ok_or_else(|| readout.error(row, constraints[0]))?;
            if !span.insert_if_independent(candidate.clone()) {
                return Err(StabilizerError::BasisRankDeficient {
                    expected: target_rank,
                    actual: span.rank(),
                });
            }
            rows.push(candidate);
            kinds.push(kind.clone());
        }

        for (row, kind) in self.rows.iter().zip(&self.kinds) {
            if !kind.is_selective_fixing() {
                continue;
            }
            if !span.insert_if_independent(row.clone()) {
                return Err(StabilizerError::BasisRankDeficient {
                    expected: target_rank,
                    actual: span.rank(),
                });
            }
            rows.push(row.clone());
            kinds.push(kind.clone());
        }

        for (row, kind) in self.rows.iter().zip(&self.kinds) {
            if matches!(kind, StabilizerRowKind::Logical) {
                readout.insert_logical(row.clone(), &mut span, &mut rows, &mut kinds);
            }
        }

        let mut added_kernel = false;
        budget.visit("normalization seed states")?;
        let mut pattern = readout
            .joint_groups
            .iter()
            .flat_map(|group| {
                group
                    .constraints
                    .iter()
                    .map(|constraint| ExactPauliConstraint {
                        col: constraint.col,
                        pauli: Pauli::I,
                    })
            })
            .collect::<Vec<_>>();
        let mut offset = 0;
        let supports = readout.joint_groups.iter().flat_map(|group| {
            let start = offset;
            offset += group.constraints.len();
            group
                .permitted_support
                .iter()
                .map(move |support| (start, support))
        });
        // Identity, then one permitted group at a time spans all joint seeds.
        // Reuse one constraint vector instead of retaining every dense seed.
        for support in std::iter::once(None).chain(supports.map(Some)) {
            if support.is_some() {
                budget.visit("normalization seed states")?;
            }
            for constraint in &mut pattern {
                constraint.pauli = Pauli::I;
            }
            if let Some((start, support)) = support {
                for (slot, &pauli) in support.iter().enumerate() {
                    pattern[start + slot].pauli = pauli;
                }
            }
            budget.check_affine(
                &readout.source.rows,
                &readout.source.coeffs,
                pattern.len().saturating_mul(2),
            )?;
            let Some(solution) =
                solve_affine_tracked(&readout.source.rows, &readout.source.coeffs, &pattern)
            else {
                continue;
            };

            if !added_kernel {
                for coeff in &solution.kernel_coeffs {
                    readout.insert_logical(
                        readout.materialize(coeff),
                        &mut span,
                        &mut rows,
                        &mut kinds,
                    );
                }
                added_kernel = true;
            }

            readout.insert_logical(
                readout.materialize(&solution.particular_coeff),
                &mut span,
                &mut rows,
                &mut kinds,
            );
        }

        let public_len = rows.len();
        for row in &readout.source.rows {
            if rows.len() == target_rank {
                break;
            }
            if span.insert_if_independent(row.clone()) {
                rows.push(row.clone());
                kinds.push(StabilizerRowKind::Logical);
            }
        }

        canonicalize_boundaries_with_neutral_logicals(
            &mut rows[..public_len],
            &kinds[..public_len],
            constraints,
            zx,
        );

        if rows.len() != target_rank || span.rank() != target_rank {
            return Err(StabilizerError::BasisRankDeficient {
                expected: target_rank,
                actual: span.rank(),
            });
        }
        self.rows = rows;
        self.kinds = kinds;
        self.public_len = public_len;
        Ok(())
    }

    /// Resolve a selective node whose fill is witnessed by a tagged fixing row,
    /// consuming that row: for each fill constraint the fixing row is the pivot,
    /// XORed into every other row that still carries forbidden support, then
    /// dropped. Returns the reduced basis and, per surviving row, the source-row
    /// combination that produced it.
    ///
    /// # Errors
    ///
    /// Returns [`StabilizerError::SelectiveSupportUnsatisfiable`] if no tagged
    /// fixing row is available to pivot, or the one found cannot cancel the
    /// forbidden support.
    pub(crate) fn apply_tagged_selective_fill(
        &self,
        constraints: &[(usize, Pauli)],
        selective_pos: IVec3,
        selective_kind: SelectiveKind,
    ) -> Result<(StabilizerBasis, Vec<CoeffVec>), StabilizerError> {
        let mut rows = self.rows.clone();
        let mut kinds = self.kinds.clone();
        let mut public_len = self.public_len;
        let width = rows.len();
        let mut coeffs = (0..rows.len())
            .map(|index| CoeffVec::singleton(index, width))
            .collect::<Vec<_>>();

        let unsatisfiable = || StabilizerError::SelectiveSupportUnsatisfiable {
            pos: selective_pos,
            kind: selective_kind,
        };

        for &(col, target_pauli) in constraints {
            let pivot_index = kinds
                .iter()
                .position(|kind| kind.fixes_selective(selective_pos))
                .ok_or_else(unsatisfiable)?;
            let pivot_pauli = rows[pivot_index].get(col);
            if pivot_pauli == Pauli::I || pivot_pauli == target_pauli {
                return Err(unsatisfiable());
            }

            let pivot_row = rows[pivot_index].clone();
            let pivot_coeff = coeffs[pivot_index].clone();
            for index in 0..rows.len() {
                if index == pivot_index {
                    continue;
                }
                let pauli = rows[index].get(col);
                if pauli != Pauli::I && pauli != target_pauli {
                    rows[index] ^= &pivot_row;
                    coeffs[index].xor_assign(&pivot_coeff);
                }
            }

            let _ = rows.remove(pivot_index);
            kinds.remove(pivot_index);
            coeffs.remove(pivot_index);
            if pivot_index < public_len {
                public_len -= 1;
            }
        }

        Ok((
            StabilizerBasis::with_public_len(rows, kinds, public_len),
            coeffs,
        ))
    }

    /// Resolve a selective node generically: Gaussian-eliminate the rows against
    /// the fill constraints, drop the pivoted rows, then re-normalize any
    /// remaining selective support. Every surviving row is `Logical`; existing
    /// row kinds play no part here (unlike the tagged path). Returns the reduced
    /// basis and the per-row source combination.
    ///
    /// # Errors
    ///
    /// Returns [`StabilizerError::SelectiveSupportUnsatisfiable`] if the
    /// remaining selective support cannot be normalized.
    pub(crate) fn apply_generic_selective_fill(
        &self,
        constraints: &[(usize, Pauli)],
        remaining_constraints: &[SelectiveConstraint],
    ) -> Result<(StabilizerBasis, Vec<CoeffVec>), StabilizerError> {
        let mut rows = self.rows.clone();
        let width = rows.len();
        let mut coeffs = (0..rows.len())
            .map(|index| CoeffVec::singleton(index, width))
            .collect::<Vec<_>>();
        let num_pivots = gaussian_elimination_with_tracking(
            &mut rows,
            &mut coeffs,
            constraints.iter().copied(),
            |row, &(col, target_pauli)| {
                let pauli = row.get(col);
                pauli != Pauli::I && pauli != target_pauli
            },
            None,
        );

        rows.drain(..num_pivots);
        coeffs.drain(..num_pivots);
        normalize_selective_support_in_place(&mut rows, Some(&mut coeffs), remaining_constraints)?;
        Ok((StabilizerBasis::all_logical(rows), coeffs))
    }
}

fn joint_selective_groups(
    kinds: &[StabilizerRowKind],
    constraints: &[SelectiveConstraint],
    action_graph: &ActionDag,
    limits: ModuleCertificationLimits,
) -> Result<Vec<JointSelectiveGroup>, StabilizerError> {
    let mut groups = Vec::new();
    for kind in kinds {
        let targets = kind.selective_fixing_targets();
        if targets.len() <= 1 {
            continue;
        }
        let constraints = targets
            .iter()
            .map(|target| {
                *constraints
                    .iter()
                    .find(|constraint| constraint.pos == target.pos)
                    .expect("fixing target names a selective constraint")
            })
            .collect::<Vec<_>>();
        let permitted_support = reachable_joint_support(&constraints, action_graph, limits)?;
        groups.push(JointSelectiveGroup {
            constraints,
            permitted_support,
        });
    }
    Ok(groups)
}

fn reachable_joint_support(
    constraints: &[SelectiveConstraint],
    action_graph: &ActionDag,
    limits: ModuleCertificationLimits,
) -> Result<Vec<Vec<Pauli>>, StabilizerError> {
    let targets = constraints
        .iter()
        .map(|constraint| constraint.pos)
        .collect::<Vec<_>>();
    let limit = limits.max_guarded_domain_size;
    let values = action_graph
        .resolve_value_domain_bounded_with_limits(&targets, limit, limits.boolean_limits())
        .map_err(|error| error.into_stabilizer("selective value domain", limit))?;
    let mut reachable = values
        .values()
        .iter()
        .cloned()
        .map(|values| {
            constraints
                .iter()
                .zip(values)
                .map(|(constraint, value)| {
                    Pauli::from(if value {
                        constraint.kind.pauli_if_true()
                    } else {
                        constraint.kind.pauli_if_false()
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    // A readable support is a complete joint orbit: applying the shared fixing
    // row must land on another branch tuple reachable from the source controls.
    let all_reachable = reachable.iter().cloned().collect::<HashSet<_>>();
    reachable.retain(|support| {
        let other = support
            .iter()
            .zip(constraints)
            .map(|(&pauli, constraint)| pauli ^ constraint.forbidden)
            .collect::<Vec<_>>();
        all_reachable.contains(&other)
    });
    Ok(reachable)
}

fn single_selective_fixings(
    rows: &[PauliString],
    kinds: &[StabilizerRowKind],
    constraints: &[SelectiveConstraint],
) -> Vec<(SelectiveConstraint, PauliString)> {
    rows.iter()
        .zip(kinds)
        .filter_map(|(row, kind)| {
            let [target] = kind.selective_fixing_targets() else {
                return None;
            };
            constraints
                .iter()
                .find(|constraint| constraint.pos == target.pos)
                .map(|constraint| (*constraint, row.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use glam::ivec3;

    use super::*;
    use crate::{Action, BinaryOp, Expr, SelectiveFixingTarget};

    fn joint_support(actions: Vec<Action>) -> Vec<Vec<Pauli>> {
        let constraints = [
            SelectiveConstraint::new(ivec3(0, 0, 0), SelectiveKind::XZ, 0),
            SelectiveConstraint::new(ivec3(1, 0, 0), SelectiveKind::XZ, 1),
        ];
        let kind = StabilizerRowKind::SelectiveFixing {
            targets: constraints
                .iter()
                .map(|constraint| SelectiveFixingTarget {
                    pos: constraint.pos,
                    forbidden: constraint.forbidden,
                })
                .collect(),
        };
        joint_selective_groups(
            &[kind],
            &constraints,
            &ActionDag::from_actions(&actions),
            ModuleCertificationLimits::DEFAULT,
        )
        .expect("two-site joint support fits its branch-domain limit")
        .remove(0)
        .permitted_support
    }

    fn resolve(x: i32, condition: Expr) -> Action {
        Action::Resolve {
            target: ivec3(x, 0, 0),
            condition,
        }
    }

    #[test]
    fn joint_support_obeys_explicit_domain_limits_above_and_below_the_default() {
        let constraints = (0..13)
            .map(|index| {
                SelectiveConstraint::new(ivec3(index, 0, 0), SelectiveKind::XZ, index as usize)
            })
            .collect::<Vec<_>>();
        let actions = (0..13)
            .map(|index| resolve(index, Expr::Var(format!("m{index}"))))
            .collect::<Vec<_>>();
        let dag = ActionDag::from_actions(&actions);
        assert!(matches!(
            reachable_joint_support(
                &constraints,
                &dag,
                ModuleCertificationLimits {
                    max_guarded_domain_size: 1,
                    ..ModuleCertificationLimits::UNLIMITED
                }
            ),
            Err(StabilizerError::ResourceLimited {
                phase: "selective value domain",
                observed: 2,
                limit: 1
            })
        ));
        for max_guarded_domain_size in [8_192, usize::MAX] {
            assert_eq!(
                reachable_joint_support(
                    &constraints,
                    &dag,
                    ModuleCertificationLimits {
                        max_guarded_domain_size,
                        ..ModuleCertificationLimits::UNLIMITED
                    }
                )
                .unwrap()
                .len(),
                8_192
            );
        }
    }

    #[test]
    fn representative_prefers_output_free_solution() {
        let constraint = SelectiveConstraint::new(ivec3(0, 0, 0), SelectiveKind::XZ, 0);
        let readout = ReadoutConstraints {
            width: 3,
            source: TrackedBasis::new(vec![
                PauliString::try_from("XX_").unwrap(),
                PauliString::try_from("X__").unwrap(),
            ]),
            single_fixings: Vec::new(),
            joint_groups: vec![JointSelectiveGroup {
                constraints: vec![constraint],
                permitted_support: Vec::new(),
            }],
        };
        let mut budget = SearchBudget::new(usize::MAX);
        assert_eq!(
            readout
                .representative(
                    &PauliString::try_from("X_X").unwrap(),
                    &[1],
                    false,
                    &mut budget,
                )
                .unwrap()
                .unwrap(),
            PauliString::try_from("__X").unwrap(),
        );
    }

    #[test]
    fn named_representative_can_cross_multiple_joint_groups() {
        let groups = [0, 2]
            .map(|first| JointSelectiveGroup {
                constraints: [first, first + 1]
                    .map(|col| {
                        SelectiveConstraint::new(ivec3(col as i32, 0, 0), SelectiveKind::XZ, col)
                    })
                    .to_vec(),
                permitted_support: vec![vec![Pauli::X; 2], vec![Pauli::Z; 2]],
            })
            .to_vec();
        let readout = ReadoutConstraints {
            width: 6,
            source: TrackedBasis::new(vec![PauliString::try_from("_X_X_X").unwrap()]),
            single_fixings: Vec::new(),
            joint_groups: groups,
        };
        assert_eq!(
            readout
                .representative(
                    &PauliString::try_from("X_X_XX").unwrap(),
                    &[5],
                    false,
                    &mut SearchBudget::new(usize::MAX),
                )
                .unwrap(),
            Some(PauliString::try_from("XXXXX_").unwrap()),
        );
    }

    #[test]
    fn representative_finds_output_free_kernel_member_after_normalization() {
        let first = SelectiveConstraint::new(ivec3(0, 0, 0), SelectiveKind::XZ, 0);
        let second = SelectiveConstraint::new(ivec3(1, 0, 0), SelectiveKind::XZ, 1);
        let first_fixing = PauliString::try_from("Y_X_").unwrap();
        let second_fixing = PauliString::try_from("_YX_").unwrap();
        let readout = ReadoutConstraints {
            width: 4,
            source: TrackedBasis::new(vec![
                first_fixing.clone(),
                second_fixing.clone(),
                PauliString::try_from("_Y__").unwrap(),
            ]),
            single_fixings: vec![(first, first_fixing), (second, second_fixing)],
            joint_groups: Vec::new(),
        };

        let base = PauliString::try_from("Y__X").unwrap();
        let mut budget = SearchBudget::new(usize::MAX);
        assert_eq!(
            readout
                .representative(&base, &[2], false, &mut budget)
                .unwrap(),
            Some(PauliString::try_from("___X").unwrap()),
        );

        let mut budget = SearchBudget::new(1);
        assert!(matches!(
            readout.representative(&base, &[2], false, &mut budget),
            Err(StabilizerError::ResourceLimited {
                phase: "normalization search states",
                observed: 2,
                limit: 1,
            })
        ));
    }

    #[test]
    fn shared_let_control_permits_only_equal_axes() {
        let control = Expr::Not(Box::new(Expr::Var("m".into())));
        let support = joint_support(vec![
            Action::Let {
                name: "control".into(),
                expr: control,
            },
            resolve(0, Expr::Var("control".into())),
            resolve(1, Expr::Var("control".into())),
        ]);

        assert_eq!(
            support,
            vec![vec![Pauli::Z, Pauli::Z], vec![Pauli::X, Pauli::X]]
        );
    }

    #[test]
    fn incomplete_joint_fixing_orbit_is_not_permitted() {
        let support = joint_support(vec![
            resolve(0, Expr::Var("m".into())),
            resolve(
                1,
                Expr::Binary(
                    BinaryOp::And,
                    Box::new(Expr::Var("m".into())),
                    Box::new(Expr::Var("n".into())),
                ),
            ),
        ]);

        assert_eq!(
            support,
            vec![vec![Pauli::Z, Pauli::Z], vec![Pauli::X, Pauli::X]]
        );
    }

    #[test]
    fn wide_joint_control_is_reduced_to_its_reachable_outputs() {
        let condition = (0..=usize::BITS)
            .map(|index| Expr::Var(format!("m{index}")))
            .reduce(|left, right| Expr::Binary(BinaryOp::Xor, Box::new(left), Box::new(right)))
            .expect("the variable range is nonempty");
        let support = joint_support(vec![resolve(0, condition.clone()), resolve(1, condition)]);

        assert_eq!(
            support,
            vec![vec![Pauli::Z, Pauli::Z], vec![Pauli::X, Pauli::X]]
        );
    }
}
