//! Binary affine-system solving for constrained stabilizer rows.

use bloq_utils::{Pauli, PauliString};

use super::basis::{CoeffVec, echelonize_with_witnesses};
use super::{SearchBudget, StabilizerError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExactPauliConstraint {
    pub(super) col: usize,
    pub(super) pauli: Pauli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PauliComponentConstraint {
    pub(super) col: usize,
    pub(super) basis: Pauli,
    pub(super) present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AffineTrackedSolution {
    pub(super) particular_coeff: CoeffVec,
    pub(super) kernel_coeffs: Vec<CoeffVec>,
}

/// Visits the affine kernel in the original depth-first order, with the last
/// kernel row changing fastest. Iteration avoids a stack frame per kernel row.
pub(super) fn visit_affine_coefficients(
    particular: &CoeffVec,
    kernel: &[CoeffVec],
    budget: &mut SearchBudget,
    phase: &'static str,
    mut visit: impl FnMut(&CoeffVec, &mut SearchBudget) -> bool,
) -> Result<bool, StabilizerError> {
    budget.visit(phase)?;
    let mut combination = particular.clone();
    let mut selected = vec![false; kernel.len()];
    loop {
        if visit(&combination, budget) {
            return Ok(true);
        }
        let Some(next) = selected.iter().rposition(|selected| !selected) else {
            return Ok(false);
        };
        budget.visit(phase)?;
        for (index, selected) in selected.iter_mut().enumerate().skip(next) {
            *selected = !*selected;
            combination.xor_assign(&kernel[index]);
        }
    }
}

fn solve_binary_affine_system(
    equations: &mut [CoeffVec],
    var_count: usize,
) -> Option<(CoeffVec, Vec<CoeffVec>)> {
    let rhs_col = var_count;
    let pivot_cols = echelonize_with_witnesses(equations, &mut [], var_count);

    for equation in equations.iter() {
        let has_variable_support = (0..var_count).any(|col| equation.bit(col));
        if !has_variable_support && equation.bit(rhs_col) {
            return None;
        }
    }

    let mut particular = CoeffVec::zeros(var_count);
    let mut is_pivot_col = vec![false; var_count];
    for (pivot_row, &pivot_col) in pivot_cols.iter().enumerate() {
        is_pivot_col[pivot_col] = true;
        if equations[pivot_row].bit(rhs_col) {
            particular.set_bit(pivot_col, true);
        }
    }

    let mut kernel = Vec::new();
    for (free_col, is_pivot_col) in is_pivot_col.iter().copied().enumerate() {
        if is_pivot_col {
            continue;
        }

        let mut basis = CoeffVec::zeros(var_count);
        basis.set_bit(free_col, true);
        for (pivot_row, &pivot_col) in pivot_cols.iter().enumerate() {
            if equations[pivot_row].bit(free_col) {
                basis.set_bit(pivot_col, true);
            }
        }
        kernel.push(basis);
    }

    Some((particular, kernel))
}

pub(super) fn solve_affine_tracked(
    basis_rows: &[PauliString],
    basis_coeffs: &[CoeffVec],
    constraints: &[ExactPauliConstraint],
) -> Option<AffineTrackedSolution> {
    let components = constraints
        .iter()
        .flat_map(|constraint| {
            [Pauli::X, Pauli::Z].map(|basis| PauliComponentConstraint {
                col: constraint.col,
                basis,
                present: constraint.pauli & basis,
            })
        })
        .collect::<Vec<_>>();
    solve_affine_tracked_components(basis_rows, basis_coeffs, &components)
}

pub(super) fn solve_affine_tracked_components(
    basis_rows: &[PauliString],
    basis_coeffs: &[CoeffVec],
    constraints: &[PauliComponentConstraint],
) -> Option<AffineTrackedSolution> {
    debug_assert_eq!(basis_rows.len(), basis_coeffs.len());
    let var_count = basis_rows.len();
    let row_width = basis_rows.first().map(PauliString::len).unwrap_or_else(|| {
        constraints
            .iter()
            .map(|constraint| constraint.col + 1)
            .max()
            .unwrap_or(0)
    });
    let coeff_width = basis_coeffs.iter().map(CoeffVec::len).max().unwrap_or(0);

    let mut normalized_coeffs = basis_coeffs.to_vec();
    for coeff in &mut normalized_coeffs {
        coeff.resize(coeff_width);
    }

    let mut equations = Vec::with_capacity(constraints.len());
    for constraint in constraints {
        debug_assert!(constraint.col < row_width);
        let mut equation = CoeffVec::zeros(var_count + 1);
        for (row_index, row) in basis_rows.iter().enumerate() {
            if row.get(constraint.col) & constraint.basis {
                equation.set_bit(row_index, true);
            }
        }
        equation.set_bit(var_count, constraint.present);
        equations.push(equation);
    }

    let (particular_basis, kernel_basis) = solve_binary_affine_system(&mut equations, var_count)?;
    let particular_coeff = materialize_coeff(&particular_basis, &normalized_coeffs, coeff_width);
    let kernel_coeffs = kernel_basis
        .iter()
        .map(|basis| materialize_coeff(basis, &normalized_coeffs, coeff_width))
        .collect::<Vec<_>>();

    Some(AffineTrackedSolution {
        particular_coeff,
        kernel_coeffs,
    })
}

pub(super) fn materialize_tracked_row(
    coeff: &CoeffVec,
    basis_rows: &[PauliString],
    row_width: usize,
) -> PauliString {
    let mut row = PauliString::new(row_width);
    for index in coeff.iter_ones() {
        row ^= &basis_rows[index];
    }
    row
}

fn materialize_coeff(coeff: &CoeffVec, basis_coeffs: &[CoeffVec], coeff_width: usize) -> CoeffVec {
    let mut tracked = CoeffVec::zeros(coeff_width);
    for index in coeff.iter_ones() {
        tracked.xor_assign(&basis_coeffs[index]);
    }
    tracked
}

#[cfg(test)]
mod tests {
    use bloq_utils::{Pauli, PauliString};

    use super::super::test_support::{algebraic_rows_from_coeffs, sparse_to_coeff};
    use super::{ExactPauliConstraint, materialize_tracked_row, solve_affine_tracked};

    #[test]
    fn affine_enumeration_preserves_order_and_charges_rejected_candidates() {
        use super::{CoeffVec, SearchBudget, StabilizerError, visit_affine_coefficients};

        let kernel = (0..3)
            .map(|index| CoeffVec::singleton(index, 3))
            .collect::<Vec<_>>();
        let mut seen = Vec::new();
        assert!(
            !visit_affine_coefficients(
                &CoeffVec::zeros(3),
                &kernel,
                &mut SearchBudget::new(8),
                "test affine states",
                |row, _| {
                    seen.push(row.to_indices());
                    false
                },
            )
            .unwrap()
        );
        assert_eq!(
            seen,
            [
                vec![],
                vec![2],
                vec![1],
                vec![1, 2],
                vec![0],
                vec![0, 2],
                vec![0, 1],
                vec![0, 1, 2]
            ]
        );

        let kernel = (0..80)
            .map(|index| CoeffVec::singleton(index, 80))
            .collect::<Vec<_>>();
        let mut visits = 0;
        assert!(matches!(
            visit_affine_coefficients(
                &CoeffVec::zeros(80),
                &kernel,
                &mut SearchBudget::new(1),
                "test affine states",
                |_, _| {
                    visits += 1;
                    false
                },
            ),
            Err(StabilizerError::ResourceLimited {
                observed: 2,
                limit: 1,
                ..
            })
        ));
        assert_eq!(visits, 1);
    }

    #[test]
    fn solve_affine_tracked_finds_particular_and_kernel() {
        let b0_rows = vec![
            PauliString::try_from("X_").unwrap(),
            PauliString::try_from("_X").unwrap(),
            PauliString::try_from("_Z").unwrap(),
        ];
        let basis_rows = vec![
            PauliString::try_from("XX").unwrap(),
            PauliString::try_from("XZ").unwrap(),
        ];
        let basis_coeffs = vec![sparse_to_coeff(&[0, 1], 3), sparse_to_coeff(&[0, 2], 3)];
        let constraints = vec![ExactPauliConstraint {
            col: 0,
            pauli: Pauli::X,
        }];

        let solution = solve_affine_tracked(&basis_rows, &basis_coeffs, &constraints).unwrap();

        assert_eq!(
            materialize_tracked_row(&solution.particular_coeff, &b0_rows, 2),
            PauliString::try_from("XX").unwrap()
        );
        assert_eq!(solution.particular_coeff, sparse_to_coeff(&[0, 1], 3));
        assert_eq!(
            algebraic_rows_from_coeffs(
                std::slice::from_ref(&solution.particular_coeff),
                &b0_rows,
                2,
            ),
            vec![materialize_tracked_row(
                &solution.particular_coeff,
                &b0_rows,
                2
            )]
        );

        assert_eq!(
            algebraic_rows_from_coeffs(&solution.kernel_coeffs, &b0_rows, 2),
            vec![PauliString::try_from("_Y").unwrap()]
        );
        assert_eq!(solution.kernel_coeffs, vec![sparse_to_coeff(&[1, 2], 3)]);

        assert_eq!(
            materialize_tracked_row(&solution.particular_coeff, &b0_rows, 2).get(0),
            Pauli::X
        );
        assert!(
            algebraic_rows_from_coeffs(&solution.kernel_coeffs, &b0_rows, 2)
                .iter()
                .all(|row| row.get(0) == Pauli::I)
        );
    }

    #[test]
    fn solve_affine_tracked_reports_unsatisfiable_exact_constraints() {
        let basis_rows = vec![PauliString::try_from("X_").unwrap()];
        let basis_coeffs = vec![sparse_to_coeff(&[0], 1)];
        let constraints = vec![
            ExactPauliConstraint {
                col: 0,
                pauli: Pauli::X,
            },
            ExactPauliConstraint {
                col: 0,
                pauli: Pauli::Z,
            },
        ];

        assert!(solve_affine_tracked(&basis_rows, &basis_coeffs, &constraints).is_none());
    }

    #[test]
    fn solve_affine_tracked_preserves_two_free_kernel_coordinates() {
        let b0_rows = vec![
            PauliString::try_from("XI").unwrap(),
            PauliString::try_from("IX").unwrap(),
            PauliString::try_from("_Z").unwrap(),
        ];
        let basis_rows = b0_rows.clone();
        let basis_coeffs = vec![
            sparse_to_coeff(&[0], 3),
            sparse_to_coeff(&[1], 3),
            sparse_to_coeff(&[2], 3),
        ];
        let constraints = vec![ExactPauliConstraint {
            col: 0,
            pauli: Pauli::X,
        }];

        let first = solve_affine_tracked(&basis_rows, &basis_coeffs, &constraints).unwrap();
        assert_eq!(
            materialize_tracked_row(&first.particular_coeff, &b0_rows, 2),
            PauliString::try_from("XI").unwrap()
        );
        assert_eq!(first.particular_coeff, sparse_to_coeff(&[0], 3));
        assert_eq!(
            algebraic_rows_from_coeffs(&first.kernel_coeffs, &b0_rows, 2),
            vec![
                PauliString::try_from("IX").unwrap(),
                PauliString::try_from("_Z").unwrap(),
            ]
        );
        assert_eq!(
            first.kernel_coeffs,
            vec![sparse_to_coeff(&[1], 3), sparse_to_coeff(&[2], 3)]
        );
        assert_eq!(
            algebraic_rows_from_coeffs(std::slice::from_ref(&first.particular_coeff), &b0_rows, 2,),
            vec![materialize_tracked_row(
                &first.particular_coeff,
                &b0_rows,
                2
            )]
        );
    }
}
