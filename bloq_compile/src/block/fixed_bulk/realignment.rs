//! Realignment template for temporal Hadamard transitions.
//!
//! When two temporally adjacent surface code blocks require a Hadamard transition,
//! the realignment chunk bridges them by performing syndrome extraction with a
//! non-standard CNOT schedule followed by a transversal Hadamard on all data qubits.

use std::sync::Arc;

use bloq_circuit::{Chunk, ChunkOrLoop, CoordCircuit, Flow, GateType, PauliBasis, PauliMap};
use bloq_graph::{Basis, Pauli};
use glam::IVec2;

use crate::{
    CompileError,
    block::fixed_bulk::observable::logical_line_operator,
    block::fixed_bulk::utils::{
        Corner, Rect, carve_patch, checkerboard_basis, corner_tile, tile_cx_schedule,
    },
    block::gateway::{ChunkMeasurements, GatewayEntry, LocalStabilizer, ObservableGateway},
    block::measurements::MeasurementIndex,
    block::patch::{Patch, Tile},
    block::{CompiledTemplate, LoweringTemplate},
};

/// CX interaction order for a realignment tile.
///
/// Weight-3 variant of the compact CX schedule: same first 3 corners, with the
/// final slot repeating the first corner instead of BR.
fn realignment_cx_schedule(tile_basis: Basis, horizontal_hook_basis: Basis) -> [Corner; 4] {
    let std = tile_cx_schedule(tile_basis, horizontal_hook_basis).compact();
    [std[0], std[1], std[2], std[0]]
}

/// Build the realignment patch: a shifted surface code patch with the top and
/// left boundaries removed.
fn make_realignment_patch(distance: u32, top_basis: Basis) -> Patch {
    let d = distance as i32;
    carve_patch(
        Rect::new(d, d).data_qubits(),
        |m| {
            let basis = checkerboard_basis(m);
            !(m.y == 2 * d && basis != top_basis) && !(m.x == 0 && basis == top_basis)
        },
        |m| m,
        |m, data| {
            let basis = checkerboard_basis(m);
            corner_tile(
                m,
                basis,
                realignment_cx_schedule(basis, top_basis).map(Some),
                data,
            )
        },
    )
}

/// Build the realignment chunk for a temporal Hadamard transition.
///
/// Boundary tiles on the right edge (with `basis == top_basis`) and bottom edge
/// (with `basis != top_basis`) have their reset/measurement basis flipped,
/// creating the stabilizer transition at the patch boundary.
pub(crate) fn build_realignment_chunk(
    distance: u32,
    top_basis: Basis,
) -> Result<Chunk, CompileError> {
    let d = distance as i32;
    let patch = make_realignment_patch(distance, top_basis);

    // The top and left boundary measure qubits stay in the patch (their tiles
    // name the flow supports) but are excluded from the circuit.
    let circuit_qubits = patch
        .used_set()
        .into_iter()
        .filter(|&q| q.y != 2 * d && q.x != 0)
        .collect::<crate::FxSet<_>>();
    let mut circuit = CoordCircuit::new();
    let circuit_tiles: Vec<Tile> = patch
        .tiles()
        .iter()
        .filter(|t| circuit_qubits.contains(&t.measure_qubit()))
        .cloned()
        .collect();

    let mut x_measure_qubits: Vec<IVec2> = Vec::with_capacity(patch.len() / 2);
    let mut z_measure_qubits: Vec<IVec2> = Vec::with_capacity(patch.len() / 2);
    for tile in &circuit_tiles {
        let measure = tile.measure_qubit();
        let mut basis = tile.basis();
        if is_flipped_boundary(measure, basis, top_basis, d) {
            basis = basis.flip();
        }
        match basis {
            Basis::X => x_measure_qubits.push(measure),
            Basis::Z => z_measure_qubits.push(measure),
        }
    }
    circuit.do_gate(GateType::RZ, z_measure_qubits.iter().copied())?;
    circuit.do_gate(GateType::RX, x_measure_qubits.iter().copied())?;
    circuit.tick();

    // CNOT layers
    for k in 0..4 {
        let mut cx_pairs: Vec<IVec2> = Vec::new();
        for tile in &circuit_tiles {
            if let Some(dq) = tile.data_slots()[k] {
                // Standard direction for first two layers; reversed for last two
                let measure_is_control = (tile.basis() == Basis::X) ^ (k >= 2);
                let pair = if measure_is_control {
                    [tile.measure_qubit(), dq]
                } else {
                    [dq, tile.measure_qubit()]
                };
                cx_pairs.extend(pair);
            }
        }
        if !cx_pairs.is_empty() {
            circuit.do_gate(GateType::CX, cx_pairs)?;
        }
        circuit.tick();
    }

    // Transversal Hadamard rotation.
    // Boundary tiles measure in their reset basis; interior tiles measure in
    // the opposite basis (Hadamard transition swaps the stabilizer type).
    circuit.do_gate(GateType::H, patch.data_set())?;

    // Measurement.
    let (mut mz_meas, mut mx_meas): (Vec<IVec2>, Vec<IVec2>) = (Vec::new(), Vec::new());
    for (&m, reset_basis) in z_measure_qubits
        .iter()
        .map(|m| (m, Basis::Z))
        .chain(x_measure_qubits.iter().map(|m| (m, Basis::X)))
    {
        let on_boundary = on_transition_boundary(m, d);
        let meas_basis = if on_boundary {
            reset_basis
        } else {
            reset_basis.flip()
        };
        match meas_basis {
            Basis::X => mx_meas.push(m),
            Basis::Z => mz_meas.push(m),
        }
    }
    circuit.measure(PauliBasis::Z, mz_meas);
    circuit.measure(PauliBasis::X, mx_meas);

    // Construct flows
    let mut flows = Vec::new();
    let measurement_ids = MeasurementIndex::from_circuit(&circuit);

    for tile in patch.tiles() {
        let basis = tile.basis();
        let pauli = Pauli::from(basis);
        let pauli_after_h = pauli.flip();
        let measure = tile.measure_qubit();
        let stabilizer: PauliMap = Corner::ALL
            .iter()
            .filter_map(|&c| {
                let dq = measure + c.to_ivec2();
                if circuit_qubits.contains(&dq) {
                    Some((dq, pauli))
                } else {
                    None
                }
            })
            .collect();

        let on_boundary = on_transition_boundary(measure, d);
        let on_flipped = is_flipped_boundary(measure, basis, top_basis, d);
        let on_same = on_boundary && !on_flipped;

        // Signed offset toward the partner measure qubit used by in-flows
        // and flipped-boundary out-flows. For top_basis-matching tiles the
        // partner sits 2 rows below (-Y); for opposite-basis tiles it sits
        // 2 columns to the right (+X).
        let toward_partner = if basis == top_basis {
            IVec2::new(0, -2)
        } else {
            IVec2::new(2, 0)
        };

        // In-flow
        if !on_boundary {
            let meas = measure + toward_partner;
            flows.push(
                Flow::new(stabilizer.clone(), PauliMap::empty())
                    .with_measurements([measurement_ids.expect_measurement(meas)])
                    .with_center(measure),
            );
        } else if on_same {
            // Same-basis boundary: use own measurement.
            flows.push(
                Flow::new(stabilizer.clone(), PauliMap::empty())
                    .with_measurements([measurement_ids.expect_measurement(measure)])
                    .with_center(measure),
            );
        }

        // Out-flow
        if !circuit_qubits.contains(&measure) {
            continue;
        }
        if on_flipped {
            // Flipped boundary: use own measurement plus the adjacent
            // tile's measurement (if it exists in the circuit).
            let meas_qubits = vec![measure, measure + toward_partner];
            flows.push(
                Flow::new(PauliMap::empty(), stabilizer.clone())
                    .with_measurements(
                        meas_qubits
                            .into_iter()
                            .filter(|m| circuit_qubits.contains(m))
                            .map(|m| measurement_ids.expect_measurement(m)),
                    )
                    .with_center(measure),
            );
        } else {
            // Interior or same-basis boundary: reference the adjacent tile
            // in the post-Hadamard direction.
            let meas = measure - toward_partner;
            let end_stabilizer: PauliMap = Corner::ALL
                .iter()
                .map(|c| meas + c.to_ivec2())
                .filter_map(|m| {
                    if circuit_qubits.contains(&m) {
                        Some((m, pauli_after_h))
                    } else {
                        None
                    }
                })
                .collect();
            flows.push(
                Flow::new(PauliMap::empty(), end_stabilizer)
                    .with_measurements(
                        circuit_qubits
                            .contains(&meas)
                            .then(|| measurement_ids.expect_measurement(meas)),
                    )
                    .with_center(meas),
            );
        }
    }

    Ok(Chunk { circuit, flows })
}

pub(crate) fn compile_realignment(
    distance: u32,
    top_basis: Basis,
) -> Result<CompiledTemplate, CompileError> {
    let chunk = build_realignment_chunk(distance, top_basis)?;
    let gateway = realignment_gateway(distance, top_basis, &chunk);
    Ok(Arc::new(LoweringTemplate::from_chunks(
        vec![ChunkOrLoop::Single(Box::new(chunk))],
        gateway,
    )?))
}

/// The right and bottom edges: the two boundaries whose tiles take part in the
/// stabilizer transition.
fn on_transition_boundary(measure: IVec2, d: i32) -> bool {
    measure.x == 2 * d || measure.y == 0
}

/// Whether a boundary tile should have its basis flipped for the transition:
/// right-edge tiles matching `top_basis`, bottom-edge tiles not matching it.
fn is_flipped_boundary(measure: IVec2, basis: Basis, top_basis: Basis, d: i32) -> bool {
    (measure.x == 2 * d && basis == top_basis) || (measure.y == 0 && basis != top_basis)
}

pub(crate) fn realignment_observable_meas_qubit(
    distance: u32,
    top_basis: Basis,
    in_observable_basis: Basis,
) -> Option<IVec2> {
    if (distance % 4 == 3) ^ (in_observable_basis == Basis::X) {
        return None;
    }
    let d = distance as i32;
    if (distance % 4 == 3) ^ (top_basis == Basis::Z) {
        Some(IVec2::new(d + 1, 0))
    } else {
        Some(IVec2::new(2 * d, d - 1))
    }
}

fn realignment_gateway(distance: u32, top_basis: Basis, chunk: &Chunk) -> ObservableGateway {
    let mut gateway = ObservableGateway::new();
    let measurement_ids = MeasurementIndex::from_circuit(&chunk.circuit);
    for observable_basis in [Basis::X, Basis::Z] {
        let measurements = if let Some(qubit) =
            realignment_observable_meas_qubit(distance, top_basis, observable_basis)
        {
            vec![ChunkMeasurements {
                chunk_index: 0,
                measurements: vec![measurement_ids.expect_measurement(qubit)],
            }]
        } else {
            Vec::new()
        };
        // The realignment applies the logical Hadamard *in place*: a
        // transversal H flips each data qubit's basis
        // without moving it, so the operator exits on the same middle-line
        // support it entered with X↔Z swapped. `operator_in` therefore equals
        // the lower cube's `operator_out` and `operator_out` the upper cube's
        // `operator_in`, so both temporal seams cancel during lowering.
        let pauli_in = Pauli::from(observable_basis);
        let operator_in = logical_line_operator(distance, observable_basis, top_basis, pauli_in);
        let operator_out =
            logical_line_operator(distance, observable_basis, top_basis, pauli_in.flip());
        gateway.insert(
            LocalStabilizer::isolated(pauli_in),
            GatewayEntry {
                measurements,
                operator_in,
                operator_out,
            },
        );
    }
    gateway
}

#[cfg(test)]
mod tests {
    use bloq_stim::StimFlowVerifier;
    use rstest::rstest;

    use bloq_graph::Basis;

    use super::*;
    use bloq_circuit::Op;

    use crate::block::fixed_bulk::split_ops_into_moments;

    #[rstest]
    fn test_realignment_chunk_flows(
        #[values(3, 5, 7)] distance: u32,
        #[values(Basis::X, Basis::Z)] top_basis: Basis,
    ) {
        let chunk = build_realignment_chunk(distance, top_basis).expect("realignment chunk");
        chunk.verify_flows(None, None).unwrap();
    }

    #[rstest]
    fn test_realignment_chunk_merges_rotation_with_measurement(
        #[values(3, 5, 7)] distance: u32,
        #[values(Basis::X, Basis::Z)] top_basis: Basis,
    ) {
        let chunk = build_realignment_chunk(distance, top_basis).expect("realignment chunk");
        let moments = split_ops_into_moments(
            chunk
                .circuit
                .body(chunk.circuit.entry_body())
                .unwrap()
                .ops(),
        );
        assert_eq!(moments.len(), 6);
        let final_moment = moments
            .get(5)
            .expect("realignment keeps final rotation and measurement moment");
        assert!(final_moment.iter().any(|op| matches!(
            op,
            Op::Gate {
                gate: GateType::H,
                ..
            }
        )));
        assert!(
            final_moment
                .iter()
                .any(|op| matches!(op, Op::Measure { .. }))
        );
    }
}
