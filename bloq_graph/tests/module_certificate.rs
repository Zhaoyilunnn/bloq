use bloq_graph::{BlockGraph, ModuleCertificationError, ModuleCertificationLimits};

#[test]
fn leaf_certificate_exposes_signed_hadamard_rows_and_lazy_witnesses() {
    let program = BlockGraph::from_text(
        r"BLOG 1.0

module main {
  in q_in: data = 0
  out q_out: data = 3
  0: Port [0,0,0] role=input <q_in>
  1: XZX [0,0,1]
  2: ZXZ [0,0,2]
  3: Port [0,0,3] role=output <q_out>
  [0,0,0] -> +Z
  [0,0,1] -H> +Z
  [0,0,2] -> +Z
}",
    )
    .expect("Hadamard module is valid");
    let certificate = program
        .certify_leaf("main", ModuleCertificationLimits::DEFAULT)
        .expect("Hadamard leaf is certifiable");
    assert_eq!(certificate.module(), "main");
    let graph = program.root().local_body().fix_shadowed_faces();
    let zx = graph.to_zx_graph().expect("Hadamard graph has a ZX view");
    let columns = program
        .root()
        .interface
        .quantum_ports
        .iter()
        .map(|port| zx.node_at(port.position).expect("declared port exists").id)
        .collect::<Vec<_>>();
    let expected = [("XZ".to_owned(), 0), ("ZX".to_owned(), 0)];
    let mut actual = certificate
        .boundary_rows()
        .map(|row| (row.paulis.to_string(), row.phase()))
        .collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(actual, expected);
    for (index, row) in certificate.boundary_rows().enumerate() {
        let witness = certificate
            .materialize_boundary_row(index)
            .expect("boundary row retains its witness");
        assert_eq!(witness.paulis.len(), zx.total_ids());
        assert_eq!(witness.phase(), row.phase());
        for (boundary, &column) in columns.iter().enumerate() {
            assert_eq!(witness.paulis.get(column), row.paulis.get(boundary));
        }
    }
    assert!(
        certificate
            .materialize_boundary_row(expected.len())
            .is_none()
    );
    let error = program
        .certify_leaf(
            "main",
            ModuleCertificationLimits {
                max_frontier_width: expected.len(),
                ..ModuleCertificationLimits::DEFAULT
            },
        )
        .expect_err("live frontier is wider than the declared interface");
    assert!(matches!(
        error,
        ModuleCertificationError::ResourceLimited {
            phase: "frontier columns",
            observed,
            limit: 2,
            ..
        } if observed > 2
    ));
    assert!(matches!(
        program.certify_leaf(
            "main",
            ModuleCertificationLimits {
                max_frontier_width: 1,
                ..ModuleCertificationLimits::DEFAULT
            }
        ),
        Err(ModuleCertificationError::ResourceLimited {
            phase: "frontier columns",
            observed: 2,
            limit: 1,
            ..
        })
    ));
}
