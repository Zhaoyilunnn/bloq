//! Linear-size exact checks for stabilizer Choi states and preparations.

#![allow(dead_code, reason = "shared by independent integration-test binaries")]

use std::collections::{BTreeMap, BTreeSet};

use bloq_vm::{EnginePauli, EnginePauliString, OutputLogical, Simulator};
use glam::IVec3;

pub(crate) fn physical_qubit(qubit: usize, width: usize) -> OutputLogical {
    OutputLogical {
        port: IVec3::ZERO,
        logical_x: EnginePauliString::single(width, qubit, EnginePauli::X),
        logical_z: EnginePauliString::single(width, qubit, EnginePauli::Z),
        consumed: false,
    }
}

/// Read one signed logical Pauli, including each output's correction frame.
/// A partial set of these assertions does not certify a complete Choi state.
pub(crate) fn assert_pauli_expectation(
    sim: &Simulator,
    outputs: &[(&OutputLogical, (bool, bool))],
    logical: &EnginePauliString,
    expected: f64,
) {
    assert_eq!(logical.nqubits, outputs.len());
    let mut product = EnginePauliString::new(sim.num_qubits());
    product.set_phase(logical.phase_exponent());
    for (index, (output, (x, z))) in outputs.iter().enumerate() {
        assert!(!output.consumed, "capture logical outputs before reuse");
        let axis = logical.get(index);
        if matches!(axis, EnginePauli::X | EnginePauli::Y) {
            product = &product * &super::widen(&output.logical_x, sim.num_qubits());
            product.set_phase(product.phase_exponent() + 2 * i32::from(*z));
        }
        if matches!(axis, EnginePauli::Z | EnginePauli::Y) {
            product = &product * &super::widen(&output.logical_z, sim.num_qubits());
            product.set_phase(product.phase_exponent() + 2 * i32::from(*x));
        }
        if axis == EnginePauli::Y {
            product.set_phase(product.phase_exponent() + 1);
        }
    }
    let actual = sim
        .peek_observable_expectation(&product)
        .expect("Choi observable is Hermitian on live qubits");
    assert!(
        (actual - expected).abs() < 1e-9,
        "logical {logical:?}: {actual} != {expected}"
    );
}

/// Signed +1 generators must have full rank, so they determine the whole state.
pub(crate) fn assert_stabilizer_state(
    sim: &Simulator,
    outputs: &[(&OutputLogical, (bool, bool))],
    generators: &[EnginePauliString],
) {
    let operators = outputs
        .iter()
        .flat_map(|(output, _)| [&output.logical_x, &output.logical_z])
        .map(|operator| super::widen(operator, sim.num_qubits()))
        .collect::<Vec<_>>();
    for (i, left) in operators.iter().enumerate() {
        for (j, right) in operators.iter().enumerate().skip(i + 1) {
            assert_eq!(
                left.commutes_with(right),
                i / 2 != j / 2,
                "independent logical output axes"
            );
        }
    }
    let mut pivots = BTreeMap::<usize, BTreeSet<usize>>::new();
    for generator in generators {
        assert_pauli_expectation(sim, outputs, generator, 1.0);
        let mut columns = BTreeSet::new();
        for index in 0..outputs.len() {
            let axis = generator.get(index);
            if matches!(axis, EnginePauli::X | EnginePauli::Y) {
                columns.insert(2 * index);
            }
            if matches!(axis, EnginePauli::Z | EnginePauli::Y) {
                columns.insert(2 * index + 1);
            }
        }
        while let Some(&first) = columns.first() {
            let Some(pivot) = pivots.get(&first) else {
                pivots.insert(first, columns);
                break;
            };
            columns = columns.symmetric_difference(pivot).copied().collect();
        }
    }
    assert_eq!(
        pivots.len(),
        outputs.len(),
        "complete independent Choi stabilizers"
    );
}
