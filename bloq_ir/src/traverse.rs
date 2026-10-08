//! Recursive traversal over every nesting level of a [`Bloq`] — the primary
//! way to read a compiled program's node graph.
//!
//! # The IR shape
//!
//! A [`Bloq`] is a *nested* graph. Its top level is a [`SubGraph`]; a
//! [`crate::RegionNode`] (`RepeatUntilSuccess`)
//! owns one nested body, and that body is itself a full [`SubGraph`]
//! with the same read surface as the top. So "the program" is a tree of graph
//! levels, and every quantum, classical, or region node lives at exactly one
//! level.
//!
//! A [`crate::BloqNodeId`] is only unique *within* its level: the same
//! numeric id can name a top-level node and a node inside a
//! region body. The owner half of a node's identity is its [`LevelPath`] — the
//! chain of enclosing `(region node, body)` hops from the top level down. A
//! `(LevelPath, BloqNodeId)` pair is therefore a program-unique node address,
//! the analogue of rustc's `OwnerId`/`ItemLocalId` split.
//!
//! # Choosing a traversal
//!
//! - [`Bloq::walk`] is the MLIR-style pre-order walk: it visits *individual
//!   nodes*, top-down, and the callback returns a [`WalkControl`] to keep
//!   going, prune a node's region bodies, or stop the whole walk early. Reach
//!   for it when you want to inspect nodes and possibly skip subtrees.
//! - [`Bloq::levels`] yields whole graph *levels* as `(LevelPath, &SubGraph)`.
//!   Reach for it for table passes — instance collection, id allocation,
//!   per-level edge queries — where you read a level as a unit rather than
//!   reacting to each node.
//!
//! # Ordering
//!
//! Both traversals visit a level's nodes in **ascending [`crate::BloqNodeId`]
//! order** — the deterministic validation/scan order, *not* the execution
//! schedule. When you need the order a backend would emit in, call
//! [`SubGraph::deterministic_emit_order`] on each level yourself; scan order
//! and schedule order differ whenever an edge runs against ascending id order.

use crate::{Bloq, BloqNode, BloqNodeId, BloqNodeKind, BodySelector, SubGraph};

/// One hop of a [`LevelPath`]: the region descended through and its selected
/// body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct LevelSegment {
    /// The enclosing region node's id in its parent level.
    pub region: BloqNodeId,
    /// The selected body of the region.
    pub body: BodySelector,
}

/// Path from the top level to one graph level: the chain of enclosing region
/// body hops. Empty = the top level.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct LevelPath {
    segments: Vec<LevelSegment>,
}

impl LevelPath {
    /// The top-level ancestor of a node at this level: the outermost
    /// enclosing region for a nested level, else `local` itself.
    ///
    /// Because ids are level-local, a node's own `local` id
    /// is only a valid key for [`Bloq::top`]/[`SubGraph::node`] when the node
    /// *is* at the top level. For a node inside a region body, this returns the
    /// id of the outermost enclosing region — the one node on the walk from the
    /// top level down to it that a top-level lookup can resolve. Passing a
    /// body-local `local` id straight to a top-level lookup would silently hit
    /// the wrong node (or none); route it through here first.
    pub fn top_level_ancestor(&self, local: BloqNodeId) -> BloqNodeId {
        self.segments
            .first()
            .map_or(local, |segment| segment.region)
    }

    /// The region-body hops from the top level down, outermost first.
    #[must_use]
    pub fn segments(&self) -> &[LevelSegment] {
        &self.segments
    }

    /// Whether this path identifies the top level.
    #[must_use]
    pub fn is_top_level(&self) -> bool {
        self.segments.is_empty()
    }

    /// Whether this path is `prefix` or lies inside it.
    #[must_use]
    pub fn starts_with(&self, prefix: &Self) -> bool {
        self.segments.starts_with(&prefix.segments)
    }

    /// This path extended by descending into `region`'s `body`.
    #[must_use]
    pub fn child(&self, region: BloqNodeId, body: BodySelector) -> Self {
        let mut segments = self.segments.clone();
        segments.push(LevelSegment { region, body });
        Self { segments }
    }
}

impl BodySelector {
    /// The stable name of this body selector: `"body"`.
    pub fn name(&self) -> &'static str {
        match self {
            BodySelector::Body => "body",
        }
    }

    /// The inverse of [`BodySelector::name`], so a caller holding the stable
    /// name (a serialized path, a binding-layer argument) can name a selector
    /// without reconstructing one by scanning the program.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "body" => Some(BodySelector::Body),
            _ => None,
        }
    }
}

/// Flow control returned from a [`Bloq::walk`] callback (MLIR `WalkResult`):
/// whether to descend, prune, or stop after the current node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkControl {
    /// Keep walking; descend into this node's region bodies.
    Continue,
    /// Keep walking, but do not descend into this node's region bodies.
    SkipBodies,
    /// Abort the whole walk.
    Stop,
}

/// Context for one node visited by [`Bloq::walk`].
///
/// This carries the program (for templates), the node's own graph level (for edge queries),
/// the level's path (for global identity), and the node itself.
///
/// `id` is level-local, so `(path, id)` together are the node's program-unique
/// address; `id` alone only keys into the top level (see
/// [`LevelPath::top_level_ancestor`]). Use `level` to run edge queries — e.g.
/// [`level.incoming(id)`](SubGraph::incoming) — against the graph the node
/// actually lives in.
#[derive(Debug, Clone, Copy)]
pub struct NodeCx<'a> {
    /// The whole program, e.g. for resolving [`crate::TemplateId`]s.
    pub bloq: &'a Bloq,
    /// The graph level this node lives at — the target for edge queries.
    pub level: &'a SubGraph,
    /// The owner path to `level`; empty at the top level.
    pub path: &'a LevelPath,
    /// The node's level-local id (unique only within `level`).
    pub id: BloqNodeId,
    /// The visited node.
    pub node: &'a BloqNode,
}

impl Bloq {
    /// Pre-order depth-first walk over every node at every nesting level, in
    /// ascending node-id order within a level (the validation/scan order —
    /// use [`SubGraph::deterministic_emit_order`] per level when you need the
    /// schedule). Returns [`WalkControl::Stop`] if the callback stopped the
    /// walk, else [`WalkControl::Continue`].
    ///
    /// The callback receives a [`NodeCx`] for each visited node and returns a
    /// [`WalkControl`]: [`Continue`](WalkControl::Continue) to descend into the
    /// node's region bodies (if any), [`SkipBodies`](WalkControl::SkipBodies)
    /// to visit the node but prune its bodies, or [`Stop`](WalkControl::Stop)
    /// to abort the whole walk.
    ///
    /// # Examples
    ///
    /// In real code a [`Bloq`] comes from the compiler (`bloq_compile`), but
    /// that crate depends on this one, so — to keep this doctest free of a
    /// dependency cycle — we parse an equivalent program from the `.bloqir`
    /// text format instead. It is a single quantum node feeding a
    /// `RepeatUntilSuccess` region (whose body holds another quantum node and
    /// an observable fragment), then a terminal observable — the shape a magic-state
    /// (`T`) block lowers to.
    ///
    /// ```
    /// use bloq_ir::{Bloq, BloqNodeKind, WalkControl};
    ///
    /// let program = Bloq::from_text(
    ///     "\
    /// BLOQIR 1
    ///
    /// template t0 {
    ///   circuit {
    ///     MPP X(0,0):m0
    ///   }
    /// }
    ///
    /// graph {
    ///   n0 quantum {
    ///     instance i0 t0 @ (0,0)
    ///   }
    ///   n1 rus in0 source n0 {
    ///     body {
    ///       n0 quantum {
    ///         instance i1 t0 @ (2,0)
    ///       }
    ///       n1 observable fragment measurements i1:m0 from generator 0
    ///       n0 -> n1 order
    ///     }
    ///   }
    ///   n2 observable 0
    ///   n0 -> n1 order
    ///   n1 -> n2 order
    /// }
    /// ",
    /// )
    /// .expect("valid .bloqir text");
    ///
    /// // A full walk visits every node at every level, matching on kind.
    /// let (mut quantum, mut classical, mut region) = (0, 0, 0);
    /// program.walk(|cx| {
    ///     match cx.node.kind {
    ///         BloqNodeKind::Quantum(_) => quantum += 1,
    ///         BloqNodeKind::Classical(_) => classical += 1,
    ///         BloqNodeKind::Region(_) => region += 1,
    ///     }
    ///     WalkControl::Continue
    /// });
    /// // Two quantum (top-level + in-body), two classical (in-body fragment +
    /// // terminal observable), one region.
    /// assert_eq!((quantum, classical, region), (2, 2, 1));
    ///
    /// // `SkipBodies` prunes region bodies: the two in-body nodes are skipped,
    /// // leaving only the three top-level nodes.
    /// let mut top_level = 0;
    /// program.walk(|cx| {
    ///     top_level += 1;
    ///     match cx.node.kind {
    ///         BloqNodeKind::Region(_) => WalkControl::SkipBodies,
    ///         _ => WalkControl::Continue,
    ///     }
    /// });
    /// assert_eq!(top_level, 3);
    ///
    /// // `Stop` aborts early. Here we halt at the first region and report it.
    /// let mut region_path = None;
    /// let control = program.walk(|cx| {
    ///     if matches!(cx.node.kind, BloqNodeKind::Region(_)) {
    ///         region_path = Some(cx.path.clone());
    ///         WalkControl::Stop
    ///     } else {
    ///         WalkControl::Continue
    ///     }
    /// });
    /// assert_eq!(control, WalkControl::Stop);
    /// // The region is at the top level, so its owner path is empty.
    /// assert!(region_path.expect("stopped at the region").segments().is_empty());
    /// ```
    pub fn walk(&self, mut f: impl FnMut(NodeCx<'_>) -> WalkControl) -> WalkControl {
        walk_level(self, self.top(), &mut LevelPath::default(), &mut f)
    }

    /// Every graph level, pre-order top-down: the top level, then each region
    /// body. For whole-program table passes (instance collection, id
    /// allocation) that read levels rather than individual nodes.
    ///
    /// Each item is `(LevelPath, &SubGraph)`: the path identifies the level
    /// (empty for the top level), and the [`SubGraph`] exposes the level's full
    /// read surface — [`nodes`](SubGraph::nodes), [`edges`](SubGraph::edges),
    /// [`deterministic_emit_order`](SubGraph::deterministic_emit_order), etc.
    ///
    /// # Examples
    ///
    /// A per-level table pass that sums quantum nodes across the whole program
    /// (see [`Bloq::walk`] for the `.bloqir` program used here):
    ///
    /// ```
    /// use bloq_ir::{Bloq, BloqNodeKind};
    /// # let program = Bloq::from_text(
    /// #     "\
    /// # BLOQIR 1
    /// #
    /// # template t0 {
    /// #   circuit {
    /// #     MPP X(0,0):m0
    /// #   }
    /// # }
    /// #
    /// # graph {
    /// #   n0 quantum {
    /// #     instance i0 t0 @ (0,0)
    /// #   }
    /// #   n1 rus in0 source n0 {
    /// #     body {
    /// #       n0 quantum {
    /// #         instance i1 t0 @ (2,0)
    /// #       }
    /// #       n1 observable fragment measurements i1:m0 from generator 0
    /// #       n0 -> n1 order
    /// #     }
    /// #   }
    /// #   n2 observable 0
    /// #   n0 -> n1 order
    /// #   n1 -> n2 order
    /// # }
    /// # ",
    /// # )
    /// # .expect("valid .bloqir text");
    ///
    /// let mut level_count = 0;
    /// let mut quantum_total = 0;
    /// for (path, level) in program.levels() {
    ///     level_count += 1;
    ///     let quantum = level
    ///         .nodes()
    ///         .filter(|(_, node)| matches!(node.kind, BloqNodeKind::Quantum(_)))
    ///         .count();
    ///     quantum_total += quantum;
    ///     // `path` is the level's identity: empty at the top, one hop inside
    ///     // the region body.
    ///     assert!(path.segments().len() <= 1);
    /// }
    /// // Two levels (top + the region body), one quantum node in each.
    /// assert_eq!(level_count, 2);
    /// assert_eq!(quantum_total, 2);
    /// ```
    pub fn levels(&self) -> impl Iterator<Item = (LevelPath, &SubGraph)> {
        let mut levels = Vec::new();
        collect_levels(self.top(), &mut LevelPath::default(), &mut levels);
        levels.into_iter()
    }

    /// The graph level identified by `path`, or `None` if a hop is stale or
    /// names the wrong region body.
    #[must_use]
    pub fn level_at(&self, path: &LevelPath) -> Option<&SubGraph> {
        level_at_segments(self.top(), path.segments())
    }

    pub(crate) fn level_at_segments(&self, segments: &[LevelSegment]) -> Option<&SubGraph> {
        level_at_segments(self.top(), segments)
    }

    pub(crate) fn level_at_mut(&mut self, path: &LevelPath) -> Option<&mut SubGraph> {
        level_at_segments_mut(self.top_mut(), path.segments())
    }

    pub(crate) fn level_at_segments_mut(
        &mut self,
        segments: &[LevelSegment],
    ) -> Option<&mut SubGraph> {
        level_at_segments_mut(self.top_mut(), segments)
    }
}

fn level_at_segments<'a>(level: &'a SubGraph, segments: &[LevelSegment]) -> Option<&'a SubGraph> {
    let Some((&LevelSegment { region, body }, rest)) = segments.split_first() else {
        return Some(level);
    };
    let body = level
        .node(region)?
        .try_region()?
        .bodies()
        .find_map(|(selector, candidate)| (selector == body).then_some(candidate))?;
    level_at_segments(body, rest)
}

fn level_at_segments_mut<'a>(
    level: &'a mut SubGraph,
    segments: &[LevelSegment],
) -> Option<&'a mut SubGraph> {
    let Some((&LevelSegment { region, body }, rest)) = segments.split_first() else {
        return Some(level);
    };
    let BloqNodeKind::Region(region) = &mut level.node_mut(region)?.kind else {
        return None;
    };
    let body = region
        .bodies_mut()
        .find_map(|(selector, candidate)| (selector == body).then_some(candidate))?;
    level_at_segments_mut(body, rest)
}

fn walk_level(
    bloq: &Bloq,
    level: &SubGraph,
    path: &mut LevelPath,
    f: &mut impl FnMut(NodeCx<'_>) -> WalkControl,
) -> WalkControl {
    for (id, node) in level.nodes() {
        match f(NodeCx {
            bloq,
            level,
            path,
            id,
            node,
        }) {
            WalkControl::Stop => return WalkControl::Stop,
            WalkControl::SkipBodies => continue,
            WalkControl::Continue => {}
        }
        let Some(region) = node.try_region() else {
            continue;
        };
        for (selector, body) in region.bodies() {
            path.segments.push(LevelSegment {
                region: id,
                body: selector,
            });
            let control = walk_level(bloq, body, path, f);
            path.segments.pop();
            if control == WalkControl::Stop {
                return WalkControl::Stop;
            }
        }
    }
    WalkControl::Continue
}

fn collect_levels<'a>(
    level: &'a SubGraph,
    path: &mut LevelPath,
    out: &mut Vec<(LevelPath, &'a SubGraph)>,
) {
    out.push((path.clone(), level));
    for (id, node) in level.nodes() {
        let Some(region) = node.try_region() else {
            continue;
        };
        for (selector, body) in region.bodies() {
            path.segments.push(LevelSegment {
                region: id,
                body: selector,
            });
            collect_levels(body, path, out);
            path.segments.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_path_json_preserves_scope_schema() {
        let path = LevelPath::default().child(BloqNodeId(7), BodySelector::Body);
        assert_eq!(
            serde_json::to_value(path).unwrap(),
            serde_json::json!({ "segments": [{ "region": 7, "body": "Body" }] })
        );
    }
}
