//! Whole-corpus compile guard plus a differential regression check for
//! `Bloq::sorted_layout_coords` completeness. Every registry transform accepted
//! for compilation must build and compile, then its declared layout must cover
//! every coordinate any executor touches:
//! instantiated-circuit qubits and boundary-face operator supports, at every
//! nesting level. `run_bloq` and the Stim backend consume the layout API
//! directly, so an under-report would strand their qubit indexing.

use bloq_compile::{CompileConfig, CompileContext};
use bloq_ir::{Bloq, ClassicalNode, SubGraph};
use bloq_test::{CompileReadyCase, compile_ready_test_cases};
use glam::IVec2;
use rustc_hash::FxHashSet;

/// Every coordinate the executors can touch: instantiated-circuit qubits (all
/// ops and measurement records, via `CoordCircuit::qubits`) plus observable
/// boundary-operator supports (already instance-global), recursing into region
/// bodies. Independent re-derivation — deliberately not the layout walk.
fn executor_coords(bloq: &Bloq, level: &SubGraph, coords: &mut FxHashSet<IVec2>) {
    for (_, node) in level.nodes() {
        if let Some(quantum) = node.try_quantum() {
            // Layout covers every guarded member, including untaken choices.
            // Instantiate each member to avoid selecting a single static world.
            for instance in &quantum.instances {
                let mut member = bloq_ir::BloqNode::from_members(Vec::new());
                member.expect_quantum_mut().instances.push(*instance);
                let circuit = member
                    .instantiate_circuit(bloq.templates())
                    .expect("corpus instance emits");
                coords.extend(circuit.qubits());
            }
        }
        if let Some(ClassicalNode::Observable { operators, .. }) = node.try_classical() {
            for operator in operators {
                coords.extend(operator.operator.iter().map(|(coord, _)| *coord));
            }
        }
        if let Some(region) = node.try_region() {
            for (_, body) in region.bodies() {
                executor_coords(bloq, body, coords);
            }
        }
    }
}

#[test]
fn all_corpus_programs_compile_with_complete_layout() {
    let cases = compile_ready_test_cases(None).expect("build compilation corpus");
    let mut failures = Vec::new();
    let ctx = CompileContext::new(CompileConfig::new(3));
    for case in &cases {
        let id = case.id().to_string();
        let ready = match CompileReadyCase::from_test_case(case.clone()) {
            Ok(ready) => ready,
            Err(error) => {
                failures.push(format!("{id}: build: {error}"));
                continue;
            }
        };
        let artifacts = match ctx.compile(&ready.graph) {
            Ok(artifacts) => artifacts,
            Err(error) => {
                failures.push(format!("{id}: compile: {error}"));
                continue;
            }
        };
        let mut bloq = artifacts.bloq;
        // The executors run the flattened program; guard the layout they see.
        bloq.flatten().expect("corpus programs flatten");

        let declared: FxHashSet<IVec2> = bloq
            .sorted_layout_coords()
            .expect("compiled layout coordinates fit i32")
            .iter()
            .copied()
            .collect();
        let mut touched = FxHashSet::default();
        executor_coords(&bloq, bloq.top(), &mut touched);

        let mut missing: Vec<IVec2> = touched.difference(&declared).copied().collect();
        if !missing.is_empty() {
            missing.sort_by_key(|coord| (coord.x, coord.y));
            failures.push(format!("{id}: layout misses {missing:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "corpus compile/layout coverage failures:\n{}",
        failures.join("\n"),
    );
}
