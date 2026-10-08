//! Check an adaptive controlled adder with ideal CCZ resources.
//! --plus requires the phase-free adder to preserve the uniform superposition.

use bloq_ir::Bloq;
use bloq_vm::{ExecError, ShotContext, Simulator, run_bloq_with_io};
use glam::IVec3;

#[path = "../tests/common/choi.rs"]
mod choi;

fn corrected_z(sim: &Simulator, ctx: &ShotContext<'_>, port: IVec3) -> Result<bool, ExecError> {
    let output = ctx
        .outputs
        .iter()
        .find(|output| output.port == port)
        .expect("output exists");
    assert!(!output.consumed, "terminal output is live");
    let value = sim.peek_observable_expectation(&output.logical_z)?;
    assert!(
        (value.abs() - 1.0).abs() < 1e-9,
        "output {port:?} is a basis state: {value}"
    );
    let frame = ctx
        .frames
        .iter()
        .find(|frame| frame.port == port)
        .and_then(|frame| frame.x)
        .expect("output X frame is available");
    Ok((value < 0.0) ^ frame)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let source = args
        .next()
        .expect("usage: verify_controlled_adder SOURCE.blog COMPILED.bloq [SHOTS=128] [--plus]");
    let binary = args.next().expect("compiled binary IR path");
    let shots = args
        .next()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(128);
    assert!(shots > 0);
    let coherent = match args.next().as_deref() {
        None => false,
        Some("--plus") => true,
        Some("--choi") => {
            let program = bloq_graph::BlockGraph::load(source)?;
            let bloq = Bloq::from_binary(&std::fs::read(binary)?)?;
            choi::assert_channel(&program, &bloq, shots);
            println!("Verified {shots} complete Bell-input Choi branches");
            return Ok(());
        }
        Some(other) => panic!("unknown mode {other}"),
    };
    let program = bloq_graph::BlockGraph::load(source)?;
    let graph = program.flatten()?;
    let offset = IVec3::new(0, 0, -*graph.spans().expect("nonempty graph").2.start());
    let ports = &program.interface.quantum_ports;
    let port = |name: &str| {
        ports
            .iter()
            .find(|port| port.name == name)
            .expect("named interface port")
            .position
            + offset
    };
    let bits = ports
        .iter()
        .filter(|port| port.name.starts_with('i') && port.name.ends_with("_in"))
        .count();
    assert!((1..=10).contains(&bits));
    let inputs = std::iter::once("q_in".to_owned())
        .chain((0..bits).map(|bit| format!("i{bit}_in")))
        .chain((0..bits).map(|bit| format!("t{bit}_in")))
        .map(|name| port(&name))
        .collect::<Vec<_>>();
    let outputs = std::iter::once("q_out".to_owned())
        .chain((0..bits).map(|bit| format!("i{bit}_out")))
        .chain((0..bits).map(|bit| format!("s{bit}_out")))
        .map(|name| port(&name))
        .collect::<Vec<_>>();
    let register_mask = (1usize << bits) - 1;
    let input_count = 1usize << inputs.len();
    let input_mask = |shot: usize| {
        if shots == input_count {
            return (shot * 37) % input_count;
        }
        match shot {
            0 => 0,
            1 => 1 | (register_mask << 1) | (1 << (bits + 1)),
            2 => 1 | 2 | (register_mask << (bits + 1)),
            3 => (register_mask << 1) | (register_mask << (bits + 1)),
            _ => shot.wrapping_mul(0x9E37_79B9) % input_count,
        }
    };
    let resources = (0..bits)
        .map(|bit| format!("bit{bit}_and"))
        .chain((0..bits - 1).map(|bit| format!("bit{bit}_maj")))
        .map(|prefix| {
            ports
                .iter()
                .filter(|port| port.resource_type == "ccz" && port.name.starts_with(&prefix))
                .map(|port| port.position + offset)
                .collect::<Vec<_>>()
                .try_into()
                .expect("three resource legs")
        })
        .collect::<Vec<[IVec3; 3]>>();
    let bloq = Bloq::from_binary(&std::fs::read(binary)?)?;
    let mut failures = Vec::new();
    let report = run_bloq_with_io(
        &bloq,
        shots,
        0xC0FFEE,
        |sim, ctx| {
            if !coherent {
                let mask = input_mask(ctx.shot);
                for (bit, port) in inputs.iter().enumerate() {
                    let qubit = ctx
                        .inputs
                        .iter()
                        .find(|input| input.port == *port)
                        .expect("input seed")
                        .qubit;
                    sim.h(qubit);
                    if mask & (1 << bit) != 0 {
                        sim.x(qubit);
                    }
                }
            }
            for &resource in &resources {
                ctx.prepare_ccz(resource)?;
            }
            Ok(())
        },
        |sim, ctx| {
            assert_eq!(ctx.outputs.len(), outputs.len());
            if coherent {
                // A phase-free basis permutation preserves |+> on every data
                // wire. These independent X stabilizers fix the entire state.
                for output in ctx.outputs {
                    let flip = ctx
                        .frames
                        .iter()
                        .find(|frame| frame.port == output.port)
                        .and_then(|frame| frame.z)
                        .expect("output Z frame is available");
                    let expected = if flip { -1.0 } else { 1.0 };
                    let actual = sim.peek_observable_expectation(&output.logical_x)?;
                    assert!(
                        (actual - expected).abs() < 1e-9,
                        "shot {} output {:?}: corrected X must be +1; actual={actual}, frame_z={flip}, branches={:?}",
                        ctx.shot,
                        output.port,
                        ctx.named_branch_selectors
                    );
                }
            } else {
                let mask = input_mask(ctx.shot);
                let q = mask & 1;
                let i = (mask >> 1) & register_mask;
                let t = (mask >> (bits + 1)) & register_mask;
                let expected = q | (i << 1) | (((t + q * i) & register_mask) << (bits + 1));
                let mut actual = 0;
                for (bit, port) in outputs.iter().enumerate() {
                    actual |= usize::from(corrected_z(sim, ctx, *port)?) << bit;
                }
                if actual != expected {
                    failures.push((ctx.shot, mask, expected, actual));
                }
            }
            Ok(())
        },
    )?;
    assert_eq!(report.discarded, 0);
    assert!(
        report
            .detectors
            .iter()
            .all(|detector| detector.constant && detector.value != Some(true))
    );
    assert!(
        report
            .detectors
            .iter()
            .any(|detector| detector.per_shot.len() == shots)
    );
    assert!(
        report.max_rank > 1,
        "the CCZ resources exercised non-Clifford execution"
    );
    assert!(failures.is_empty(), "logical mismatches: {failures:?}");
    println!(
        "Verified {} {} shots and {} detectors; peak rank {}",
        report.shots,
        if coherent {
            "coherent arithmetic"
        } else {
            "arithmetic"
        },
        report.detectors.len(),
        report.max_rank
    );
    Ok(())
}
