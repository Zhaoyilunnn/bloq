//! Exact logical-state readouts without ancillas or simulator mutation.

use std::sync::OnceLock;

use crate::backend::{PauliString, Simulator};

use crate::{ExecError, OutputLogical};

/// Read a logical Bloch vector, corrected by the supplied `(X, Z)` frame bits.
/// Use `(false, false)` to read the uncorrected state. Signed representatives
/// are preserved, including `Y_L = i X_L Z_L`.
///
/// # Errors
/// Propagates engine errors from malformed logical observables.
pub fn logical_bloch(
    sim: &Simulator,
    output: &OutputLogical,
    frame: (bool, bool),
) -> Result<(f64, f64, f64), ExecError> {
    let signature = logical_signature(sim, &[(output, frame)])?;
    Ok((signature[0], signature[1], signature[2]))
}

/// Compute all nonidentity joint Pauli expectations of the live logical outputs.
///
/// Values are corrected by their `(X, Z)` frame bits. Output zero is the
/// least-significant base-four digit, with digits `I, X, Y, Z`. This determines
/// the full reduced logical state, including entanglement, without measuring it.
/// Representatives of different widths are extended with trailing identities.
/// For `n` outputs, this allocates `4^n - 1` values and makes that many exact
/// simulator expectation reads; use it only for small output registers.
///
/// # Errors
/// [`ExecError::LogicalSignatureTooLarge`] if `4^n` overflows `usize`;
/// otherwise propagates engine errors from malformed logical observables.
pub fn logical_signature(
    sim: &Simulator,
    outputs: &[(&OutputLogical, (bool, bool))],
) -> Result<Vec<f64>, ExecError> {
    let cardinality = u32::try_from(outputs.len())
        .ok()
        .and_then(|n| 4usize.checked_pow(n))
        .ok_or(ExecError::LogicalSignatureTooLarge {
            outputs: outputs.len(),
        })?;
    let width = outputs
        .iter()
        .map(|(out, _)| out.logical_x.nqubits.max(out.logical_z.nqubits))
        .max()
        .unwrap_or(0);
    let logicals: Vec<_> = outputs
        .iter()
        .map(|(out, frame)| {
            (
                crate::circuit::widen_pauli(&out.logical_x, width),
                crate::circuit::widen_pauli(&out.logical_z, width),
                *frame,
            )
        })
        .collect();
    (1..cardinality)
        .map(|mut mask| {
            let mut product = PauliString::new(width);
            for (logical_x, logical_z, (x, z)) in &logicals {
                let digit = mask & 3;
                if digit == 1 || digit == 2 {
                    product = &product * logical_x;
                    product.set_phase(product.phase_exponent() + 2 * i32::from(*z));
                }
                if digit == 2 || digit == 3 {
                    product = &product * logical_z;
                    product.set_phase(product.phase_exponent() + 2 * i32::from(*x));
                }
                if digit == 2 {
                    product.set_phase(product.phase_exponent() + 1);
                }
                mask >>= 2;
            }
            sim.peek_observable_expectation(&product)
                .map_err(Into::into)
        })
        .collect()
}

/// Exact Pauli signature of `CCZ|+++⟩`, prepared by the simulator itself.
///
/// # Panics
///
/// Panics if the simulator rejects CCZ on three freshly allocated qubits.
#[must_use]
pub fn ccz_state_signature() -> &'static [f64] {
    static SIGNATURE: OnceLock<Vec<f64>> = OnceLock::new();
    SIGNATURE.get_or_init(|| {
        use crate::backend::Pauli;
        let mut sim = Simulator::with_seed(3, 0);
        for q in 0..3 {
            sim.h(q);
        }
        sim.ccz(0, 1, 2).expect("CCZ on three fresh qubits");
        let outputs: Vec<_> = (0..3)
            .map(|q| OutputLogical {
                port: glam::IVec3::ZERO,
                logical_x: PauliString::single(3, q, Pauli::X),
                logical_z: PauliString::single(3, q, Pauli::Z),
                consumed: false,
            })
            .collect();
        logical_signature(
            &sim,
            &outputs
                .iter()
                .map(|out| (out, (false, false)))
                .collect::<Vec<_>>(),
        )
        .expect("valid three-qubit logicals")
    })
}

/// All distinct Pauli-byproduct signatures of `CCZ|+++⟩`.
#[must_use]
pub fn ccz_orbit_signatures() -> &'static [Vec<f64>] {
    static SIGNATURES: OnceLock<Vec<Vec<f64>>> = OnceLock::new();
    SIGNATURES.get_or_init(|| {
        (0..64usize)
            .map(|frame| {
                ccz_state_signature()
                    .iter()
                    .enumerate()
                    .map(|(index, expectation)| {
                        let mut mask = index + 1;
                        let mut frame = frame;
                        let mut flip = false;
                        for _ in 0..3 {
                            let pauli = mask & 3;
                            let byproduct = frame & 3;
                            flip ^= pauli != 0 && byproduct != 0 && pauli != byproduct;
                            mask >>= 2;
                            frame >>= 2;
                        }
                        expectation * sign(flip)
                    })
                    .collect()
            })
            .collect()
    })
}

/// Whether two Pauli signatures agree entry-wise within a tight tolerance.
#[must_use]
pub fn signatures_match(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
}

fn sign(bit: bool) -> f64 {
    if bit { -1.0 } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Pauli;

    #[test]
    fn signed_logical_readouts_preserve_y_and_frame_without_mutation() {
        let mut sim = Simulator::with_seed(2, 0);
        sim.h(0);
        sim.t(0).unwrap();
        sim.cx(0, 1).unwrap();
        let mut output = OutputLogical {
            port: glam::IVec3::ZERO,
            logical_x: &PauliString::single(2, 0, Pauli::X) * &PauliString::single(2, 1, Pauli::X),
            logical_z: PauliString::single(1, 0, Pauli::Z),
            consumed: false,
        };
        let r = std::f64::consts::FRAC_1_SQRT_2;
        for x_sign in [false, true] {
            for z_sign in [false, true] {
                output.logical_x.set_phase(2 * i32::from(x_sign));
                output.logical_z.set_phase(2 * i32::from(z_sign));
                let (x, y, z) = logical_bloch(&sim, &output, (false, false)).unwrap();
                assert!((x - r * sign(x_sign)).abs() < 1e-9);
                assert!((y - r * sign(x_sign ^ z_sign)).abs() < 1e-9);
                assert!(z.abs() < 1e-9);
                let signature = logical_signature(&sim, &[(&output, (z_sign, x_sign))]).unwrap();
                assert!(signatures_match(&signature, &[r, r, 0.0]));
            }
        }
        assert_eq!(sim.num_qubits(), 2);
        assert_eq!(sim.peek_z(0).unwrap(), 0.0);
        sim.reset(0).unwrap();
        sim.reset(1).unwrap();
        // Logical representatives may be narrower than the live engine after
        // input injection or early-output capture allocates a private qubit.
        sim.reset(2).unwrap();
        assert_eq!(
            logical_bloch(&sim, &output, (false, false)).unwrap().2,
            -1.0
        );
        assert!(signatures_match(
            &logical_signature(&sim, &[(&output, (true, false))]).unwrap(),
            &[0.0, 0.0, 1.0],
        ));
    }

    #[test]
    fn joint_signature_extends_mixed_widths_and_preserves_signed_correlations() {
        let mut sim = Simulator::with_seed(2, 0);
        sim.h(0);
        sim.cx(0, 1).unwrap();
        let first = OutputLogical {
            port: glam::IVec3::ZERO,
            logical_x: PauliString::single(1, 0, Pauli::X),
            logical_z: PauliString::single(1, 0, Pauli::Z),
            consumed: false,
        };
        let mut second = OutputLogical {
            port: glam::IVec3::X,
            logical_x: PauliString::single(2, 1, Pauli::X),
            logical_z: PauliString::single(2, 1, Pauli::Z),
            consumed: false,
        };
        second.logical_z.set_phase(2);
        let signature =
            logical_signature(&sim, &[(&first, (false, false)), (&second, (true, false))]).unwrap();
        let mut bell = vec![0.0; 15];
        bell[4] = 1.0; // XX
        bell[9] = -1.0; // YY
        bell[14] = 1.0; // ZZ
        assert!(signatures_match(&signature, &bell));

        // 4^(word_bits/2) cannot fit in usize; reject before allocation or reads.
        let count = usize::BITS as usize / 2;
        assert!(matches!(
            logical_signature(&sim, &vec![(&first, (false, false)); count]),
            Err(ExecError::LogicalSignatureTooLarge { outputs }) if outputs == count,
        ));
    }

    #[test]
    fn ccz_orbit_accepts_paulis_and_rejects_non_clifford_twist() {
        let mut sim = Simulator::with_seed(3, 0);
        for q in 0..3 {
            sim.h(q);
        }
        sim.ccz(0, 1, 2).unwrap();
        sim.x(0);
        let outputs: Vec<_> = (0..3)
            .map(|q| OutputLogical {
                port: glam::IVec3::ZERO,
                logical_x: PauliString::single(3, q, Pauli::X),
                logical_z: PauliString::single(3, q, Pauli::Z),
                consumed: false,
            })
            .collect();
        let outputs: Vec<_> = outputs.iter().map(|out| (out, (false, false))).collect();
        let orbit = ccz_orbit_signatures();
        assert_eq!(orbit.len(), 64);
        for (i, member) in orbit.iter().enumerate() {
            assert!(
                !orbit[..i]
                    .iter()
                    .any(|other| signatures_match(member, other))
            );
        }
        let signature = logical_signature(&sim, &outputs).unwrap();
        assert!(
            orbit
                .iter()
                .any(|member| signatures_match(member, &signature))
        );
        sim.t(0).unwrap();
        let signature = logical_signature(&sim, &outputs).unwrap();
        assert!(
            !orbit
                .iter()
                .any(|member| signatures_match(member, &signature))
        );
    }
}
