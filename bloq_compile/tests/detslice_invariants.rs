//! Detector-slice invariants over small gallery programs at several distances.
//! Kept in `bloq_compile` to avoid making `bloq_ir` depend on the compiler in
//! tests.

use bloq_compile::{CompileConfig, CompileContext, compile_clifford_proxy, compile_detslice_proxy};
use bloq_graph::{GalleryCategory, GalleryItem};
use bloq_ir::{
    NodeRef, ProgramRegionId, ProgramSliceError, ProgramSliceOptions, moment_segments,
    program_detector_slices, program_detector_slices_with_options,
};

mod common;

#[test]
fn small_gallery_programs_have_aligned_tapes_and_expected_detector_breaks() {
    let mut options = ProgramSliceOptions::default();
    options.set_detectors_only(true);
    // Cover distance scaling on small circuits and all eight CCZ branch choices
    // at d3. Whole-gallery membership enumeration makes this test exponential.
    for (gallery, distance) in [
        (GalleryItem::CNOT, 3),
        (GalleryItem::CNOT, 5),
        (GalleryItem::CNOT, 7),
        (GalleryItem::T, 3),
        (GalleryItem::T, 5),
        (GalleryItem::T, 7),
        (GalleryItem::CCZGateTeleport, 3),
    ] {
        let context = CompileContext::new(CompileConfig::new(distance));
        let compiled = context
            .compile(&gallery.build())
            .unwrap_or_else(|error| panic!("{gallery:?} d{distance}: {error}"))
            .bloq;
        if compiled.has_conditional_membership() {
            assert!(matches!(
                program_detector_slices(&compiled),
                Err(ProgramSliceError::MembershipSelectionRequired)
            ));
        }
        for bloq in common::pinned_memberships(&compiled) {
            let slices = program_detector_slices_with_options(&bloq, &options)
                .unwrap_or_else(|error| panic!("{gallery:?}: detector tape failed: {error}"));

            // The editor resolves logical rows for its pinned T-to-S proxy. Raw
            // detector rows are independent of that substitution and remain break-free.
            let detector_breaks: Vec<_> = slices
                .breaks
                .iter()
                .filter(|brk| matches!(brk.region, ProgramRegionId::Detector { .. }))
                .collect();
            // External resources are injected by the execution hook after the
            // compiled reset boundary, which the standalone tape cannot model.
            if !gallery.in_category(GalleryCategory::ExternalResource) {
                assert!(
                    detector_breaks.is_empty(),
                    "{gallery:?}: compiled detector regions broke: {detector_breaks:?}",
                );
            }

            let mut flat = bloq.clone();
            flat.flatten().expect("compiled program flattens");

            // Pinning removes untaken members and their detector declarations.
            // Every remaining detector must be tracked.
            let tracked = slices
                .regions
                .iter()
                .map(|region| &region.id)
                .collect::<std::collections::HashSet<_>>();
            for (path, level) in flat.levels() {
                for (node, quantum) in level.quantum_nodes() {
                    let owner = NodeRef {
                        path: path.clone(),
                        node,
                    };
                    let template_detectors: usize = quantum
                        .instances
                        .iter()
                        .map(|instance| {
                            flat.templates()
                                .get(instance.template_id)
                                .unwrap()
                                .detectors
                                .len()
                        })
                        .sum();
                    for index in 0..template_detectors + quantum.detectors.len() {
                        let region = ProgramRegionId::Detector {
                            owner: owner.clone(),
                            detector: index as u32,
                        };
                        assert!(
                            tracked.contains(&region),
                            "{gallery:?}: unexpected missing detector {region:?}"
                        );
                    }
                }
            }
            assert_eq!(slices.skipped_cross_tape, 0, "{gallery:?}");
            for (id, _) in flat.quantum_nodes() {
                let plan = flat[id]
                    .emission_plan(flat.templates())
                    .expect("compiled node instances merge");
                let ops = plan
                    .circuit
                    .body(plan.circuit.entry_body())
                    .map(bloq_ir::circuit::CircuitBody::ops)
                    .unwrap_or(&[]);
                let expected = moment_segments(ops).len();
                let node_ref = NodeRef::top_level(id);
                let actual = slices
                    .per_node
                    .get(&node_ref)
                    .unwrap_or_else(|| panic!("{gallery:?}: node {id:?} missing from per_node"))
                    .0
                    .len();
                assert_eq!(
                    actual, expected,
                    "{gallery:?}: node {id:?} moment count mismatch"
                );
            }
        }
    }
}

#[test]
fn toffoli_clifford_proxy_resolves_break_free_observables() {
    let graph = GalleryItem::ToffoliFromAndDelayedCZ.build();
    let config = CompileConfig::new(3);

    for pins in [[false; 5], [true; 5]] {
        for (label, artifacts) in [
            ("distance", compile_clifford_proxy(config, &graph, &pins)),
            ("detslice", compile_detslice_proxy(config, &graph, &pins)),
        ] {
            let bloq = artifacts
                .unwrap_or_else(|error| panic!("Toffoli {label} proxy {pins:?}: {error}"))
                .bloq;
            let slices = program_detector_slices(&bloq)
                .unwrap_or_else(|error| panic!("Toffoli {label} proxy {pins:?}: {error}"));
            assert!(
                slices.breaks.is_empty(),
                "Toffoli {label} proxy {pins:?} has breaks: {:?}",
                slices.breaks,
            );
        }
    }
}
