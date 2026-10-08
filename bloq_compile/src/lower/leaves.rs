//! Expression leaves use one slot per occurrence (WF-11), even when several
//! occurrences read the same producer. The pinned proxy uses it for frame equations.

use bloq_ir::{Bloq, BloqEdge, BloqNodeId, ClassicalExpr};

/// Ordered expression inputs, wired after their consuming node is created.
#[derive(Default)]
pub(super) struct ExprLeaves {
    leaves: Vec<BloqNodeId>,
}

impl ExprLeaves {
    pub(super) fn input(&mut self, producer: BloqNodeId) -> ClassicalExpr {
        let slot = self.leaves.len();
        self.leaves.push(producer);
        ClassicalExpr::In(slot as u32)
    }

    pub(super) fn wire(self, bloq: &mut Bloq, node: BloqNodeId) {
        for (slot, producer) in self.leaves.into_iter().enumerate() {
            bloq.add_edge(producer, node, BloqEdge::value(slot as u32));
        }
    }
}
