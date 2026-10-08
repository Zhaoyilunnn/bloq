//! Selective-block compilation: a dynamic single-qubit-basis measurement.
//!
//! A `Selective(kind)` block resolves at runtime to one of two Pauli-basis
//! measurements — `kind.pauli_if_true()` or `kind.pauli_if_false()`. The block
//! compiler builds **both** arms as independent templates with a shared output
//! signature, registered as complementary guarded instances; resolve lowering
//! feeds their selector.
//!
//! Each arm is one of:
//! - **X / Z basis** — one normal syndrome round whose final moment measures
//!   the ancillas and every data qubit ([`compile_measurement`]).
//!   The final readout reconstructs same-basis stabilizers and discards the
//!   opposite basis after the closing round has checked both incoming bases.
//! - **Y basis** — reuses the Y block template ([`compile_y`]).
//!
//! The closing round separates the readout face from the preceding cube's top
//! face. Without it, a spatial split's side face can conflict with the temporal
//! pipe's readout face, and discarding complementary checks admits short logical
//! errors. Decoder-wait rounds can provide this separation in live selective
//! execution, but fixed and statically pinned readouts cannot assume a wait.
//!
//! This minimal static example has one logical Z surface on blocks 0, 1, and 2
//! and pipes `0 -> 1` and `0 -> 2` (generator 0, emitted as `L0`). At distance 7,
//! immediate X readout admits a six-fault logical error; the closing round
//! restores distance 7 without changing the cube schedules:
//!
//! ```text
//! BLOG 1.0
//! module main {
//!   0: XXZ [0, 0, 0]
//!   1: ZXZ [-1, 0, 0]
//!   2: XZZ [0, 1, 0]
//!   3: ZXZ [1, 0, 0]
//!   4: X [1, 0, 1]
//!   0 -> 1
//!   0 -> 2
//!   0 -> 3
//!   3 -> 4
//! }
//! ```
//!
//! At distance 11, one closing round gives graphlike distance 10; one extra
//! memory round on pipe `3 -> 4` restores 11. Larger distances can need more
//! padding, so check the complete emitted circuit's fault distance.

use std::sync::Arc;

use bloq_circuit::{ChunkOrLoop, Pauli, PauliMap};
use bloq_graph::{Basis, PauliBasis, SelectiveKind};

use crate::CompileError;
use crate::block::fixed_bulk::observable::{line_qubits, logical_line_operator};
use crate::block::fixed_bulk::utils::{make_normal_surface_code_patch, make_surface_code_chunk};
use crate::block::fixed_bulk::ybasis::compile_y;
use crate::block::gateway::{ChunkMeasurements, GatewayEntry, LocalStabilizer, ObservableGateway};
use crate::block::measurements::MeasurementIndex;
use crate::block::{CompiledTemplate, LoweringTemplate};
use crate::signature::Connectivity;

/// The two per-basis measurement templates a selective block lowers into.
/// `when_true` / `when_false` correspond to `pauli_if_true` / `pauli_if_false`,
/// matching the simulator's `ResolveSpec` (`sim/compile.rs`).
#[derive(Debug)]
pub(crate) struct SelectiveTemplates {
    pub(crate) when_true: CompiledTemplate,
    pub(crate) when_false: CompiledTemplate,
}

/// Compile a selective block into its two per-basis arm templates.
pub(crate) fn compile_selective(
    connectivity: Connectivity,
    distance: u32,
    boundary_basis: Basis,
    kind: SelectiveKind,
) -> Result<SelectiveTemplates, CompileError> {
    let when_true =
        compile_selective_arm(kind.pauli_if_true(), connectivity, distance, boundary_basis)?;
    let when_false = compile_selective_arm(
        kind.pauli_if_false(),
        connectivity,
        distance,
        boundary_basis,
    )?;

    // Both arms must consume the same incoming stabilizers.
    validate_shared_input_seam(kind, &when_true, &when_false)?;

    Ok(SelectiveTemplates {
        when_true,
        when_false,
    })
}

fn compile_selective_arm(
    basis: PauliBasis,
    connectivity: Connectivity,
    distance: u32,
    boundary_basis: Basis,
) -> Result<CompiledTemplate, CompileError> {
    match basis {
        PauliBasis::Y => compile_y(boundary_basis, connectivity, distance),
        PauliBasis::X => compile_measurement(Basis::X, connectivity, distance, boundary_basis),
        PauliBasis::Z => compile_measurement(Basis::Z, connectivity, distance, boundary_basis),
    }
}

/// Close incoming syndrome history and read out data with the round's ancillas.
pub(crate) fn compile_measurement(
    basis: Basis,
    connectivity: Connectivity,
    distance: u32,
    boundary_basis: Basis,
) -> Result<CompiledTemplate, CompileError> {
    let patch = make_normal_surface_code_patch(distance, boundary_basis);
    let meas_data = patch.data_set().into_iter().map(|q| (q, basis)).collect();
    let chunk = make_surface_code_chunk(&patch, None, Some(&meas_data))?;
    let measurement_ids = MeasurementIndex::from_circuit(&chunk.circuit);
    let gateway = build_transversal_gateway(
        basis,
        boundary_basis,
        distance,
        connectivity,
        &measurement_ids,
    );
    let chunks = vec![ChunkOrLoop::Single(Box::new(chunk))];
    Ok(Arc::new(LoweringTemplate::from_chunks(chunks, gateway)?))
}

/// Build the observable gateway for a transversal measurement (terminate case).
///
/// The logical operator is the middle line of [`logical_line_operator`]. The
/// data-qubit records along that line reconstruct the logical. The incoming pipe
/// is `-Z`, so the logical enters on the −Z face (`operator_in`) and terminates
/// here (`operator_out` empty). The weight-0 isolated key covers the block's own
/// readout with no incoming operator.
fn build_transversal_gateway(
    basis: Basis,
    boundary_basis: Basis,
    distance: u32,
    connectivity: Connectivity,
    measurement_ids: &MeasurementIndex,
) -> ObservableGateway {
    let pauli = Pauli::from(basis);
    let operator_in = logical_line_operator(distance, basis, boundary_basis, pauli);
    let line_coords = line_qubits(distance, basis != boundary_basis);

    let measurements = vec![ChunkMeasurements {
        chunk_index: 0,
        measurements: line_coords
            .iter()
            .map(|&q| measurement_ids.expect_measurement(q))
            .collect(),
    }];

    let mut gateway = ObservableGateway::new();
    for conn in [Connectivity::ISOLATED, connectivity] {
        let key = LocalStabilizer::new(pauli, conn);
        let operator_in_face = if conn.is_isolated() {
            PauliMap::empty()
        } else {
            operator_in.clone()
        };
        gateway.insert(
            key,
            GatewayEntry {
                measurements: measurements.clone(),
                operator_in: operator_in_face,
                operator_out: PauliMap::empty(),
            },
        );
    }
    gateway
}

/// Incoming stabilizer supports, ignoring basis-dependent records and discard
/// markers. Both arms must consume the same quantum seam.
fn seam_supports(template: &LoweringTemplate) -> crate::FxSet<PauliMap> {
    template
        .program_template
        .boundary_flows
        .iter()
        .filter(|flow| flow.end.is_empty() && !flow.start.is_empty())
        .map(|flow| flow.start.clone())
        .collect()
}

fn validate_shared_input_seam(
    kind: SelectiveKind,
    when_true: &LoweringTemplate,
    when_false: &LoweringTemplate,
) -> Result<(), CompileError> {
    if seam_supports(when_true) != seam_supports(when_false) {
        return Err(CompileError::SelectiveArmSignatureMismatch {
            kind,
            reason: "the two arms present different incoming quantum seams".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bloq_circuit::{FlowMarker, Op};
    use bloq_graph::Direction;

    use super::*;

    /// X/Z arms consume the same normal patch and measure their declared bases.
    #[test]
    fn xz_selective_arms_share_seam_signature() {
        let connectivity = Connectivity::ISOLATED.with_pipe(Direction::ZMINUS);
        let templates = compile_selective(connectivity, 3, Basis::Z, SelectiveKind::XZ)
            .expect("XZ selective compiles with matching arm signatures");

        for (template, expected_basis) in [
            (&templates.when_true, PauliBasis::X),
            (&templates.when_false, PauliBasis::Z),
        ] {
            assert_eq!(
                seam_supports(template).len(),
                8,
                "d3 has eight seam stabilizers"
            );
            let circuit = &template.program_template.circuit;
            assert!(matches!(
                circuit.body(circuit.entry_body()).unwrap().ops().last(),
                Some(Op::Measure { basis, .. }) if *basis == expected_basis
            ));
        }
    }

    #[test]
    fn fixed_measurement_closes_both_syndrome_bases_before_readout() {
        let distance = 3;
        let connectivity = Connectivity::ISOLATED.with_pipe(Direction::ZMINUS);

        for boundary_basis in [Basis::X, Basis::Z] {
            for (basis, pauli_basis, pauli) in [
                (Basis::X, PauliBasis::X, Pauli::X),
                (Basis::Z, PauliBasis::Z, Pauli::Z),
            ] {
                let template = compile_measurement(basis, connectivity, distance, boundary_basis)
                    .expect("fixed measurement compiles");
                let circuit = &template.program_template.circuit;
                let body = circuit.body(circuit.entry_body()).expect("entry body");
                let Some(Op::Measure {
                    basis: actual_basis,
                    qubits,
                    measurements,
                    ..
                }) = body.ops().last()
                else {
                    panic!("fixed measurement must end with the transversal readout");
                };
                assert_eq!(*actual_basis, pauli_basis);
                assert_eq!(qubits.len(), (distance * distance) as usize);
                assert_eq!(measurements.len(), qubits.len());
                let last_tick = body
                    .ops()
                    .iter()
                    .rposition(|op| matches!(op, Op::Tick))
                    .expect("syndrome round has a final CX barrier");
                assert!(
                    body.ops()[..last_tick]
                        .iter()
                        .all(|op| !matches!(op, Op::Measure { .. })),
                    "ancilla and data readout share the final moment"
                );

                let patch = make_normal_surface_code_patch(distance, boundary_basis);
                assert_eq!(
                    circuit.num_measurements() as usize,
                    patch.len() + (distance * distance) as usize
                );
                for ancilla_basis in [PauliBasis::X, PauliBasis::Z] {
                    let measured: crate::FxSet<_> = body.ops()[..body.ops().len() - 1]
                        .iter()
                        .filter_map(|op| match op {
                            Op::Measure { basis, qubits, .. } if *basis == ancilla_basis => {
                                Some(qubits.iter().copied())
                            }
                            _ => None,
                        })
                        .flatten()
                        .collect();
                    let expected = patch
                        .tiles()
                        .iter()
                        .filter(|tile| PauliBasis::from(tile.basis()) == ancilla_basis)
                        .map(crate::block::patch::Tile::measure_qubit)
                        .collect();
                    assert_eq!(measured, expected);
                }
                let flows = &template.program_template.boundary_flows;
                assert_eq!(flows.len(), patch.tiles().len());
                assert!(
                    flows
                        .iter()
                        .all(|flow| !flow.start.is_empty() && flow.end.is_empty())
                );
                assert!(flows.iter().all(|flow| {
                    flow.marker == FlowMarker::Detector && !flow.measurements.is_empty()
                }));

                let entry =
                    &template.observable_gateway[&LocalStabilizer::new(pauli, connectivity)];
                assert_eq!(entry.measurements[0].measurements.len(), distance as usize);
                assert_eq!(entry.measurements[0].chunk_index, 0);
                assert!(
                    entry.measurements[0]
                        .measurements
                        .iter()
                        .all(|&id| id >= patch.len() as u32)
                );
                assert!(!entry.operator_in.is_empty());
                assert!(entry.operator_out.is_empty());
            }
        }
    }
}
