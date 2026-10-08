//! Count source-query support: `profile_source_planner WIDTH|yoke:WIDTH`.
//! No physical execution.

use std::time::Instant;

use bloq_graph::{GuardedSurfaceSpace, GuardedTopology, ModuleCertificationLimits};

fn main() {
    let case = std::env::args().nth(1).expect("width or yoke:width");
    let (source_kind, bits, authored) = if let Some(width) = case.strip_prefix("yoke:") {
        let width = width.parse().expect("yoke width");
        (
            "yoked_memory",
            width,
            bloq_test::benchmark::yoked_memory(width),
        )
    } else {
        let bits = case.parse().expect("adder width");
        assert!(bits >= 3, "benchmark width must be at least three bits");
        (
            "controlled_adder",
            bits,
            bloq_test::benchmark::controlled_adder(bits),
        )
    };
    let linked = bloq_graph::flatten_module_definition(&authored, authored.root(), "")
        .expect("diagnostic source geometry");
    let graph = linked.graph.fix_shadowed_faces();
    let start = Instant::now();
    let topology =
        GuardedTopology::new(&graph, ModuleCertificationLimits::DEFAULT).expect("source topology");
    let topology_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut scope_sites = std::collections::BTreeMap::<String, usize>::new();
    for &position in topology.sites.keys() {
        let scope = linked
            .sites
            .get(&glam::IVec3::from_array(position))
            .map(|site| site.instance_path.as_str())
            .unwrap_or("");
        *scope_sites.entry(scope.to_owned()).or_default() += 1;
    }
    let root_sites = scope_sites.get("").copied().unwrap_or(0);
    let largest_child_scope = scope_sites
        .iter()
        .filter(|(scope, _)| !scope.is_empty())
        .map(|(_, &count)| count)
        .max()
        .unwrap_or(0);
    let relation_start = Instant::now();
    let relation =
        GuardedSurfaceSpace::new(topology, &linked.sites, ModuleCertificationLimits::DEFAULT)
            .expect("signed source relation");
    let composition_ms = relation_start.elapsed().as_secs_f64() * 1000.0;
    let readout_start = Instant::now();
    let plan = relation
        .plan_readouts()
        .expect("complete generic readout plan");
    let readout_planning_ms = readout_start.elapsed().as_secs_f64() * 1000.0;
    let planning_ms = start.elapsed().as_secs_f64() * 1000.0;
    let supports = (0..plan.surfaces.len())
        .map(|index| plan.support_sites(index).len())
        .collect::<Vec<_>>();
    let fragments = (0..plan.surfaces.len())
        .flat_map(|index| plan.query_fragments(index).iter().copied())
        .collect::<std::collections::BTreeSet<_>>();
    let fragment_sites = fragments
        .iter()
        .map(|&id| plan.query_fragment(id).1.len())
        .sum::<usize>();
    println!(
        "{{\"source_kind\":\"{source_kind}\",\"bits\":{bits},\"planning_ms\":{planning_ms},\"topology_ms\":{topology_ms},\"composition_ms\":{composition_ms},\"readout_planning_ms\":{readout_planning_ms},\"root_sites\":{root_sites},\"largest_child_scope\":{largest_child_scope},\"queries\":{},\"support_site_visits\":{},\"max_query_support\":{},\"boolean_nodes\":{},\"distinct_fragments\":{},\"fragment_site_visits\":{fragment_sites}}}",
        supports.len(),
        supports.iter().sum::<usize>(),
        supports.iter().max().copied().unwrap_or(0),
        plan.topology.diagram.nodes().len(),
        fragments.len()
    );
}
