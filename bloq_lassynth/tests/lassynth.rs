use std::time::Duration;

use bloq_lassynth::{Port, SynthesisError, SynthesisProblem, synthesize, synthesize_with_timeout};
use bloq_utils::{Direction, PauliString, PortRole, UDirection};
use glam::IVec3;

fn paulis(value: &str) -> PauliString {
    PauliString::try_from(value).expect("test stabilizer is valid")
}

#[test]
fn synthesizes_upstream_cnot_into_a_valid_block_graph() {
    let ports = [
        Port::new(IVec3::new(1, 0, -1), Direction::ZPLUS, UDirection::Y).with_tag("control_in"),
        Port::new(IVec3::new(0, 1, -1), Direction::ZPLUS, UDirection::Y).with_tag("target_in"),
        Port::new(IVec3::new(1, 0, 3), Direction::ZMINUS, UDirection::Y).with_tag("control_out"),
        Port::new(IVec3::new(0, 1, 3), Direction::ZMINUS, UDirection::Y).with_tag("target_out"),
    ];
    let problem = SynthesisProblem::new(
        IVec3::new(2, 2, 3),
        ports,
        [
            paulis("ZIZI"),
            paulis("IZZZ"),
            paulis("XIXX"),
            paulis("IXIX"),
        ],
    );

    let graph = synthesize(&problem).expect("the canonical LaSsynth CNOT is satisfiable");

    graph.validate().expect("LaSsynth emits a valid graph");
    assert_eq!(
        graph
            .blocks()
            .filter(|block| block.kind().is_port())
            .count(),
        4
    );
    assert!(graph.pipe_count() >= 4);
    assert_eq!(
        graph.get_block(IVec3::new(1, 0, -1)).unwrap().tag(),
        Some("control_in")
    );
}

#[test]
fn exterior_ports_preserve_the_paper_uma_box() {
    let output =
        |position, basis| Port::new(position, Direction::XMINUS, basis).with_role(PortRole::Output);
    let problem = SynthesisProblem::new(
        IVec3::new(3, 3, 2),
        [
            Port::new(IVec3::new(1, 0, -1), Direction::ZPLUS, UDirection::X),
            Port::new(IVec3::new(1, 2, -1), Direction::ZPLUS, UDirection::X),
            Port::new(IVec3::new(2, 0, -1), Direction::ZPLUS, UDirection::X),
            Port::new(IVec3::new(2, 2, -1), Direction::ZPLUS, UDirection::Y),
            output(IVec3::new(3, 0, 1), UDirection::Z),
            output(IVec3::new(3, 1, 0), UDirection::Y),
            output(IVec3::new(3, 1, 1), UDirection::Z),
        ],
        [
            "X___X__", "_X_____", "__XZX_Z", "__ZXXZ_", "___Z_XZ", "__Z__ZX", "Z_ZZZ__",
        ]
        .map(paulis),
    );

    let graph = synthesize(&problem).expect("the exact 3 x 3 x 2 UMA interior is satisfiable");

    let in_box =
        |position: IVec3| position.cmpge(IVec3::ZERO).all() && position.cmplt(problem.size()).all();
    assert!(
        graph
            .blocks()
            .all(|block| block.kind().is_port() != in_box(block.pos()))
    );
}

#[test]
fn a_volume_too_shallow_for_the_cnot_is_unsatisfiable() {
    let problem = SynthesisProblem::new(
        IVec3::new(2, 2, 1),
        [
            Port::new(IVec3::new(1, 0, -1), Direction::ZPLUS, UDirection::Y),
            Port::new(IVec3::new(0, 1, -1), Direction::ZPLUS, UDirection::Y),
            Port::new(IVec3::new(1, 0, 1), Direction::ZMINUS, UDirection::Y),
            Port::new(IVec3::new(0, 1, 1), Direction::ZMINUS, UDirection::Y),
        ],
        [
            paulis("ZIZI"),
            paulis("IZZZ"),
            paulis("XIXX"),
            paulis("IXIX"),
        ],
    );

    assert!(matches!(
        synthesize(&problem),
        Err(SynthesisError::Unsatisfiable)
    ));
}

#[test]
fn spatial_hadamard_is_opt_in() {
    let problem = SynthesisProblem::new(
        IVec3::new(2, 1, 1),
        [
            Port::new(IVec3::NEG_X, Direction::XPLUS, UDirection::Y).with_role(PortRole::Input),
            Port::new(IVec3::new(2, 0, 0), Direction::XMINUS, UDirection::Z)
                .with_role(PortRole::Output),
        ],
        [paulis("XZ"), paulis("ZX")],
    );
    assert!(matches!(
        synthesize(&problem),
        Err(SynthesisError::Unsatisfiable)
    ));

    let graph = synthesize(&problem.with_spatial_hadamard(true))
        .expect("one spatial Hadamard bridges the rotated frames");
    assert!(
        graph
            .pipes()
            .any(|pipe| pipe.dir().as_udirection().is_spatial() && pipe.is_hadamard())
    );
    assert_eq!(
        graph.get_block(IVec3::NEG_X).unwrap().port_role(),
        Some(PortRole::Input)
    );
}

#[test]
fn spatial_synthesis_ports_require_roles() {
    let problem = SynthesisProblem::new(
        IVec3::new(2, 1, 1),
        [Port::new(IVec3::NEG_X, Direction::XPLUS, UDirection::Y)],
        [],
    );
    assert!(matches!(
        synthesize(&problem),
        Err(SynthesisError::InvalidPort { .. })
    ));
}

#[test]
fn invalid_coordinates_and_volume_are_reported() {
    let oversized = SynthesisProblem::new(IVec3::new(i32::MAX, 2, 1), [], []);
    assert!(matches!(
        synthesize(&oversized),
        Err(SynthesisError::InvalidSize(_))
    ));

    let overflowing_port = SynthesisProblem::new(
        IVec3::ONE,
        [Port::new(
            IVec3::new(i32::MIN, 0, 0),
            Direction::XMINUS,
            UDirection::Y,
        )],
        [],
    );
    assert!(matches!(
        synthesize(&overflowing_port),
        Err(SynthesisError::InvalidPort { .. })
    ));

    let interior_port = SynthesisProblem::new(
        IVec3::new(1, 1, 3),
        [Port::new(
            IVec3::new(0, 0, 1),
            Direction::ZPLUS,
            UDirection::Y,
        )],
        [],
    );
    assert!(matches!(
        synthesize(&interior_port),
        Err(SynthesisError::InvalidPort { .. })
    ));
}

#[test]
fn wall_clock_budget_is_enforced() {
    let problem = SynthesisProblem::new(IVec3::ONE, [], []);
    assert!(matches!(
        synthesize_with_timeout(&problem, Duration::ZERO),
        Err(SynthesisError::TimedOut)
    ));
}
