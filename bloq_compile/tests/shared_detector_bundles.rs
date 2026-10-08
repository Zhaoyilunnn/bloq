//! Shared detector rows retain compiled source selections and VM behavior.

#![cfg(test)]

use std::collections::BTreeMap;

use bloq_compile::{CompileConfig, CompileContext};
use bloq_ir::{Bloq, BloqNodeId, BloqNodeKind, LevelPath, NodeDetector, NodeProvenance, SubGraph};
use bloq_test::benchmark::controlled_adder;
use bloq_vm::run_bloq_with_io;
use glam::IVec3;

fn level_mut<'a>(bloq: &'a mut Bloq, path: &LevelPath) -> &'a mut SubGraph {
    let mut level = bloq.top_mut();
    for segment in path.segments() {
        let BloqNodeKind::Region(region) = &mut level.node_mut(segment.region).unwrap().kind else {
            panic!("level path names a region");
        };
        level = region
            .bodies_mut()
            .find_map(|(body, level)| (body == segment.body).then_some(level))
            .unwrap();
    }
    level
}

fn inline_expanded(bloq: &Bloq) -> Bloq {
    let mut expanded = bloq.clone();
    for (path, level) in bloq.levels() {
        for (id, quantum) in level.quantum_nodes() {
            let detectors = bloq
                .node_detectors(quantum)
                .unwrap()
                .map(|row| row.to_owned())
                .collect();
            let mut next = quantum.detectors.len();
            let bundle_ranges = quantum
                .detector_bundles
                .iter()
                .map(|use_| {
                    let first = next;
                    next += bloq
                        .detector_bundles()
                        .get(use_.bundle)
                        .unwrap()
                        .detectors()
                        .len();
                    u32::try_from(first).unwrap()..u32::try_from(next).unwrap()
                })
                .collect::<Vec<_>>();
            let quantum = level_mut(&mut expanded, &path)
                .node_mut(id)
                .unwrap()
                .expect_quantum_mut();
            quantum.detectors = detectors;
            quantum.detector_bundles.clear();
            for guard in &mut quantum.guards {
                for use_index in std::mem::take(&mut guard.detector_bundles) {
                    guard
                        .detectors
                        .extend(bundle_ranges[use_index as usize].clone());
                }
            }
        }
    }
    expanded
}

fn detector_rows(bloq: &Bloq) -> Vec<(LevelPath, BloqNodeId, Vec<NodeDetector>)> {
    bloq.levels()
        .flat_map(|(path, level)| {
            level.quantum_nodes().map(move |(id, node)| {
                (
                    path.clone(),
                    id,
                    bloq.node_detectors(node)
                        .unwrap()
                        .map(|row| row.to_owned())
                        .collect(),
                )
            })
        })
        .collect()
}

fn reachable_choices(bloq: &Bloq, mask: u8) -> BTreeMap<String, bool> {
    let selectors = bloq
        .nodes()
        .filter_map(|(id, node)| match &node.provenance {
            NodeProvenance::BranchSelector { name } => Some((name.clone(), id)),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let mut choices = (0..3)
        .map(|bit| (format!("cz{bit}"), mask & (1 << bit) != 0))
        .collect::<BTreeMap<_, _>>();
    let mut pins = choices
        .iter()
        .map(|(name, &value)| (selectors[name], value))
        .collect::<Vec<_>>();
    let mut predicates = bloq_ir::lowering::PredicateAnalysis::new(bloq.top());
    assert!(predicates.assignment_reachable(&pins).unwrap());
    // Complete adaptive readouts linearly instead of enumerating their joint domain.
    for (name, id) in selectors {
        if choices.contains_key(&name) {
            continue;
        }
        pins.push((id, false));
        if !predicates.assignment_reachable(&pins).unwrap() {
            pins.last_mut().unwrap().1 = true;
            assert!(predicates.assignment_reachable(&pins).unwrap());
        }
        choices.insert(name, pins.last().unwrap().1);
    }
    choices
}

#[test]
fn compiled_adder_bundles_preserve_selected_rows_codecs_and_execution() {
    let program = controlled_adder(3);
    let distance = 3;
    let shared = CompileContext::new(CompileConfig::new(distance))
        .compile(&program)
        .unwrap()
        .bloq;
    let stored_rows = shared
        .detector_bundles()
        .iter()
        .map(|(_, bundle)| bundle.detectors().len())
        .sum::<usize>();
    let expanded_rows = shared
        .levels()
        .flat_map(|(_, level)| level.quantum_nodes())
        .map(|(_, node)| shared.node_detector_count(node).unwrap())
        .sum::<usize>();
    assert!(
        stored_rows > 0,
        "d{distance} must exercise shared detector rows"
    );
    assert!(
        expanded_rows > stored_rows,
        "d{distance} must reuse stored rows"
    );
    let uses = shared
        .levels()
        .flat_map(|(_, level)| level.quantum_nodes())
        .map(|(_, node)| node.detector_bundles.len())
        .sum::<usize>();
    assert!(uses > shared.detector_bundles().len());
    shared.validate().unwrap();

    let expanded = inline_expanded(&shared);
    let text = Bloq::from_text(&shared.to_text()).unwrap();
    let binary = Bloq::from_binary(&shared.to_binary()).unwrap();
    let rows = detector_rows(&shared);
    let selected_rows = [0, 0b111, 0b010, 0b101].map(|mask| {
        let choices = reachable_choices(&shared, mask);
        let reference = shared.pin_membership(&choices).unwrap();
        (choices, detector_rows(&reference))
    });
    for other in [&expanded, &text, &binary] {
        other.validate().unwrap();
        assert_eq!(detector_rows(other), rows);
        for (choices, reference) in &selected_rows {
            let selected = other.pin_membership(choices).unwrap();
            assert_eq!(detector_rows(&selected), *reference);
        }
    }
    let graph = program.materialize_root_graph().unwrap();
    let offset = IVec3::new(0, 0, -*graph.spans().unwrap().2.start());
    let resources = program
        .root()
        .interface
        .quantum_ports
        .iter()
        .filter(|port| port.resource_type == "ccz")
        .map(|port| port.position + offset)
        .collect::<Vec<_>>();
    let (resources, remainder) = resources.as_chunks::<3>();
    assert!(remainder.is_empty());
    let run = |bloq| {
        run_bloq_with_io(
            bloq,
            2,
            0xC0FFEE,
            |_, context| {
                for &group in resources {
                    context.prepare_ccz(group)?;
                }
                Ok(())
            },
            |_, _| Ok(()),
        )
        .unwrap()
    };
    let report = run(&shared);
    assert_eq!(report.discarded, 0);
    assert!(report.all_detectors_constant());
    assert!(
        report
            .detectors
            .iter()
            .all(|detector| detector.value != Some(true))
    );
    assert_eq!(run(&expanded), report);
}
