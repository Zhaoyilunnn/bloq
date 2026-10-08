#![cfg(test)]

use std::time::Duration;

use bloq_graph::verify::{BranchAssignment, BranchStatus, LogicalVerifier, QuizxGraph};
use bloq_graph::{Action, BlockKind, MeasureTarget};
use bloq_lassynth::{
    CliffordTError, ComponentOptions, Port, SynthesisError, parse_component, synthesize_qasm,
};
use bloq_utils::{Direction, UDirection};
use glam::IVec3;
use quizx::circuit::CircuitStats;
use quizx::graph::{BasisElem, GraphLike};

fn qasm(qubits: usize, body: &str) -> String {
    format!("OPENQASM 2.0; include \"qelib1.inc\"; qreg q[{qubits}]; {body}")
}

fn temporal(position: IVec3, direction: Direction) -> Port {
    Port::new(position, direction, UDirection::Y)
}

fn verify(graph: &bloq_graph::BlockGraph, source: &str) {
    graph.validate().expect("synthesis emits a valid graph");
    let parsed = parse_component(source).unwrap();
    let circuit = parsed.circuit;
    let mut expected: QuizxGraph = circuit.to_graph();
    let boundaries = |fixed: &[bool]| {
        fixed
            .iter()
            .map(|fixed| {
                if *fixed {
                    BasisElem::Z0
                } else {
                    BasisElem::SKIP
                }
            })
            .collect::<Vec<_>>()
    };
    expected.plug_inputs(&boundaries(&parsed.reset_inputs));
    expected.plug_outputs(&boundaries(&parsed.measured_outputs));
    let verifier = if CircuitStats::make(&circuit).non_cliff == 0 {
        LogicalVerifier::with_internal_measurements(graph)
    } else {
        LogicalVerifier::new(graph)
    }
    .unwrap();
    let names = graph
        .actions()
        .into_iter()
        .filter_map(|action| match action {
            Action::Measure { name, .. } => Some(name),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut verified = 0;
    for bits in 0..1 << names.len() {
        let assignment = names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), bits & (1 << index) != 0))
            .collect::<BranchAssignment>();
        if verifier
            .verify_branch(&expected, &assignment)
            .unwrap()
            .status()
            == BranchStatus::Verified
        {
            verified += 1;
        }
    }
    assert!(verified > 0, "{source} has a nonzero verified branch");
}

#[test]
fn signed_clifford_and_t_compositions_verify_every_parity_branch() {
    let options = ComponentOptions::new(
        IVec3::new(2, 2, 4),
        [temporal(IVec3::new(1, 0, -1), Direction::ZPLUS)],
        [temporal(IVec3::new(1, 0, 4), Direction::ZMINUS)],
        Duration::from_secs(10),
    );
    for body in [
        "h q; s q;",
        "h q; sdg q;",
        "tdg q;",
        "s q; t q;",
        "sdg q; t q;",
        "x q; tdg q;",
        "tdg q; y q;",
    ] {
        let source = qasm(1, body);
        let graph =
            synthesize_qasm(&source, &options).unwrap_or_else(|error| panic!("{body}: {error}"));
        verify(&graph, &source);
    }
}

#[test]
fn inverse_t_has_verified_branches_for_both_parity_outcomes() {
    let options = ComponentOptions::new(
        IVec3::new(2, 2, 3),
        [temporal(IVec3::new(0, 0, -1), Direction::ZPLUS)],
        [Port::new(
            IVec3::new(0, 1, 3),
            Direction::ZMINUS,
            UDirection::X,
        )],
        Duration::from_secs(10),
    );
    for body in ["tdg q;", "x q; tdg q;"] {
        let source = qasm(1, body);
        let graph = synthesize_qasm(&source, &options).unwrap();
        let expected = parse_component(&source).unwrap().circuit.to_graph();
        let verifier = LogicalVerifier::new(&graph).unwrap();
        for parity in [false, true] {
            let assignment = BranchAssignment::from([("t_0_mzz".into(), parity)]);
            assert_eq!(
                verifier
                    .verify_branch(&expected, &assignment)
                    .unwrap()
                    .status(),
                BranchStatus::Verified,
                "{source}: parity {parity} must be a nonzero branch",
            );
        }
    }
}

#[test]
fn zero_boundary_maps_are_rejected_with_or_without_surviving_ports() {
    for qubits in [1, 2] {
        let source = qasm(
            qubits,
            "creg c[1]; reset q[0]; x q[0]; measure q[0] -> c[0];",
        );
        let options = ComponentOptions::new(
            IVec3::new(2, 2, 3),
            (qubits == 2).then(|| temporal(IVec3::new(1, 0, -1), Direction::ZPLUS)),
            (qubits == 2).then(|| temporal(IVec3::new(1, 0, 3), Direction::ZMINUS)),
            Duration::from_secs(10),
        );
        assert!(matches!(
            synthesize_qasm(&source, &options),
            Err(CliffordTError::ZeroMap)
        ));
    }
}

#[test]
fn qasm_keywords_accept_all_whitespace_separators() {
    for separator in [" ", "\t", "\n", "\r\n"] {
        let source = format!(
            "OPENQASM{separator}2.0; include{separator}\"qelib1.inc\"; \
             qreg{separator}q[1]; creg{separator}c[1]; reset{separator}q; \
             barrier{separator}q; id{separator}q; y{separator}q; \
             measure{separator}q -> c;"
        );
        let parsed = parse_component(&source).unwrap();
        assert_eq!(parsed.reset_inputs, [true]);
        assert_eq!(parsed.measured_outputs, [true]);
        assert_eq!(parsed.circuit.num_qubits(), 1);
    }
}

#[test]
fn whole_component_cnot_uses_configured_ports() {
    let source = qasm(2, "cx q[0],q[1];");
    let options = ComponentOptions::new(
        IVec3::new(2, 2, 3),
        [
            temporal(IVec3::new(1, 0, -1), Direction::ZPLUS),
            temporal(IVec3::new(0, 1, -1), Direction::ZPLUS),
        ],
        [
            temporal(IVec3::new(1, 0, 3), Direction::ZMINUS),
            temporal(IVec3::new(0, 1, 3), Direction::ZMINUS),
        ],
        Duration::from_secs(10),
    );

    let graph = synthesize_qasm(&source, &options).expect("CNOT fits its fixed box");

    assert_eq!(graph.port_count(), 4);
    assert_eq!(
        graph.get_block(IVec3::new(1, 0, -1)).unwrap().tag(),
        Some("input_0")
    );
    verify(&graph, &source);
}

#[test]
fn terminal_measurement_closes_the_output() {
    let source = qasm(
        1,
        "creg c[1]; h q[0]; t q[0]; h q[0]; x q[0]; t q[0]; measure q[0] -> c[0];",
    );
    let options = ComponentOptions::new(
        IVec3::new(3, 3, 5),
        [temporal(IVec3::new(1, 0, -1), Direction::ZPLUS)],
        [],
        Duration::from_secs(10),
    );

    let graph = synthesize_qasm(&source, &options).expect("measured T component fits");

    assert_eq!(graph.port_count(), 1);
    assert_eq!(
        graph
            .blocks()
            .filter(|block| block.kind() == BlockKind::T)
            .count(),
        1
    );
    assert!(
        graph
            .actions()
            .iter()
            .any(|action| matches!(action, Action::Feedback { .. }))
    );
    verify(&graph, &source);
}

#[test]
fn one_t_becomes_magic_input_and_selective_measurement() {
    let source = qasm(1, "t q[0];");
    let options = ComponentOptions::new(
        IVec3::new(2, 2, 3),
        [temporal(IVec3::new(1, 0, -1), Direction::ZPLUS)],
        [temporal(IVec3::new(1, 0, 3), Direction::ZMINUS)],
        Duration::from_secs(10),
    );

    let graph = synthesize_qasm(&source, &options).expect("T fits its fixed box");

    assert_eq!(
        graph
            .blocks()
            .filter(|block| block.kind() == BlockKind::T)
            .count(),
        1
    );
    assert_eq!(graph.actions().len(), 2);
    verify(&graph, &source);
}

#[test]
fn hadamard_t_cnot_component_verifies() {
    let source = qasm(2, "h q[0]; t q[0]; cx q[0],q[1];");
    let options = ComponentOptions::new(
        IVec3::new(2, 3, 5),
        [
            temporal(IVec3::new(1, 0, -1), Direction::ZPLUS),
            temporal(IVec3::new(0, 1, -1), Direction::ZPLUS),
        ],
        [
            temporal(IVec3::new(1, 0, 5), Direction::ZMINUS),
            temporal(IVec3::new(0, 1, 5), Direction::ZMINUS),
        ],
        Duration::from_secs(10),
    );

    let graph = synthesize_qasm(&source, &options).expect("T + CNOT fits");
    verify(&graph, &source);
}

#[test]
fn small_entangling_clifford_t_component_verifies() {
    let source = qasm(2, "h q[0]; t q[0]; cx q[0],q[1]; tdg q[1];");
    let options = ComponentOptions::new(
        IVec3::new(3, 3, 5),
        [
            temporal(IVec3::new(1, 0, -1), Direction::ZPLUS),
            temporal(IVec3::new(0, 1, -1), Direction::ZPLUS),
        ],
        [
            temporal(IVec3::new(1, 0, 5), Direction::ZMINUS),
            temporal(IVec3::new(0, 1, 5), Direction::ZMINUS),
        ],
        Duration::from_secs(10),
    );

    let graph = synthesize_qasm(&source, &options).expect("small Clifford+T map fits");

    let owners = graph
        .actions()
        .into_iter()
        .filter_map(|action| match action {
            Action::Measure { target, .. } => Some(target),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(owners.len(), 2);
    assert_ne!(owners[0], owners[1]);
    assert!(owners.iter().all(|target| matches!(
        target,
        MeasureTarget::Edge { dir, .. } if dir.as_udirection().is_spatial()
    )));
    verify(&graph, &source);
}

#[test]
fn reset_closes_the_logical_input() {
    let source = qasm(1, "reset q; h q[0];");
    let options = ComponentOptions::new(
        IVec3::new(2, 2, 3),
        [],
        [temporal(IVec3::new(1, 0, 3), Direction::ZMINUS)],
        Duration::from_secs(10),
    );

    let graph = synthesize_qasm(&source, &options).expect("reset state preparation fits");

    assert_eq!(graph.port_count(), 1);
    verify(&graph, &source);
}

#[test]
fn y_preparation_and_measurement_keep_opposite_spider_phase_conventions() {
    for (body, state) in [
        ("reset q; h q; s q;", true),
        ("reset q; h q; sdg q;", true),
        ("creg m[1]; sdg q; h q; measure q -> m;", false),
        ("creg m[1]; s q; h q; measure q -> m;", false),
    ] {
        let source = qasm(1, body);
        let options = ComponentOptions::new(
            IVec3::new(2, 2, 3),
            (!state).then(|| temporal(IVec3::new(1, 0, -1), Direction::ZPLUS)),
            state.then(|| temporal(IVec3::new(1, 0, 3), Direction::ZMINUS)),
            Duration::from_secs(10),
        );
        let graph =
            synthesize_qasm(&source, &options).unwrap_or_else(|error| panic!("{body}: {error}"));
        verify(&graph, &source);
    }
}

#[test]
fn overflowing_component_port_is_reported() {
    let source = qasm(1, "");
    let options = ComponentOptions::new(
        IVec3::ONE,
        [Port::new(
            IVec3::new(i32::MIN, 0, 0),
            Direction::XMINUS,
            UDirection::Y,
        )],
        [temporal(IVec3::Z, Direction::ZMINUS)],
        Duration::from_secs(10),
    );

    assert!(matches!(
        synthesize_qasm(&source, &options),
        Err(CliffordTError::Synthesis(SynthesisError::InvalidPort {
            index: 0,
            ..
        }))
    ));
}

#[test]
fn invalid_component_volume_is_reported_before_port_allocation() {
    let source = qasm(1, "t q[0];");
    let options = ComponentOptions::new(
        IVec3::new(1, 1, i32::MIN),
        [temporal(IVec3::NEG_Z, Direction::ZPLUS)],
        [temporal(IVec3::new(0, 0, i32::MIN), Direction::ZMINUS)],
        Duration::from_secs(10),
    );

    assert!(matches!(
        synthesize_qasm(&source, &options),
        Err(CliffordTError::Synthesis(SynthesisError::InvalidSize(_)))
    ));
}

#[test]
fn thin_t_box_reports_absent_spatial_measurement_owners() {
    let size = IVec3::new(1, 1, 6);
    let options = ComponentOptions::new(
        size,
        [Port::new(
            IVec3::new(-1, 0, 0),
            Direction::XPLUS,
            UDirection::Y,
        )],
        [Port::new(
            IVec3::new(1, 0, 5),
            Direction::XMINUS,
            UDirection::Y,
        )],
        Duration::from_secs(2),
    );
    assert!(matches!(
        synthesize_qasm(&qasm(1, "t q[0];"), &options),
        Err(CliffordTError::Synthesis(SynthesisError::NoMeasurementAnchor {
            row: 0,
            size: reported,
        })) if reported == size
    ));
}
