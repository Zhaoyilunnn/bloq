//! Basis-evolution helpers for internal-measurement correction.

use glam::IVec3;

use crate::{
    Pauli, PauliBasis, RuntimeBasisError, RuntimeStabilizerBasis, Stabilizer,
    SymbolicOutputCorrection, solve_output_correction_symbolic,
};

use super::{MeasurementKey, VerifyLogicalError};

/// One selective-fill replay step: apply `chosen` at `pos` over `basis` (with
/// the T-port fallback) and return the evolved basis together with the
/// measurement key it records.
pub(crate) fn replay_fill(
    basis: &RuntimeStabilizerBasis,
    pos: IVec3,
    chosen: PauliBasis,
) -> Result<(RuntimeStabilizerBasis, MeasurementKey), VerifyLogicalError> {
    let basis = apply_selective_fill_with_fallback(basis, pos, chosen)?;
    let key = MeasurementKey::Node {
        pos,
        pauli: Pauli::from(chosen),
    };
    Ok((basis, key))
}

/// Applies a selective fill at `pos` in the `chosen` basis, retrying with T
/// nodes demoted to ports if the fill would otherwise be non-Clifford.
fn apply_selective_fill_with_fallback(
    basis: &RuntimeStabilizerBasis,
    pos: IVec3,
    chosen: PauliBasis,
) -> Result<RuntimeStabilizerBasis, VerifyLogicalError> {
    match basis.apply_selective_fill(pos, chosen) {
        Ok(transition) => Ok(transition),
        Err(RuntimeBasisError::NonCliffordFillUnsupported) => basis
            .clone()
            .with_t_nodes_as_ports()
            .apply_selective_fill(pos, chosen)
            .map_err(|err| VerifyLogicalError::RuntimeBasisUpdateFailed { pos, source: err }),
        Err(err) => Err(VerifyLogicalError::RuntimeBasisUpdateFailed { pos, source: err }),
    }
}

/// The evolved-basis terminal solve: derive the output-correction surfaces of
/// `basis`, keep only the measured-support constraints
/// (`has_support`, mirroring `solve_output_correction`'s parity-only skip), and
/// run the shared symbolic solve. Returns the surviving constraints alongside
/// the solve so callers fold their own runtime RHS.
pub(crate) fn solve_evolved_correction(
    basis: &RuntimeStabilizerBasis,
    outputs: &[IVec3],
    mut has_support: impl FnMut(&Stabilizer) -> bool,
) -> Result<(Vec<Stabilizer>, SymbolicOutputCorrection), VerifyLogicalError> {
    let derived = basis
        .derive_output_correction_surfaces(outputs)
        .map_err(|_| VerifyLogicalError::OutputSurfaceUnavailable(outputs.to_vec()))?;
    let (constraints, surfaces): (Vec<_>, Vec<_>) = derived
        .into_iter()
        .filter(|(_, surface)| has_support(surface))
        .unzip();
    let symbolic = solve_output_correction_symbolic(outputs, &constraints)?;
    Ok((surfaces, symbolic))
}
