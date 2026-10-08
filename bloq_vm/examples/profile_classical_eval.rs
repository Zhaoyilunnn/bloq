//! Pure-classical VM probe: `profile_classical_eval CALLS SHOTS`.
//! Measures preparation and execution without quantum simulation work.

use std::time::Instant;

use bloq_ir::{Bloq, BloqEdge, BloqNode, ClassicalExpr, ClassicalNode};
use bloq_vm::{LoweringConfig, lower};

fn main() {
    let calls: usize = std::env::args()
        .nth(1)
        .expect("calls")
        .parse()
        .expect("calls");
    let shots: usize = std::env::args()
        .nth(2)
        .expect("shots")
        .parse()
        .expect("shots");
    assert!(shots > 0);
    let mut bloq = Bloq::new();
    let values = [false, true, false];
    let inputs = values.map(|value| {
        bloq.add_node(BloqNode::classical(ClassicalNode::Compute {
            expr: ClassicalExpr::Const(value),
        }))
    });
    let mut expected = Vec::with_capacity(calls);
    for i in 0..calls {
        let node = bloq.add_node(BloqNode::classical(ClassicalNode::Compute {
            expr: ClassicalExpr::Select(Box::new([
                ClassicalExpr::In(0),
                ClassicalExpr::xor([ClassicalExpr::In(1), ClassicalExpr::In(2)]),
                ClassicalExpr::And(Box::new([
                    ClassicalExpr::In(1),
                    ClassicalExpr::Not(Box::new(ClassicalExpr::In(2))),
                ])),
            ])),
        }));
        for slot in 0..3 {
            bloq.add_edge(inputs[(i + slot) % 3], node, BloqEdge::value(slot as u32));
        }
        let [condition, a, b] = [values[i % 3], values[(i + 1) % 3], values[(i + 2) % 3]];
        expected.push(Some(if condition { a & !b } else { a ^ b }));
    }
    bloq.optimize().expect("optimize");
    let start = Instant::now();
    let program = lower(&bloq, &LoweringConfig::default()).expect("prepare");
    let prepare_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    for shot in 0..shots {
        let result = bloq_vm::runtime::run(
            &program,
            bloq_vm::runtime::RuntimeConfig {
                seed: shot as u64,
                ..Default::default()
            },
        )
        .expect("execute");
        assert_eq!(&result.artifact.final_bits[3..], expected);
    }
    let run_ms = start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{{\"calls\":{calls},\"shots\":{shots},\"prepare_ms\":{prepare_ms},\"run_ms\":{run_ms},\"tasks\":{}}}",
        program.tasks.len()
    );
}
