//! Translate a [`bloq_ir::circuit::GateType`] into the engine's instruction set.
//!
//! Single-qubit Cliffords map by name onto [`Gate1Q`], whose own
//! [`images`](Gate1Q::images) tableau is the engine's definition of the gate.
//! Tests pin the signed images against Stim's gate definitions.
//!
//! Two-qubit gates need no table at all. `GateType::two_qubit_bases` already
//! names the `<A>C<B>` axis pair the engine's own two-qubit instruction is
//! parameterized by, so the translation is the identity on it.

use crate::backend::{Gate1Q, Instruction, PauliBasis};
use bloq_ir::circuit::GateType;

/// The instruction `gate` contributes on one operand, or `None` for
/// [`GateType::I`], which has nothing to apply.
///
/// Two-qubit gates consume operand *pairs* and are translated by the caller;
/// passing one here panics.
pub(crate) fn single_qubit_instruction(gate: GateType, qubit: usize) -> Option<Instruction> {
    let instruction = match gate {
        GateType::I => return None,
        // Resets.
        GateType::RX => Instruction::Reset {
            basis: PauliBasis::X,
            qubit,
        },
        GateType::RY => Instruction::Reset {
            basis: PauliBasis::Y,
            qubit,
        },
        GateType::RZ => Instruction::Reset {
            basis: PauliBasis::Z,
            qubit,
        },
        // T family: axis from bloq's clifford_proxy (T→Z, T_YZ→X, T_XZ→Y).
        GateType::T => t(PauliBasis::Z, qubit, false),
        GateType::T_DAG => t(PauliBasis::Z, qubit, true),
        GateType::T_YZ => t(PauliBasis::X, qubit, false),
        GateType::T_YZ_DAG => t(PauliBasis::X, qubit, true),
        GateType::T_XZ => t(PauliBasis::Y, qubit, false),
        GateType::T_XZ_DAG => t(PauliBasis::Y, qubit, true),
        other => Instruction::Gate1 {
            gate: single_qubit_clifford(other),
            qubit,
        },
    };
    Some(instruction)
}

fn t(basis: PauliBasis, qubit: usize, adjoint: bool) -> Instruction {
    Instruction::T {
        basis,
        qubit,
        adjoint,
    }
}

/// The engine gate a single-qubit Clifford `GateType` names.
///
/// One-to-one and by name: the two enums carry the same stim gate set, spelled
/// with underscores in the IR and in camel case in the engine.
pub(crate) fn single_qubit_clifford(gate: GateType) -> Gate1Q {
    match gate {
        GateType::X => Gate1Q::X,
        GateType::Y => Gate1Q::Y,
        GateType::Z => Gate1Q::Z,
        GateType::H => Gate1Q::H,
        GateType::H_XY => Gate1Q::Hxy,
        GateType::H_YZ => Gate1Q::Hyz,
        GateType::H_NXY => Gate1Q::Hnxy,
        GateType::H_NXZ => Gate1Q::Hnxz,
        GateType::H_NYZ => Gate1Q::Hnyz,
        GateType::SQRT_X => Gate1Q::SqrtX,
        GateType::SQRT_X_DAG => Gate1Q::SqrtXDag,
        GateType::SQRT_Y => Gate1Q::SqrtY,
        GateType::SQRT_Y_DAG => Gate1Q::SqrtYDag,
        GateType::S => Gate1Q::S,
        GateType::S_DAG => Gate1Q::SDag,
        GateType::C_XYZ => Gate1Q::Cxyz,
        GateType::C_ZYX => Gate1Q::Czyx,
        GateType::C_NXYZ => Gate1Q::Cnxyz,
        GateType::C_XNYZ => Gate1Q::Cxnyz,
        GateType::C_XYNZ => Gate1Q::Cxynz,
        GateType::C_NZYX => Gate1Q::Cnzyx,
        GateType::C_ZNYX => Gate1Q::Cznyx,
        GateType::C_ZYNX => Gate1Q::Czynx,
        other => unreachable!("non-single-qubit-Clifford gate {other:?} reached the gate table"),
    }
}

#[cfg(test)]
mod tests {
    use super::{single_qubit_clifford, single_qubit_instruction};
    use crate::backend::{Instruction, PauliBasis};
    use bloq_ir::circuit::GateType;

    /// Pin the `GateType` → [`Gate1Q`](crate::backend::Gate1Q) mapping by its
    /// fingerprint: each gate's `(X→, Z→)` images, which equal stim's tableau
    /// (`Tableau.from_named_gate`). The two enums are spelled almost
    /// identically, so a mistyped arm is exactly the failure this catches; the
    /// engine's own frame composition is pinned in `ticit`, not here.
    #[test]
    fn single_qubit_tableaux_match_stim() {
        use PauliBasis::{X, Y, Z};
        // (gate, X-image (axis, negated), Z-image (axis, negated)).
        type Row = (GateType, (PauliBasis, bool), (PauliBasis, bool));
        let cases: &[Row] = &[
            (GateType::X, (X, false), (Z, true)),
            (GateType::Y, (X, true), (Z, true)),
            (GateType::Z, (X, true), (Z, false)),
            (GateType::H, (Z, false), (X, false)),
            (GateType::H_XY, (Y, false), (Z, true)),
            (GateType::H_YZ, (X, true), (Y, false)),
            (GateType::H_NXY, (Y, true), (Z, true)),
            (GateType::H_NXZ, (Z, true), (X, true)),
            (GateType::H_NYZ, (X, true), (Y, true)),
            (GateType::SQRT_X, (X, false), (Y, true)),
            (GateType::SQRT_X_DAG, (X, false), (Y, false)),
            (GateType::SQRT_Y, (Z, true), (X, false)),
            (GateType::SQRT_Y_DAG, (Z, false), (X, true)),
            (GateType::S, (Y, false), (Z, false)),
            (GateType::S_DAG, (Y, true), (Z, false)),
            (GateType::C_XYZ, (Y, false), (X, false)),
            (GateType::C_ZYX, (Z, false), (Y, false)),
            (GateType::C_NXYZ, (Y, true), (X, true)),
            (GateType::C_XNYZ, (Y, true), (X, false)),
            (GateType::C_XYNZ, (Y, false), (X, true)),
            (GateType::C_NZYX, (Z, true), (Y, true)),
            (GateType::C_ZNYX, (Z, false), (Y, true)),
            (GateType::C_ZYNX, (Z, true), (Y, false)),
        ];
        for &(gate, x_image, z_image) in cases {
            assert_eq!(
                single_qubit_clifford(gate).images(),
                (x_image, z_image),
                "{gate:?}: tableau mismatch vs stim"
            );
        }
    }

    /// Every gate the executor can meet on a one-qubit operand must translate.
    /// A new `GateType` that reaches the `unreachable!` arm of the table fails
    /// here rather than at the first shot that happens to use it.
    #[test]
    fn every_single_qubit_gate_translates() {
        for gate in GateType::iter().filter(|gate| !gate.is_two_qubit_gate()) {
            let instruction = single_qubit_instruction(gate, 0);
            assert_eq!(
                instruction.is_none(),
                gate == GateType::I,
                "{gate:?}: only the identity contributes no instruction"
            );
        }
    }

    /// The non-Clifford arms pick an axis that is *not* the gate's spelling
    /// (`T_YZ` rotates about X, `T_XZ` about Y — bloq's `clifford_proxy`
    /// convention), so unlike the Clifford arms they cannot be checked by name.
    #[test]
    fn reset_and_t_arms_pick_the_right_axis() {
        use PauliBasis::{X, Y, Z};
        let reset = |basis| Some(Instruction::Reset { basis, qubit: 3 });
        let t = |basis, adjoint| {
            Some(Instruction::T {
                basis,
                qubit: 3,
                adjoint,
            })
        };
        let cases = [
            (GateType::RX, reset(X)),
            (GateType::RY, reset(Y)),
            (GateType::RZ, reset(Z)),
            (GateType::T, t(Z, false)),
            (GateType::T_DAG, t(Z, true)),
            (GateType::T_YZ, t(X, false)),
            (GateType::T_YZ_DAG, t(X, true)),
            (GateType::T_XZ, t(Y, false)),
            (GateType::T_XZ_DAG, t(Y, true)),
        ];
        for (gate, expected) in cases {
            assert_eq!(single_qubit_instruction(gate, 3), expected, "{gate:?}");
        }
    }
}
