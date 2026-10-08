//! Fixture builders shared by the executor modules' test suites.
//!
//! Every executor fixture needs the same two pieces — a trivial one-qubit
//! template and a quantum node holding exactly one instance of it — so they
//! live here instead of being re-typed in each module's `mod tests`.

use bloq_ir::circuit::{CoordCircuit, GateType};
use bloq_ir::lowering::{BloqTemplate, TemplateInstance, TemplateInstanceId};
use bloq_ir::{Bloq, BloqNode, QuantumNode, TemplateId};
use glam::{IVec2, ivec2};

/// Add a template whose circuit is `gate` on the single qubit at the origin.
pub(super) fn one_qubit_template(bloq: &mut Bloq, gate: GateType) -> TemplateId {
    let mut circuit = CoordCircuit::new();
    circuit
        .do_gate(gate, [ivec2(0, 0)])
        .expect("a single-qubit gate on one coordinate is valid");
    bloq.add_template(BloqTemplate::new(circuit))
}

/// A quantum node carrying exactly `instance` of `template`, placed at `offset`.
pub(super) fn quantum_node(template: TemplateId, instance: u32, offset: IVec2) -> BloqNode {
    BloqNode::quantum(QuantumNode {
        instances: vec![TemplateInstance::new(
            TemplateInstanceId(instance),
            template,
            offset,
        )],
        ..QuantumNode::default()
    })
}

/// Guard all of a fixture node's instances and side tables with one input.
pub(super) fn guarded_node(mut node: BloqNode, input: u32) -> BloqNode {
    let quantum = node.expect_quantum_mut();
    quantum.guards.push(bloq_ir::QuantumGuard {
        input,
        instances: quantum
            .instances
            .iter()
            .map(|instance| instance.id)
            .collect(),
        detectors: (0..quantum.detectors.len() as u32).collect(),
        detector_bundles: (0..quantum.detector_bundles.len() as u32).collect(),
        restarts: (0..quantum.restarts.len() as u32).collect(),
        ..Default::default()
    });
    node
}
