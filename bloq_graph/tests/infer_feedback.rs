#![cfg(test)]
#![cfg(feature = "verify")]

use bloq_graph::verify::BoundaryOrder;
use bloq_graph::{
    Action, BinaryOp, Expr, FeedbackInferenceError, FeedbackOptions, GalleryItem, PauliBasis,
    infer_feedback, infer_feedback_with,
};
use glam::IVec3;

fn without_feedback(gallery: GalleryItem) -> bloq_graph::BlockGraph {
    let mut graph = gallery
        .build()
        .materialize_root_graph()
        .expect("gallery flat projection");
    graph
        .set_actions(
            graph
                .actions()
                .into_iter()
                .filter(|action| !matches!(action, Action::Feedback { .. }))
                .collect(),
        )
        .unwrap();
    graph
}

#[test]
fn ccz_injected_and_recovers_the_product_condition() {
    let mut graph = without_feedback(GalleryItem::CCZInjectedAnd);
    let source = "OPENQASM 2.0; qreg q[3]; reset q[2]; ccx q[0],q[1],q[2];";
    let preparation = "OPENQASM 2.0; qreg q[5];
        reset q[0]; reset q[1]; reset q[2];
        h q[0]; h q[1]; h q[2]; ccz q[0],q[1],q[2];";
    let output = IVec3::new(0, 1, 2);
    let options = FeedbackOptions::default()
        .with_boundaries(BoundaryOrder::new(
            vec![
                IVec3::new(-1, 2, 0),
                IVec3::new(-1, 1, 0),
                IVec3::new(-1, 0, 0),
                IVec3::new(5, 2, 0),
                IVec3::new(5, 1, 0),
            ],
            vec![IVec3::new(5, 2, 0), IVec3::new(5, 1, 0), output],
        ))
        .with_input_preparation(preparation);
    let actions = infer_feedback_with(source, &graph, &options).unwrap();
    assert!(
        matches!(actions.as_slice(), [Action::Feedback { targets, condition: Some(Expr::Binary(BinaryOp::And, left, right)) }]
        if targets.len() == 1 && targets[0].target == output && targets[0].pauli == PauliBasis::X
            && left.as_ref() == &Expr::Var("m1".into()) && right.as_ref() == &Expr::Var("m2".into()))
    );
    let mut all = graph.actions();
    all.extend(actions);
    graph.set_actions(all).unwrap();
    assert!(
        infer_feedback_with(source, &graph, &options)
            .unwrap()
            .is_empty()
    );
    let different_channel = source.replace("ccx q[0],q[1],q[2];", "cx q[0],q[2];");
    assert!(matches!(
        infer_feedback_with(&different_channel, &graph, &options.with_search_limit(0)),
        Err(FeedbackInferenceError::NoOutputPauli { .. })
    ));
}

#[test]
fn prepared_y_constants_and_internal_feedback_before_a_later_t() {
    for (gallery, body) in [
        (GalleryItem::TWithPreparedY, "t q;"),
        (GalleryItem::THTH, "t q; h q; t q; h q;"),
    ] {
        let source = format!("OPENQASM 2.0; qreg q[1]; {body}");
        assert!(
            infer_feedback_with(
                &source,
                &gallery
                    .build()
                    .materialize_root_graph()
                    .expect("gallery flat projection"),
                &FeedbackOptions::default().with_search_limit(0)
            )
            .unwrap_or_else(|error| panic!("{gallery:?}: {error:?}"))
            .is_empty(),
            "{gallery:?}"
        );
    }
    let graph = without_feedback(GalleryItem::THTH);
    let source = "OPENQASM 2.0; qreg q[1]; t q; h q; t q; h q;";
    assert!(matches!(
        infer_feedback_with(
            source,
            &graph,
            &FeedbackOptions::default().with_search_limit(0)
        ),
        Err(FeedbackInferenceError::NoOutputPauli { .. })
    ));
    let actions = infer_feedback(source, &graph).unwrap();
    assert!(actions.iter().any(|action| matches!(action, Action::Feedback { targets, .. } if targets.iter().any(|target| target.target != IVec3::new(-1,2,2)))));
    let mut candidate = graph;
    let mut combined = candidate.actions();
    combined.extend(actions);
    candidate.set_actions(combined).unwrap();
    assert!(infer_feedback(source, &candidate).unwrap().is_empty());
}

#[test]
fn measurement_controlled_internal_pauli_is_not_pushed_through_t() {
    let source = "OPENQASM 2.0; qreg q[1]; qreg coin[1]; creg mzz1[1];
        reset coin; h coin; measure coin -> mzz1;
        t q; h q; if(mzz1==1) x q; t q; h q;";
    let mut graph = GalleryItem::THTH
        .build()
        .materialize_root_graph()
        .expect("gallery flat projection");
    assert!(matches!(
        infer_feedback_with(
            source,
            &graph,
            &FeedbackOptions::default().with_search_limit(0)
        ),
        Err(FeedbackInferenceError::NoOutputPauli { .. })
    ));
    let actions = infer_feedback_with(
        source,
        &graph,
        &FeedbackOptions::default()
            .with_internal_sites(vec![IVec3::new(-1, 0, 1)])
            .with_search_limit(24),
    )
    .unwrap();
    assert!(actions.iter().any(|action| matches!(action, Action::Feedback { targets, condition: Some(_) } if targets.iter().any(|target| target.target != IVec3::new(-1,2,2)))));
    let mut combined = graph.actions();
    combined.extend(actions);
    graph.set_actions(combined).unwrap();
    assert!(infer_feedback(source, &graph).unwrap().is_empty());
}

#[test]
fn internal_feedback_can_change_which_named_outcome_is_possible() {
    let mut graph = bloq_graph::BlockGraph::from_text(
        "BLOG 1.0\nmodule main {\n
        0: Y [0,0,0]\n1: XZX [0,0,1]\n2: Y [0,0,2]\n
        0 -> +Z\n1 -> +Z\nm = measure 2\n}",
    )
    .unwrap()
    .materialize_flat_graph()
    .unwrap();
    let source = "OPENQASM 2.0; qreg q[1]; creg m[1];
        reset q; h q; sdg q; sdg q; h q; measure q -> m;";
    let options = FeedbackOptions::default().with_search_limit(0);
    assert!(matches!(
        infer_feedback_with(source, &graph, &options),
        Err(FeedbackInferenceError::SupportMismatch { .. })
    ));
    let options = options
        .with_search_limit(3)
        .with_internal_sites(vec![IVec3::ZERO]);
    let actions = infer_feedback_with(source, &graph, &options).unwrap();
    assert!(
        matches!(actions.as_slice(), [Action::Feedback { targets, condition: None }]
        if targets.len() == 1 && targets[0].pauli == PauliBasis::Z && targets[0].target == IVec3::ZERO)
    );
    let mut combined = graph.actions();
    combined.extend(actions);
    graph.set_actions(combined).unwrap();
    assert!(
        infer_feedback_with(source, &graph, &options)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ancilla_cnot_parity_drives_feedback_on_a_separate_live_wire() {
    use bloq_graph::{
        Basis, Block, BlockGraph, BlockKind, CubeKind, Direction, MeasureTarget, Pipe,
    };
    let mut graph = BlockGraph::new();
    for x in [0, 1, 3] {
        graph.add_block(Block::new(IVec3::new(x, 0, 0), BlockKind::Port));
        graph.add_block(Block::new(
            IVec3::new(x, 0, 1),
            BlockKind::Cube(CubeKind::XZX),
        ));
        graph.add_block(Block::new(
            IVec3::new(x, 0, 2),
            if x == 3 {
                BlockKind::Port
            } else {
                BlockKind::Measurement(Basis::X)
            },
        ));
        for z in [0, 1] {
            graph.add_pipe(Pipe::new(IVec3::new(x, 0, z), Direction::ZPLUS));
        }
    }
    graph.add_pipe(Pipe::new(IVec3::Z, Direction::XPLUS));
    graph
        .set_actions(vec![Action::Measure {
            target: MeasureTarget::Edge {
                src: IVec3::Z,
                dir: Direction::XPLUS,
            },
            name: "m".into(),
        }])
        .unwrap();
    let source = "OPENQASM 2.0; qreg q[4]; creg m[1]; creg erased[2]; reset q[3];
        cx q[0],q[3]; cx q[1],q[3]; measure q[3] -> m;
        h q[0]; h q[1]; measure q[0] -> erased[0]; measure q[1] -> erased[1];
        if(m==1) x q[2];";
    let options = FeedbackOptions::default()
        .with_search_limit(0)
        .with_discarded_measurement("erased[0]")
        .with_discarded_measurement("erased[1]");
    let actions = infer_feedback_with(source, &graph, &options).unwrap();
    assert!(
        matches!(actions.as_slice(), [Action::Feedback { targets, condition: Some(Expr::Var(name)) }]
        if name == "m" && targets.len() == 1 && targets[0].pauli == PauliBasis::X && targets[0].target == IVec3::new(3,0,2))
    );
    let mut combined = graph.actions();
    combined.extend(actions);
    graph.set_actions(combined).unwrap();
    assert!(
        infer_feedback_with(source, &graph, &options)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ccz_gate_teleport_recovers_quadratic_z_feedback_on_multiplex_ports() {
    let mut graph = without_feedback(GalleryItem::CCZGateTeleport);
    let outputs = vec![
        IVec3::new(0, 0, 1),
        IVec3::new(1, 0, 1),
        IVec3::new(2, 0, 1),
    ];
    let mut inputs = vec![
        IVec3::new(0, 2, 0),
        IVec3::new(1, 2, 0),
        IVec3::new(2, 2, 0),
    ];
    inputs.extend(outputs.iter().copied());
    let options = FeedbackOptions::default().with_search_limit(0)
        .with_boundaries(BoundaryOrder::new(inputs, outputs))
        .with_input_preparation("OPENQASM 2.0; qreg q[6]; reset q[0]; reset q[1]; reset q[2]; h q[0]; h q[1]; h q[2]; ccz q[0],q[1],q[2];");
    let source = "OPENQASM 2.0; qreg q[3]; ccz q[0],q[1],q[2];";
    let actions = infer_feedback_with(source, &graph, &options).unwrap();
    assert_eq!(actions.len(), 3);
    assert!(actions.iter().all(|action| matches!(action, Action::Feedback { targets, condition: Some(Expr::Binary(BinaryOp::And, _, _)) } if targets.len() == 1 && targets[0].pauli == PauliBasis::Z)));
    let mut combined = graph.actions();
    combined.extend(actions);
    graph.set_actions(combined).unwrap();
    assert!(
        infer_feedback_with(source, &graph, &options)
            .unwrap()
            .is_empty()
    );
    // Flipping a CCZ resource leg contributes CZ on the other two data
    // wires. This needs an internal action shared by every structural arm.
    let source = "OPENQASM 2.0; qreg q[3]; cz q[0],q[1]; ccz q[0],q[1],q[2];";
    assert!(matches!(
        infer_feedback_with(source, &graph, &options),
        Err(FeedbackInferenceError::NoOutputPauli { .. })
    ));
    let options = options
        .with_search_limit(3)
        .with_internal_sites(vec![IVec3::new(2, 2, 0)]);
    let actions = infer_feedback_with(source, &graph, &options).unwrap();
    assert!(actions.iter().any(|action| matches!(action, Action::Feedback { targets, condition: None } if targets[0].target == IVec3::new(2,2,0))));
    let mut combined = graph.actions();
    combined.extend(actions);
    graph.set_actions(combined).unwrap();
    assert!(
        infer_feedback_with(source, &graph, &options)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn correspondence_support_and_resource_limits_are_not_silent_defaults() {
    let graph = GalleryItem::T
        .build()
        .materialize_root_graph()
        .expect("gallery flat projection");
    let output_only = FeedbackOptions::default().with_search_limit(0);
    assert!(matches!(
        infer_feedback_with("OPENQASM 2.0; qreg q[1];", &graph, &output_only),
        Err(FeedbackInferenceError::NoOutputPauli { .. })
    ));
    for options in [
        output_only.clone().with_limits(1, 256, 20),
        output_only.clone().with_limits(256, 0, 20),
        output_only.clone().with_limits(256, 256, 1),
    ] {
        assert!(matches!(
            infer_feedback_with("OPENQASM 2.0; qreg q[1]; t q;", &graph, &options),
            Err(FeedbackInferenceError::Limit { .. })
        ));
    }
    let source = "OPENQASM 2.0; qreg q[1]; qreg coin[1]; creg result[1]; reset coin; h coin; measure coin -> result; t q;";
    assert!(matches!(
        infer_feedback_with(source, &graph, &output_only),
        Err(FeedbackInferenceError::Binding(_))
    ));
    assert!(
        infer_feedback_with(
            source,
            &graph,
            &output_only
                .clone()
                .with_measurement("result[0]", Expr::Var("mzz".into()))
        )
        .unwrap()
        .is_empty()
    );
    assert!(matches!(
        infer_feedback_with(
            source,
            &graph,
            &output_only.with_measurement("result[0]", Expr::Var("missing".into()))
        ),
        Err(FeedbackInferenceError::Binding(_))
    ));

    let zero_source = "OPENQASM 2.0; qreg q[1]; creg m[1]; reset q; measure q -> m;";
    assert!(matches!(
        infer_feedback_with(
            zero_source,
            &bloq_graph::BlockGraph::new(),
            &FeedbackOptions::default()
                .with_search_limit(0)
                .with_fixed_measurement("m[0]", true)
        ),
        Err(FeedbackInferenceError::SupportMismatch { .. })
    ));
    let cnot = GalleryItem::CNOT
        .build()
        .materialize_root_graph()
        .expect("gallery flat projection");
    let ordinary = bloq_graph::verify::LogicalVerifier::new(&cnot)
        .unwrap()
        .boundaries()
        .clone();
    let mut inputs = ordinary.inputs().to_vec();
    inputs.reverse();
    assert!(matches!(
        infer_feedback_with(
            "OPENQASM 2.0; qreg q[2]; cx q[0],q[1];",
            &cnot,
            &FeedbackOptions::default()
                .with_search_limit(0)
                .with_boundaries(BoundaryOrder::new(inputs, ordinary.outputs().to_vec()))
        ),
        Err(FeedbackInferenceError::NoOutputPauli { .. })
    ));
}

#[test]
fn y_readout_labels_survive_preceding_feedback_and_explicit_rejection() {
    let source =
        "OPENQASM 2.0; qreg q[2]; creg m[1]; sdg q[0]; h q[0]; measure q[0] -> m; if(m==1) x q[1];";
    let mut graph = bloq_graph::BlockGraph::from_text(
        "BLOG 1.0\nmodule main {\n
        in q: data = 0\nin data: data = 2\nout out: data = 4\n
        0: Port [0,0,0]\n1: Y [0,0,1]\n2: Port [2,0,0]\n3: XZX [2,0,1]\n4: Port [2,0,2]\n
        0 -> +Z\n2 -> +Z\n3 -> +Z\nm = measure 1\n}",
    )
    .unwrap()
    .materialize_flat_graph()
    .unwrap();
    let options = FeedbackOptions::default().with_search_limit(0);
    let actions = infer_feedback_with(source, &graph, &options).unwrap();
    assert!(
        matches!(actions.as_slice(), [Action::Feedback { targets, condition: Some(Expr::Var(name)) }] if name == "m" && targets[0].pauli == PauliBasis::X)
    );
    let mut combined = graph.actions();
    combined.extend(actions);
    graph.set_actions(combined).unwrap();
    graph
        .add_action(Action::Feedback {
            targets: vec![bloq_graph::FeedbackTarget {
                pauli: PauliBasis::Z,
                target: IVec3::ZERO,
                direction: None,
            }],
            condition: None,
        })
        .unwrap();
    let corrected_source = source.replace("sdg q[0];", "z q[0]; sdg q[0];");
    assert!(
        infer_feedback_with(&corrected_source, &graph, &options)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        infer_feedback_with(source, &graph, &options),
        Err(FeedbackInferenceError::NoOutputPauli { .. })
    ));
    graph
        .add_action(Action::DiscardIf(Expr::Var("m".into())))
        .unwrap();
    assert!(matches!(
        infer_feedback_with(&corrected_source, &graph, &options),
        Err(FeedbackInferenceError::RejectionMismatch { .. })
    ));
    assert!(
        infer_feedback_with(
            &corrected_source,
            &graph,
            &options.with_discard_if(Expr::Var("m".into()))
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn ccz_maj_recovers_both_carry_flips() {
    let mut graph = without_feedback(GalleryItem::CCZInjectedMaj);
    let options = FeedbackOptions::default().with_search_limit(0)
        .with_boundaries(BoundaryOrder::new(vec![
            IVec3::new(-1,0,2), IVec3::new(-1,1,2), IVec3::new(-1,2,2),
            IVec3::new(0,1,-1), IVec3::new(2,-1,0), IVec3::new(5,1,3),
        ], vec![IVec3::new(4,0,5), IVec3::new(3,0,5), IVec3::new(2,3,0), IVec3::new(3,2,5), IVec3::new(4,2,5)]))
        .with_input_preparation("OPENQASM 2.0; qreg q[6]; reset q[0]; reset q[1]; reset q[2]; h q[0]; h q[1]; h q[2]; ccz q[0],q[1],q[2];");
    let source = "OPENQASM 2.0; qreg q[5]; reset q[2]; reset q[3];
        cx q[1],q[0]; cx q[1],q[4]; ccx q[0],q[4],q[3]; cx q[1],q[3]; cx q[3],q[2];";
    let actions = infer_feedback_with(source, &graph, &options).unwrap();
    assert_eq!(actions.len(), 2);
    assert!(actions.iter().all(|action| matches!(action, Action::Feedback { targets, condition: Some(Expr::Binary(BinaryOp::And, _, _)) } if targets.len() == 1 && targets[0].pauli == PauliBasis::X)));
    let mut combined = graph.actions();
    combined.extend(actions);
    graph.set_actions(combined).unwrap();
    assert!(
        infer_feedback_with(source, &graph, &options)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn clifford_and_t_galleries_omit_geometry_frames_and_restore_constants() {
    for (gallery, body, qubits) in [
        (GalleryItem::CNOT, "cx q[0],q[1];", 2),
        (GalleryItem::CZSpatialH, "cz q[0],q[1];", 2),
        (GalleryItem::CZTemporalH, "cz q[0],q[1];", 2),
        (GalleryItem::S, "s q[0];", 1),
        (GalleryItem::T, "t q[0];", 1),
        (GalleryItem::BellState, "reset q; h q[0]; cx q[0],q[1];", 2),
        (
            GalleryItem::GHZ,
            "reset q; h q[0]; cx q[0],q[1]; cx q[0],q[2]; cx q[0],q[3];",
            4,
        ),
    ] {
        let mut graph = without_feedback(gallery);
        let source = format!("OPENQASM 2.0; qreg q[{qubits}]; {body}");
        let actions = infer_feedback(&source, &graph)
            .unwrap_or_else(|error| panic!("{gallery:?}: {error:?}"));
        if gallery == GalleryItem::S {
            assert!(matches!(
                actions.as_slice(),
                [Action::Feedback {
                    condition: None,
                    ..
                }]
            ));
            let mut all = graph.actions();
            all.extend(actions);
            graph.set_actions(all).unwrap();
            assert!(infer_feedback(&source, &graph).unwrap().is_empty());
        } else {
            assert!(actions.is_empty(), "{gallery:?}: {actions:?}");
        }
    }
}
