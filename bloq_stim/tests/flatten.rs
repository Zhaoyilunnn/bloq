//! Flatten acceptance: `Bloq::flatten` unrolls every `REPEAT` block and
//! rewrites detector side tables so the emitted circuit keeps the same
//! observables, detector count, and graphlike distance as the looped program.
//!
//! The base two-cube compile unrolls its rounds into per-layer instances, so
//! the looped fixture is a decoder-wait splice: its padding node instantiates
//! the recorded looped template (`first round + REPEAT r`), giving the program
//! a genuine repeat body with loop-carried detector state.

use bloq_compile::{CompileConfig, CompileContext};
use bloq_graph::BlockGraph;
use bloq_ir::{Bloq, BloqEdge, BloqNodeId, LevelPath, MemoryRoundTarget};
use bloq_stim::{emit_bloq_stim, emit_bloq_stim_with_stim_noise};
use stim::Circuit;

mod common;
use common::uniform_depolarizing;

const DISTANCE: u32 = 3;
/// Enough wait rounds that the emitter keeps a `REPEAT` block instead of
/// unrolling the tail itself.
const WAIT_ROUNDS: u32 = 5;

/// Two ZXZ cubes stacked in time, with a decoder wait spliced into the seam.
fn compile_waiting_two_cubes() -> Bloq {
    let graph = BlockGraph::from_blog_text(
        "BLOG 1.0\n\n  0: ZXZ [0,0,0]\n  1: ZXZ [0,0,1]\n  [0,0,0] -> +Z\n",
    )
    .expect("two-cube memory graph parses");
    let ctx = CompileContext::new(CompileConfig::new(DISTANCE));
    let mut bloq = ctx.compile(&graph).expect("two-cube graph compiles").bloq;
    let (from, to) = quantum_seam(&bloq);
    bloq.insert_memory_rounds(
        MemoryRoundTarget::Edge {
            path: LevelPath::default(),
            from,
            to,
        },
        WAIT_ROUNDS,
    )
    .expect("decoder-wait insertion verifies and applies");
    bloq
}

/// The program's single `Quantum` edge.
fn quantum_seam(bloq: &Bloq) -> (BloqNodeId, BloqNodeId) {
    let mut seams = bloq.edges().filter_map(|edge| {
        let BloqEdge::Quantum(quantum) = edge.edge else {
            return None;
        };
        let [_seam] = quantum.pipes.as_slice() else {
            panic!("two-cube seam carries exactly one pipe");
        };
        Some((edge.source, edge.target))
    });
    let seam = seams.next().expect("two-cube program has a Quantum seam");
    assert!(seams.next().is_none(), "exactly one Quantum seam expected");
    seam
}

fn noisy_graphlike_distance(bloq: &Bloq) -> usize {
    let noisy: Circuit = emit_bloq_stim_with_stim_noise(bloq, &uniform_depolarizing())
        .expect("emit noisy Stim")
        .parse()
        .expect("parse noisy Stim");
    common::graphlike_distance(&noisy).expect("detectors are deterministic and graphlike")
}

#[test]
fn flatten_preserves_observables_detectors_and_distance() {
    let mut bloq = compile_waiting_two_cubes();
    let baseline_text = emit_bloq_stim(&bloq).expect("emit looped Stim");
    assert!(
        baseline_text.contains("REPEAT"),
        "test is vacuous unless the looped program actually repeats"
    );
    let baseline: Circuit = baseline_text.parse().expect("parse looped Stim");
    let baseline_distance = noisy_graphlike_distance(&bloq);
    assert_eq!(baseline_distance, DISTANCE as usize);

    bloq.flatten().expect("program flattens");
    bloq.validate().expect("flattened program validates");

    let flat_text = emit_bloq_stim(&bloq).expect("emit flattened Stim");
    assert!(
        !flat_text.contains("REPEAT"),
        "flattened program must emit no REPEAT blocks"
    );
    let flat: Circuit = flat_text.parse().expect("parse flattened Stim");
    assert_eq!(flat.num_observables(), baseline.num_observables());
    assert_eq!(flat.num_detectors(), baseline.num_detectors());
    assert_eq!(
        noisy_graphlike_distance(&bloq),
        baseline_distance,
        "flattening must not change the graphlike distance"
    );
}
