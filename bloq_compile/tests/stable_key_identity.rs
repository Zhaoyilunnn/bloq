//! Compile-stable node identity across the two compile entry points.
//!
//! `BloqNode::stable_key` exists so a caller can name a node in one compile and
//! find it in another — the concrete need being a distance study that compiles
//! a program *and* its Clifford proxy and has to line the two up. Only
//! `bloq_compile` can check that: `bloq_ir`, where the keys live, cannot reach
//! the compiler that produces the two programs.

use bloq_compile::{CompileConfig, compile, compile_clifford_proxy};
use bloq_graph::GalleryItem;
use bloq_ir::Bloq;

/// Smallest odd distance: identity is a structural property, so a bigger code
/// only makes the test slower.
const DISTANCE: u32 = 3;

/// Every node a program and its proxy share must agree on what it was built
/// from — matching source blocks — and the shared set must be big enough to be
/// worth carrying (both graphs are mostly the same program).
#[test]
fn stable_keys_survive_a_clifford_proxy_compile() {
    // A T gate has one selective site, so the proxy needs exactly one pin, and
    // it exercises the interesting case: the dynamic region collapses while the
    // surrounding Clifford blocks stay put.
    let graph = GalleryItem::T.build();
    let config = CompileConfig::try_new(DISTANCE).expect("valid distance");

    let program = compile(&graph, DISTANCE).expect("t_gate compiles");
    let proxy = compile_clifford_proxy(config, &graph, &[true])
        .expect("t_gate has one selective site")
        .bloq;

    let program_keys = program.stable_key_map().expect("keys are unique");
    let proxy_keys = proxy.stable_key_map().expect("keys are unique");

    let shared: Vec<_> = program_keys
        .keys()
        .filter(|key| proxy_keys.contains_key(*key))
        .collect();
    assert!(
        !shared.is_empty(),
        "no node kept its identity across the proxy compile"
    );

    for key in shared {
        let in_program = program
            .node(program_keys[key])
            .expect("map holds live node ids");
        let in_proxy = proxy
            .node(proxy_keys[key])
            .expect("map holds live node ids");
        assert_eq!(
            in_program.provenance, in_proxy.provenance,
            "nodes sharing key {key} were built from different sources"
        );
    }
}

/// The keys must also be stable across two compiles of the same graph — the
/// weaker property the one above builds on, and the one a caller relies on when
/// it recompiles rather than proxies.
#[test]
fn stable_keys_are_reproducible_across_compiles() {
    for item in [GalleryItem::CNOT, GalleryItem::T, GalleryItem::BellState] {
        let graph = item.build();
        let first = compile(&graph, DISTANCE).expect("gallery entry compiles");
        let second = compile(&graph, DISTANCE).expect("gallery entry compiles");

        assert_eq!(
            keys_by_node(&first),
            keys_by_node(&second),
            "{item} keyed its nodes differently on a second compile"
        );
    }
}

/// The key map inverted: node id -> key, which is what an equality assertion
/// wants (the map itself is keyed the other way).
fn keys_by_node(program: &Bloq) -> Vec<(u32, String)> {
    let mut pairs: Vec<_> = program
        .stable_key_map()
        .expect("keys are unique")
        .into_iter()
        .map(|(key, node)| (node.0, key.to_string()))
        .collect();
    pairs.sort();
    pairs
}
