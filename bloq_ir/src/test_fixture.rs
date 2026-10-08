//! A hand-built "kitchen sink" program for exchange-format tests: every node
//! kind, edge kind, provenance channel, side table, and circuit op, plus a
//! node-id hole — everything a codec must round-trip. Not a *valid* program
//! (validation would reject the arbitrary wiring); codecs restore structure,
//! they do not validate.

use glam::{ivec2, ivec3};
use petgraph::stable_graph::NodeIndex;

use bloq_circuit::{
    Basis, BodyId, CircuitBody, ConditionalCorrection, CoordCircuit, DetectorParity, DetectorTerm,
    Flow, FlowMarker, GateType, LoopStateId, Op, Pauli, PauliBasis, PauliMap,
};

use crate::{
    Bloq, BloqEdge, BloqNode, BloqNodeId, BloqTemplate, BoundaryFace, ClassicalExpr, ClassicalNode,
    InstanceBoundaryOperator, InstanceMeasurement, NodeDetector, NodeProvenance, NodeRestart,
    ObservableOutput, QuantumNode, RegionNode, SourceBlockRef, SubGraph, TemplateDetector,
    TemplateDetectorScope, TemplateId, TemplateInstance, TemplateInstanceId, TemplateRepeatState,
    TemplateRestart, TemporalPipeRef, ValueRef, ValueRole,
};

fn pauli_map(entries: &[(i32, i32, Pauli)]) -> PauliMap {
    entries
        .iter()
        .map(|&(x, y, pauli)| (ivec2(x, y), pauli))
        .collect()
}

fn sample_template() -> BloqTemplate {
    let mut circuit = CoordCircuit::new();
    circuit
        .do_gate(GateType::H, [ivec2(0, 0), ivec2(1, 0)])
        .expect("valid single-qubit targets");
    circuit.tick();
    circuit.measure(PauliBasis::Z, [ivec2(0, 0), ivec2(1, 0)]); // m0, m1
    circuit
        .measure_pauli_products([pauli_map(&[(0, 0, Pauli::X), (1, 0, Pauli::X)])])
        .expect("non-empty product"); // m2
    let body = circuit.add_body(CircuitBody::from_ops(vec![
        Op::Measure {
            basis: PauliBasis::X,
            qubits: vec![ivec2(0, 0)],
            measurements: vec![3],
            flip_probability: 0.125,
        },
        Op::Gate {
            gate: GateType::CX,
            qubits: vec![ivec2(0, 0), ivec2(1, 0)],
        },
        Op::Depolarize1 {
            probability: 0.01,
            qubits: vec![ivec2(0, 0)],
        },
        Op::Depolarize2 {
            probability: 0.02,
            qubits: vec![ivec2(0, 0), ivec2(1, 0)],
        },
        Op::PauliError {
            probability: 0.03,
            pauli: PauliBasis::Z,
            qubits: vec![ivec2(1, 0)],
        },
    ]));
    assert_eq!(body, BodyId(1));
    circuit
        .body_mut(circuit.entry_body())
        .expect("entry body exists")
        .ops_mut()
        .extend([
            Op::Repeat {
                body,
                repetitions: 5,
            },
            Op::ConditionalPauli(vec![
                ConditionalCorrection {
                    pauli: PauliBasis::X,
                    control: 0,
                    target: ivec2(1, 0),
                },
                ConditionalCorrection {
                    pauli: PauliBasis::Z,
                    control: 1,
                    target: ivec2(0, 0),
                },
            ]),
        ]);
    // The repeat body's m3 is implied by its op; add a registry record no op
    // implies, to exercise the explicit `meas` line.
    circuit.register_measurement_id(3, ivec2(0, 0));
    circuit.register_measurement_id(9, ivec2(4, 4));

    BloqTemplate::with_parts(
        circuit,
        vec![
            TemplateDetector {
                scope: TemplateDetectorScope::TopLevel,
                parity: DetectorParity::from_measurements([0, 1]),
                coords: Some([0.5, 1.0].into_iter().collect()),
            },
            TemplateDetector {
                scope: TemplateDetectorScope::RepeatBody { body: BodyId(1) },
                parity: DetectorParity::from_terms([
                    DetectorTerm::Measurement(3),
                    DetectorTerm::LoopState(LoopStateId(0)),
                ]),
                coords: None,
            },
        ],
        vec![TemplateRepeatState {
            body: BodyId(1),
            state: LoopStateId(0),
            initial: DetectorParity::from_measurements([0]),
            next: DetectorParity::from_measurements([3]),
        }],
        vec![
            Flow::new(
                pauli_map(&[(0, 0, Pauli::X)]),
                pauli_map(&[(0, 0, Pauli::Z), (1, 0, Pauli::Y)]),
            )
            .with_measurements([0, 2])
            .with_center(ivec2(1, 1)),
            Flow::new(PauliMap::empty(), pauli_map(&[(1, 0, Pauli::X)]))
                .with_marker(FlowMarker::Restart),
        ],
        vec![TemplateRestart {
            parity: DetectorParity::from_measurements([2, 3]),
        }],
    )
}

fn instance(id: u32, template: TemplateId, x: i32, y: i32) -> TemplateInstance {
    TemplateInstance::new(TemplateInstanceId(id), template, ivec2(x, y))
}

pub(crate) fn instance_measurement(instance: u32, measurement: u32) -> InstanceMeasurement {
    InstanceMeasurement {
        instance: TemplateInstanceId(instance),
        measurement,
    }
}

fn sample_quantum_node(template: TemplateId) -> BloqNode {
    let quantum = QuantumNode {
        instances: vec![instance(0, template, 0, 0), instance(1, template, 8, 0)],
        detectors: vec![NodeDetector {
            parity: DetectorParity::from_measurements([
                instance_measurement(0, 0),
                instance_measurement(1, 1),
            ]),
            coords: Some([2.0, 3.5].into_iter().collect()),
        }],
        detector_bundles: Vec::new(),
        restarts: vec![NodeRestart {
            parity: DetectorParity::from_measurements([
                instance_measurement(0, 2),
                instance_measurement(1, 2),
            ]),
        }],
        timeline: None,
        guards: Vec::new(),
    };
    BloqNode::quantum(quantum).with_provenance(NodeProvenance::BlockComponent {
        members: vec![
            SourceBlockRef {
                pos: ivec3(0, 0, 0),
            },
            SourceBlockRef {
                pos: ivec3(1, 0, 0),
            },
        ],
    })
}

fn pipe(sz: i32, dz: i32, hadamard: bool) -> TemporalPipeRef {
    TemporalPipeRef {
        src: ivec3(0, 0, sz),
        dst: ivec3(0, 0, dz),
        hadamard,
    }
}

fn sample_region_body(template: TemplateId) -> SubGraph {
    let mut body = SubGraph::new();
    let quantum = body.add_node(BloqNode::quantum(QuantumNode {
        instances: vec![instance(2, template, 0, 8)],
        ..QuantumNode::default()
    }));
    let acc = body.add_node(BloqNode::classical(ClassicalNode::Observable {
        index: Some(8),
        operators: vec![],
        measurements: vec![instance_measurement(2, 0)],
    }));
    body.add_edge(quantum, acc, BloqEdge::Order);
    body.set_value_output(Some(ValueRef {
        node: acc,
        output: ObservableOutput::Flip,
    }));
    body
}

/// See the module doc. Node ids: `n1` is a hole (added then removed); the
/// rest are live.
pub(crate) fn sample_bloq() -> Bloq {
    let mut bloq = Bloq::new();
    let template = bloq.add_template(sample_template());

    let quantum = bloq.add_node(sample_quantum_node(template));
    let doomed = bloq.add_node(BloqNode::classical(ClassicalNode::Compute {
        expr: ClassicalExpr::Const(false),
    }));
    let acc = bloq.add_node(
        BloqNode::classical(ClassicalNode::Observable {
            index: Some(8),
            operators: vec![],
            measurements: vec![instance_measurement(0, 0), instance_measurement(1, 2)],
        })
        .with_provenance(NodeProvenance::Generator { ordinal: 0 }),
    );
    let compute = bloq.add_node(
        BloqNode::classical(ClassicalNode::Compute {
            // Exercises precedence, a right-nested subtree, and constants.
            expr: ClassicalExpr::Or(Box::new([
                ClassicalExpr::Xor(Box::new([
                    ClassicalExpr::In(0),
                    ClassicalExpr::Xor(Box::new([
                        ClassicalExpr::In(1),
                        ClassicalExpr::Const(true),
                    ])),
                ])),
                ClassicalExpr::Not(Box::new(ClassicalExpr::And(Box::new([
                    ClassicalExpr::In(2),
                    ClassicalExpr::Const(false),
                ])))),
            ])),
        })
        .with_provenance(NodeProvenance::OutputFrame {
            port: ivec3(0, 0, 1),
            basis: Basis::X,
        }),
    );
    let observable = bloq.add_node(BloqNode::classical(ClassicalNode::Observable {
        index: Some(7),
        measurements: vec![instance_measurement(0, 0), instance_measurement(1, 2)],
        operators: vec![
            InstanceBoundaryOperator {
                instance: TemplateInstanceId(0),
                face: BoundaryFace::Input,
                operator: pauli_map(&[(8, 1, Pauli::Z)]),
            },
            InstanceBoundaryOperator {
                instance: TemplateInstanceId(1),
                face: BoundaryFace::Output,
                operator: pauli_map(&[(8, 0, Pauli::X)]),
            },
        ],
    }));
    let corrected = bloq.add_node(BloqNode::classical(ClassicalNode::Compute {
        expr: ClassicalExpr::In(0),
    }));
    let flip = bloq.add_node(BloqNode::classical(ClassicalNode::Compute {
        expr: ClassicalExpr::In(0),
    }));
    // Two operators on one fragment so text round-trip covers the
    // comma-separated multi-operator form.
    let boundary = bloq.add_node(BloqNode::classical(ClassicalNode::Observable {
        index: None,
        measurements: vec![],
        operators: vec![
            InstanceBoundaryOperator {
                instance: TemplateInstanceId(1),
                face: BoundaryFace::Output,
                operator: pauli_map(&[(8, 0, Pauli::X), (9, 0, Pauli::Z)]),
            },
            InstanceBoundaryOperator {
                instance: TemplateInstanceId(0),
                face: BoundaryFace::Input,
                operator: pauli_map(&[(8, 1, Pauli::Z)]),
            },
        ],
    }));
    let discard = bloq.add_node(
        BloqNode::classical(ClassicalNode::Discard {
            condition: ClassicalExpr::In(0),
        })
        .with_provenance(NodeProvenance::Action { ordinal: 3 }),
    );
    let retry = bloq.add_node(BloqNode::region(RegionNode::RepeatUntilSuccess {
        restart_source: None,
        restart_condition: ClassicalExpr::In(0),
        body: sample_region_body(template),
    }));
    let selection = bloq.add_node(BloqNode::quantum(QuantumNode {
        instances: vec![instance(3, template, 0, 12)],
        guards: vec![crate::QuantumGuard {
            input: 0,
            instances: vec![TemplateInstanceId(3)],
            ..Default::default()
        }],
        ..Default::default()
    }));
    let rus = bloq.add_node(BloqNode::region(RegionNode::RepeatUntilSuccess {
        body: sample_region_body(template),
        restart_condition: ClassicalExpr::In(0),
        restart_source: Some(ValueRef {
            node: BloqNodeId(1),
            output: ObservableOutput::Flip,
        }),
    }));
    let piped = bloq.add_node(BloqNode::from_temporal_pipe(pipe(0, 1, true)));
    let padded = bloq.add_node(BloqNode::memory_padding(pipe(1, 2, false), 4));
    let more_padding = bloq.add_node(BloqNode::memory_padding(pipe(2, 3, false), 2));
    let product = bloq.add_node(BloqNode::classical(ClassicalNode::Observable {
        index: Some(8),
        operators: vec![],
        measurements: vec![instance_measurement(0, 1), instance_measurement(1, 0)],
    }));

    let hadamard_pipe = pipe(0, 1, true);
    bloq.add_edge(
        quantum,
        piped,
        BloqEdge::Quantum(Box::new(crate::QuantumEdge {
            guard: None,
            pipes: vec![crate::PipeSeam {
                pipe: hadamard_pipe,
                padding: Some(crate::PipePadding {
                    offset: ivec2(-8, 4),
                    one_round: TemplateId(0),
                    looped: TemplateId(0),
                }),
            }],
        })),
    );
    bloq.add_edge(
        piped,
        padded,
        BloqEdge::quantum(vec![pipe(1, 2, false), pipe(2, 3, true)]),
    );
    bloq.add_edge(
        padded,
        more_padding,
        BloqEdge::quantum(vec![pipe(2, 3, false)]),
    );
    bloq.add_edge(quantum, acc, BloqEdge::Order);
    bloq.add_edge(acc, compute, BloqEdge::value(0));
    bloq.add_edge(compute, observable, BloqEdge::value(0));
    bloq.add_edge(observable, corrected, BloqEdge::value(0));
    bloq.add_edge(
        observable,
        flip,
        BloqEdge::Value {
            slot: 0,
            role: ValueRole::Data,
            output: ObservableOutput::Flip,
        },
    );
    bloq.add_edge(
        boundary,
        observable,
        BloqEdge::Compose {
            slot: 1,
            role: ValueRole::Data,
        },
    );
    bloq.add_edge(
        compute,
        discard,
        BloqEdge::Value {
            slot: 0,
            role: ValueRole::FeedbackFold { action: 3 },
            output: ObservableOutput::Corrected,
        },
    );
    bloq.add_edge(compute, retry, BloqEdge::value(0));
    bloq.add_edge(compute, selection, BloqEdge::value(0));
    bloq.add_edge(compute, rus, BloqEdge::value(0));
    bloq.add_edge(product, compute, BloqEdge::value(1));

    // Punch a hole: remove `doomed` so ids are sparse (SEM-ID).
    bloq.graph_mut_internal()
        .remove_node(NodeIndex::new(doomed.0 as usize))
        .expect("doomed node exists");

    bloq.set_logical_inputs(vec![crate::LogicalInput {
        port: ivec3(0, 0, -1),
        instance: crate::lowering::TemplateInstanceId(0),
        x: pauli_map(&[(1, 1, Pauli::X)]),
        z: pauli_map(&[(1, 1, Pauli::Z)]),
    }]);
    bloq.set_logical_outputs(vec![crate::LogicalOutput {
        port: ivec3(0, 0, 1),
        instance: crate::lowering::TemplateInstanceId(0),
        x: pauli_map(&[(1, 1, Pauli::X)]),
        z: pauli_map(&[(1, 1, Pauli::Z)]),
    }]);
    bloq
}
