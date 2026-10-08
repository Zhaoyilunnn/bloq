use bloq_compile::compile;
use bloq_graph::GalleryItem;
use bloq_ir::{BloqEdge, NodeProvenance, ValueRole};

mod common;

#[test]
fn ccz_corrections_share_three_independent_source_selectors() {
    let source = GalleryItem::CCZGateTeleport.build();
    let graph = source.flatten().unwrap();
    let regions = graph.branch_regions().unwrap();
    assert_eq!(regions.len(), 3);
    assert!(regions.iter().all(|region| region.incoming.len() == 2));
    let projections = graph.branch_projections().unwrap();
    assert_eq!(projections.len(), 8);
    for projection in projections {
        projection.graph().stabilizers().unwrap();
    }
    let bloq = compile(&source, 3).unwrap();
    let selectors = bloq
        .nodes()
        .filter(|(_, node)| matches!(node.provenance, NodeProvenance::BranchSelector { .. }))
        .count();
    assert_eq!(selectors, 3);
    assert!(bloq.has_conditional_membership());
    bloq.validate().unwrap();
    let mut count = 0;
    for selected in common::pinned_memberships(&bloq) {
        count += 1;
        assert!(!selected.has_conditional_membership());
        selected.validate().unwrap();
    }
    assert_eq!(count, 8);
    let mut feedback_actions = bloq
        .edges()
        .filter_map(|edge| match edge.edge {
            BloqEdge::Value {
                role: ValueRole::FeedbackFold { action },
                ..
            } => Some(*action),
            _ => None,
        })
        .collect::<Vec<_>>();
    feedback_actions.sort_unstable();
    assert_eq!(feedback_actions, [6, 7, 8]);
}
