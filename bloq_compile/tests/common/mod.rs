//! Shared compiler integration-test fixtures.

use bloq_compile::Bloq;
use bloq_ir::NodeProvenance;

/// Every reachable source choice in the small compiler fixtures.
pub(crate) fn pinned_memberships(program: &Bloq) -> impl Iterator<Item = Bloq> + '_ {
    let names = program
        .nodes()
        .filter_map(|(_, node)| match &node.provenance {
            NodeProvenance::BranchSelector { name } => Some(name.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    // ponytail: exhaustive small-fixture coverage; use symbolic coverage if the
    // corpus gains cases with many independent source selectors.
    let count = 1usize
        .checked_shl(names.len() as u32)
        .expect("small fixture");
    let mut pinned = (0..count).filter_map(move |mask| {
        let choices = names
            .iter()
            .enumerate()
            .map(|(bit, name)| (name.clone(), mask & (1 << bit) != 0))
            .collect();
        match program.pin_membership(&choices) {
            Ok(pinned) => Some(pinned),
            Err(bloq_ir::MembershipPinError::UnreachableAssignment) => None,
            Err(error) => panic!("fixture membership {choices:?}: {error}"),
        }
    });
    // Keep exhaustive coverage without retaining 2^n independently edited IRs.
    let first = pinned
        .next()
        .expect("fixture has a reachable source choice");
    std::iter::once(first).chain(pinned)
}
