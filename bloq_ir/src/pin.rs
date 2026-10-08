//! Pin source selectors while retaining the selected circuit and signed recipe.

use petgraph::visit::EdgeRef;
use std::collections::BTreeMap;
use thiserror::Error;

use crate::{
    Bloq, BloqEdge, BloqNodeId, BloqNodeKind, ClassicalExpr, ClassicalNode, CycleDetected, FxMap,
    FxSet, NodeProvenance, NodeTemplateInstanceMergeError, ObservableOutput, SubGraph, ValueRef,
};

/// Why source membership choices could not be pinned to a static realization.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MembershipPinError {
    /// Input dataflow needed for pinning is cyclic.
    #[error("{0}")]
    Cycle(#[from] CycleDetected),
    /// Membership registration is invalid.
    #[error("{0}")]
    InvalidMembership(#[from] NodeTemplateInstanceMergeError),
    /// A requested selector does not exist.
    #[error("unknown source branch selector `{0}`")]
    UnknownSelector(String),
    /// A source selector has no supplied choice.
    #[error("source branch selector `{0}` has no pin")]
    MissingSelector(String),
    /// A selector name is not unique.
    #[error("source branch selector `{0}` is not unique at the top level")]
    AmbiguousSelector(String),
    /// Supplied choices cannot jointly occur.
    #[error("source selector pins contradict their Boolean dataflow")]
    UnreachableAssignment,
}

impl Bloq {
    /// Pin named structural branches and selective-measurement outcomes.
    /// The returned program retains selected instance ids, record aliases,
    /// and signed flow recipes. Guards implied by the joint choices are removed;
    /// unresolved predicates and RUS retries retain their runtime semantics.
    ///
    /// Every source selector must be pinned, and the joint tuple must be
    /// consistent with its Boolean dataflow. `self` remains unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`MembershipPinError`] for cyclic dataflow, missing or ambiguous
    /// choices, contradictory assignments, or malformed membership data.
    /// Call [`Self::validate`] explicitly to audit all IR well-formedness rules.
    pub fn pin_membership(
        &self,
        choices: &BTreeMap<String, bool>,
    ) -> Result<Self, MembershipPinError> {
        // Constant folding needs a topological order even if no selector is
        // supplied. Other IR audits are independent of this transformation.
        for (_, level) in self.levels() {
            if !level.is_acyclic() {
                return Err(CycleDetected.into());
            }
        }
        let mut selectors = BTreeMap::new();
        for (path, level) in self.levels() {
            for (id, node) in level.nodes() {
                if let NodeProvenance::BranchSelector { name } = &node.provenance
                    && (!path.segments().is_empty() || selectors.insert(name.clone(), id).is_some())
                {
                    return Err(MembershipPinError::AmbiguousSelector(name.clone()));
                }
            }
        }
        for name in choices.keys() {
            if !selectors.contains_key(name) {
                return Err(MembershipPinError::UnknownSelector(name.clone()));
            }
        }
        let forced = selectors
            .iter()
            .map(|(name, &node)| {
                choices
                    .get(name)
                    .copied()
                    .map(|value| (node.into(), value))
                    .ok_or_else(|| MembershipPinError::MissingSelector(name.clone()))
            })
            .collect::<Result<FxMap<_, _>, _>>()?;
        let mut pinned = self.clone();
        pin_level(pinned.top_mut(), &forced)?;
        prune_unused_values(pinned.top_mut());
        Ok(pinned)
    }

    /// Whether quantum stages, seams, or quantum-containing region activations
    /// have conditional membership. Static emission and slicing require selection;
    /// timing and memory edits also support exhaustive terminal choices.
    /// Classical activation only gates a recipe; it does not change membership.
    pub fn has_conditional_membership(&self) -> bool {
        fn guarded(level: &SubGraph, under_activation: bool) -> bool {
            level.nodes().any(|(_, node)| match &node.kind {
                BloqNodeKind::Quantum(quantum) => under_activation || !quantum.guards.is_empty(),
                BloqNodeKind::Region(region) => region
                    .bodies()
                    .any(|(_, body)| guarded(body, under_activation || node.activation.is_some())),
                BloqNodeKind::Classical(_) => false,
            }) || level.edges().any(
                |edge| matches!(edge.edge, BloqEdge::Quantum(quantum) if quantum.guard.is_some()),
            )
        }
        guarded(self.top(), false)
    }
}

fn membership_input(
    level: &SubGraph,
    node: BloqNodeId,
    slot: u32,
) -> Result<ValueRef, MembershipPinError> {
    level
        .value_inputs(node)
        .find(|input| input.slot == slot)
        .map(|input| input.value_ref().expect("runtime input"))
        .ok_or(NodeTemplateInstanceMergeError::MissingMembershipInput(slot).into())
}

fn fixed_values(
    level: &SubGraph,
    forced: &FxMap<ValueRef, bool>,
) -> Result<FxMap<ValueRef, bool>, MembershipPinError> {
    let mut known = forced.clone();
    if !forced.is_empty() {
        let mut predicates = forced
            .iter()
            .map(|(&id, &bit)| (id, bit))
            .collect::<Vec<_>>();
        predicates.sort_unstable();
        // Only control consumers need backward implications. Lowering unrelated
        // frame/readout expressions can make a small selection domain enormous.
        let mut nodes = forced.keys().copied().collect::<FxSet<_>>();
        for (id, node) in level.nodes() {
            for input in level.value_inputs(id) {
                if node.activation == Some(input.slot)
                    || node.try_quantum().is_some_and(|quantum| {
                        quantum.guards.iter().any(|guard| guard.input == input.slot)
                    })
                {
                    nodes.insert(input.value_ref().expect("runtime input"));
                }
            }
        }
        nodes.extend(level.edges().filter_map(|edge| match edge.edge {
            BloqEdge::Quantum(quantum) => quantum.guard,
            _ => None,
        }));
        let mut nodes = nodes.into_iter().collect::<Vec<_>>();
        nodes.sort_unstable();
        let implied = crate::membership::PredicateAnalysis::new(level)
            .values_fixed_by_assignment(&predicates, nodes)?
            .ok_or(MembershipPinError::UnreachableAssignment)?;
        known.extend(implied);
    }
    Ok(known)
}

fn known_values(level: &SubGraph, forced: &FxMap<ValueRef, bool>) -> FxMap<ValueRef, bool> {
    let mut known = forced.clone();
    for node in level
        .deterministic_emit_order()
        .expect("validated acyclic graph")
    {
        if known.contains_key(&node.into()) {
            continue;
        }
        let active = level[node]
            .activation
            .map(|slot| {
                level
                    .value_inputs(node)
                    .find(|input| input.slot == slot)
                    .and_then(|input| {
                        known
                            .get(&input.value_ref().expect("runtime input"))
                            .copied()
                    })
            })
            .unwrap_or(Some(true));
        if active == Some(false) {
            known.insert(node.into(), false);
            if matches!(
                level[node].try_classical(),
                Some(ClassicalNode::Observable { index: Some(_), .. })
            ) {
                known.insert(
                    ValueRef {
                        node,
                        output: ObservableOutput::Flip,
                    },
                    false,
                );
            }
            continue;
        }
        if let Some(ClassicalNode::Compute { expr }) = level[node].try_classical() {
            let inputs = level
                .value_inputs(node)
                .map(|input| {
                    (
                        input.slot,
                        known
                            .get(&input.value_ref().expect("runtime input"))
                            .copied(),
                    )
                })
                .collect();
            let value = simplified(expr, &inputs).eval(&mut |_| None);
            if let Some(value) = value
                && (active == Some(true) || !value)
            {
                known.insert(node.into(), value);
            }
        }
    }
    known
}

fn pin_level(
    level: &mut SubGraph,
    forced: &FxMap<ValueRef, bool>,
) -> Result<(), MembershipPinError> {
    let fixed = fixed_values(level, forced)?;
    let known = known_values(level, &fixed);
    let mut removed_slots = FxMap::<BloqNodeId, FxSet<u32>>::default();
    let mut remove_all_inputs = FxSet::default();
    let mut pinned_observables = FxMap::default();
    for id in level.node_ids().collect::<Vec<_>>() {
        let mut node = level[id].clone();
        if let Some(quantum) = node.try_quantum() {
            let mut inputs = FxMap::default();
            for guard in &quantum.guards {
                let producer = membership_input(level, id, guard.input)?;
                if let Some(&value) = known.get(&producer) {
                    inputs.insert(guard.input, value);
                    removed_slots.entry(id).or_default().insert(guard.input);
                }
            }
            node = pin_quantum_members(&node, &inputs)?;
            for input in level.value_inputs(id) {
                if !node
                    .expect_quantum()
                    .guards
                    .iter()
                    .any(|guard| guard.input == input.slot)
                {
                    removed_slots.entry(id).or_default().insert(input.slot);
                }
            }
        }
        if let Some(slot) = node.activation {
            let producer = membership_input(level, id, slot)?;
            if let Some(&enabled) = known.get(&producer) {
                // Keep an indexed readout's false activation: both ports stay
                // false, its index remains valid, and no decoder solve occurs.
                if !enabled
                    && matches!(
                        node.try_classical(),
                        Some(ClassicalNode::Observable { index: Some(_), .. })
                    )
                {
                    let Some(ClassicalNode::Observable {
                        measurements,
                        operators,
                        ..
                    }) = node.try_classical_mut()
                    else {
                        unreachable!("indexed observable checked above")
                    };
                    measurements.clear();
                    operators.clear();
                    removed_slots
                        .entry(id)
                        .or_default()
                        .extend(level.data_inputs(id).map(|input| input.slot));
                    *level.node_mut(id).expect("live observable") = node;
                    continue;
                }
                node.activation = None;
                removed_slots.entry(id).or_default().insert(slot);
                if !enabled {
                    let replacement = match node.try_classical() {
                        Some(ClassicalNode::Observable { .. }) => ClassicalNode::fragment(),
                        Some(ClassicalNode::Discard { .. }) => ClassicalNode::Discard {
                            condition: ClassicalExpr::Const(false),
                        },
                        _ => ClassicalNode::Compute {
                            expr: ClassicalExpr::Const(false),
                        },
                    };
                    node.kind = BloqNodeKind::Classical(replacement.into());
                    remove_all_inputs.insert(id);
                }
            }
        }
        if let Some(&bit) = fixed.get(&id.into())
            && (forced.contains_key(&id.into())
                || matches!(node.try_classical(), Some(ClassicalNode::Compute { .. })))
        {
            // Pin only the corrected port of a queried observable. Its Flip
            // and composition consumers still need the authored recipe.
            if matches!(
                node.try_classical(),
                Some(ClassicalNode::Observable { index: Some(_), .. })
            ) {
                pinned_observables.insert(id, bit);
            } else {
                node.kind = BloqNodeKind::Classical(
                    ClassicalNode::Compute {
                        expr: ClassicalExpr::Const(bit),
                    }
                    .into(),
                );
                node.activation = None;
                remove_all_inputs.insert(id);
            }
        }
        if let BloqNodeKind::Region(region) = &mut node.kind {
            for (_, body) in region.bodies_mut() {
                pin_level(body, &FxMap::default())?;
            }
        }
        if matches!(level[id].kind, BloqNodeKind::Region(_)) && node.try_region().is_none() {
            let outputs = level
                .boundary_outputs()
                .iter()
                .copied()
                .filter(|&output| output != id)
                .collect();
            level.set_boundary_outputs(outputs);
        }
        *level.node_mut(id).expect("live node") = node;
    }
    let pinned_observables = pinned_observables
        .into_iter()
        .map(|(source, bit)| {
            let constant = level.add_node(crate::BloqNode::classical(ClassicalNode::Compute {
                expr: ClassicalExpr::Const(bit),
            }));
            (source, constant)
        })
        .collect::<FxMap<_, _>>();
    if let Some(mut result) = level.value_output()
        && result.output == ObservableOutput::Corrected
        && let Some(&constant) = pinned_observables.get(&result.node)
    {
        result.node = constant;
        level.set_value_output(Some(result));
    }
    for edge in level.graph().edge_indices().collect::<Vec<_>>() {
        let (source, target) = level.graph().edge_endpoints(edge).expect("live edge");
        if matches!(
            level.graph()[edge],
            BloqEdge::Value {
                output: ObservableOutput::Corrected,
                ..
            }
        ) && let Some(&constant) = pinned_observables.get(&BloqNodeId(source.index() as u32))
        {
            let payload = level.graph_mut().remove_edge(edge).expect("live edge");
            level.add_edge(constant, BloqNodeId(target.index() as u32), payload);
        }
    }
    let edges = level.graph().edge_indices().collect::<Vec<_>>();
    for id in edges {
        let edge = &level.graph()[id];
        let (_, target) = level.graph().edge_endpoints(id).expect("live edge");
        let target = BloqNodeId(target.index() as u32);
        let remove = match edge {
            BloqEdge::Value { slot, .. } | BloqEdge::Compose { slot, .. } => {
                remove_all_inputs.contains(&target)
                    || removed_slots
                        .get(&target)
                        .is_some_and(|slots| slots.contains(slot))
            }
            BloqEdge::Quantum(quantum) => quantum
                .guard
                .is_some_and(|guard| known.get(&guard) == Some(&false)),
            BloqEdge::Order => false,
        };
        if remove {
            level.graph_mut().remove_edge(id);
        } else if let BloqEdge::Quantum(quantum) = &mut level.graph_mut()[id]
            && quantum
                .guard
                .is_some_and(|guard| known.contains_key(&guard))
        {
            quantum.guard = None;
        }
    }
    let mut seams = FxMap::default();
    for id in level.graph().edge_indices().collect::<Vec<_>>() {
        let BloqEdge::Quantum(quantum) = &level.graph()[id] else {
            continue;
        };
        let endpoints = (
            level.graph().edge_endpoints(id).expect("live edge"),
            quantum.guard,
        );
        if let Some(&previous) = seams.get(&endpoints) {
            let BloqEdge::Quantum(removed) = level.graph_mut().remove_edge(id).expect("live seam")
            else {
                unreachable!("the collected seam edge was checked as quantum")
            };
            let BloqEdge::Quantum(kept) = &mut level.graph_mut()[previous] else {
                unreachable!("the retained seam edge was checked as quantum")
            };
            kept.pipes.extend(removed.pipes);
        } else {
            seams.insert(endpoints, id);
        }
    }
    fold_constants(level);
    Ok(())
}

fn fold_constants(level: &mut SubGraph) {
    let mut known: FxMap<ValueRef, bool> = FxMap::default();
    for id in level
        .deterministic_emit_order()
        .expect("pinning preserves acyclicity")
    {
        let inputs = level
            .value_inputs(id)
            .map(|input| {
                (
                    input.slot,
                    known
                        .get(&input.value_ref().expect("runtime input"))
                        .copied(),
                )
            })
            .collect::<FxMap<_, _>>();
        let activation = level[id].activation;
        let expr = match level[id].try_classical() {
            Some(ClassicalNode::Compute { expr } | ClassicalNode::Discard { condition: expr }) => {
                Some(simplified(expr, &inputs))
            }
            _ => None,
        };
        if let Some(expr) = expr {
            let mut used = FxSet::default();
            expr.for_each_input(&mut |slot| {
                used.insert(slot);
            });
            match level.node_mut(id).expect("live node").try_classical_mut() {
                Some(
                    ClassicalNode::Compute { expr: target }
                    | ClassicalNode::Discard { condition: target },
                ) => *target = expr,
                _ => unreachable!("the classical expression was checked above"),
            }
            used.extend(activation);
            let discard = level.graph().edges_directed(petgraph::stable_graph::NodeIndex::new(id.0 as usize), petgraph::Direction::Incoming)
                .filter(|edge| matches!(edge.weight(), BloqEdge::Value { slot, .. } if !used.contains(slot)))
                .map(|edge| edge.id()).collect::<Vec<_>>();
            for edge in discard {
                level.graph_mut().remove_edge(edge);
            }
        }
        if let Some(ClassicalNode::Compute {
            expr: ClassicalExpr::Const(value),
        }) = level[id].try_classical()
            && (!*value || activation.is_none_or(|slot| inputs.get(&slot) == Some(&Some(true))))
        {
            known.insert(id.into(), *value);
        }
    }
}

fn pin_quantum_members(
    node: &crate::BloqNode,
    inputs: &FxMap<u32, bool>,
) -> Result<crate::BloqNode, NodeTemplateInstanceMergeError> {
    if inputs.is_empty() {
        return Ok(node.clone());
    }
    // Pending registrations are renumbered below; reject bad indices before
    // indexing them, including registrations pinned false.
    node.select_quantum_members(|_| Some(false))?;
    let quantum = node.expect_quantum();
    let renumber = |count, kind: u8| {
        let removed = quantum
            .guards
            .iter()
            .filter(|guard| inputs.get(&guard.input) == Some(&false))
            .flat_map(|guard| match kind {
                0 => &guard.detectors,
                1 => &guard.detector_bundles,
                _ => &guard.restarts,
            })
            .copied()
            .collect::<FxSet<_>>();
        let mut next = 0;
        (0..count as u32)
            .map(|index| {
                if removed.contains(&index) {
                    None
                } else {
                    let index = next;
                    next += 1;
                    Some(index)
                }
            })
            .collect::<Vec<_>>()
    };
    let detectors = renumber(quantum.detectors.len(), 0);
    let detector_bundles = renumber(quantum.detector_bundles.len(), 1);
    let restarts = renumber(quantum.restarts.len(), 2);
    let mut pending = quantum
        .guards
        .iter()
        .filter(|guard| !inputs.contains_key(&guard.input))
        .cloned()
        .collect::<Vec<_>>();
    for guard in &mut pending {
        guard.detectors = guard
            .detectors
            .iter()
            .filter_map(|&index| detectors[index as usize])
            .collect();
        guard.detector_bundles = guard
            .detector_bundles
            .iter()
            .filter_map(|&index| detector_bundles[index as usize])
            .collect();
        guard.restarts = guard
            .restarts
            .iter()
            .filter_map(|&index| restarts[index as usize])
            .collect();
        guard.detector_parities = guard
            .detector_parities
            .iter()
            .filter_map(|(index, parity)| {
                detectors[*index as usize].map(|index| (index, parity.clone()))
            })
            .collect();
        guard.restart_parities = guard
            .restart_parities
            .iter()
            .filter_map(|(index, parity)| {
                restarts[*index as usize].map(|index| (index, parity.clone()))
            })
            .collect();
    }
    pending.retain(|guard| {
        !guard.instances.is_empty()
            || !guard.detectors.is_empty()
            || !guard.detector_bundles.is_empty()
            || !guard.restarts.is_empty()
            || !guard.detector_parities.is_empty()
            || !guard.restart_parities.is_empty()
    });
    let mut selected = node.clone();
    selected
        .expect_quantum_mut()
        .guards
        .retain(|guard| inputs.contains_key(&guard.input));
    let mut selected = selected.select_quantum_members(|slot| inputs.get(&slot).copied())?;
    selected.expect_quantum_mut().guards = pending;
    Ok(selected)
}

fn simplified(expr: &ClassicalExpr, values: &FxMap<u32, Option<bool>>) -> ClassicalExpr {
    use ClassicalExpr::{And, Const, In, Not, Or, Select, Xor};
    match expr {
        Const(_) => expr.clone(),
        ClassicalExpr::Parity { inputs, constant } => {
            let mut constant = *constant;
            let inputs = inputs
                .iter()
                .copied()
                .filter(|slot| {
                    if let Some(Some(value)) = values.get(slot) {
                        constant ^= value;
                        false
                    } else {
                        true
                    }
                })
                .collect::<Vec<_>>();
            ClassicalExpr::parity(inputs, constant)
        }
        In(slot) => values
            .get(slot)
            .copied()
            .flatten()
            .map(Const)
            .unwrap_or_else(|| expr.clone()),
        Not(inner) => match simplified(inner, values) {
            Const(value) => Const(!value),
            Not(inner) => *inner,
            inner => Not(Box::new(inner)),
        },
        And(operands) | Or(operands) | Xor(operands) => {
            let mut constant = matches!(expr, And(_));
            let mut remaining = Vec::with_capacity(operands.len());
            for operand in operands {
                match simplified(operand, values) {
                    Const(bit) => {
                        match expr {
                            And(_) => constant &= bit,
                            Or(_) => constant |= bit,
                            _ => constant ^= bit,
                        }
                        if matches!(expr, And(_)) && !constant || matches!(expr, Or(_)) && constant
                        {
                            return Const(constant);
                        }
                    }
                    operand => remaining.push(operand),
                }
            }
            if remaining.is_empty() {
                return Const(constant);
            }
            let value = if remaining.len() == 1 {
                remaining
                    .pop()
                    .expect("the single remaining operand was counted above")
            } else {
                match expr {
                    And(_) => And(remaining.into_boxed_slice()),
                    Or(_) => Or(remaining.into_boxed_slice()),
                    _ => Xor(remaining.into_boxed_slice()),
                }
            };
            if matches!(expr, Xor(_)) && constant {
                match value {
                    Not(inner) => *inner,
                    value => Not(Box::new(value)),
                }
            } else {
                value
            }
        }
        Select(operands) => {
            let [condition, when_false, when_true] = operands.as_ref();
            let condition = simplified(condition, values);
            // Pinning takes a Boolean cofactor, unlike strict runtime evaluation.
            if let Const(bit) = condition {
                return simplified(if bit { when_true } else { when_false }, values);
            }
            let when_false = simplified(when_false, values);
            let when_true = simplified(when_true, values);
            if when_false == when_true {
                when_false
            } else {
                ClassicalExpr::select(condition, when_false, when_true)
            }
        }
    }
}

fn prune_unused_values(level: &mut SubGraph) {
    let mut roots = level
        .edges()
        .filter_map(|edge| match edge.edge {
            BloqEdge::Quantum(quantum) => quantum.guard.map(|value| value.node),
            _ => None,
        })
        .collect::<FxSet<_>>();
    roots.extend(level.value_output().map(|value| value.node));
    roots.extend(level.boundary_outputs());
    for id in level
        .deterministic_emit_order()
        .expect("pinned graph is acyclic")
        .into_iter()
        .rev()
    {
        let node = &level[id];
        if roots.contains(&id)
            || matches!(
                node.provenance,
                NodeProvenance::OutputFrame { .. } | NodeProvenance::BranchSelector { .. }
            )
        {
            continue;
        }
        if matches!(
            node.try_classical(),
            Some(ClassicalNode::Compute { .. } | ClassicalNode::Observable { index: None, .. })
        ) && level.data_consumers(id).next().is_none()
        {
            level.remove_node(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::BloqValidationError;

    use super::*;

    #[test]
    fn pinning_an_observable_corrected_port_preserves_flip_and_recipe_consumers() {
        let program = Bloq::from_text(
            "BLOQIR 1
 graph {
   n0 observable 0 from selector choice
   n1 compute in0 from frame x (0,0,1)
   n2 compute in0 from frame z (0,0,1)
   n3 observable 1
   n0 -> n1 value 0
   n0 -> n2 value 0 flip
   n0 -> n3 compose 0
   result n0:flip
 }",
        )
        .unwrap();
        for bit in [false, true] {
            let pinned = program
                .pin_membership(&BTreeMap::from([("choice".to_owned(), bit)]))
                .unwrap();
            pinned.validate().unwrap();
            assert!(matches!(
                pinned[BloqNodeId(0)].try_classical(),
                Some(ClassicalNode::Observable { index: Some(0), .. })
            ));
            assert_eq!(
                pinned.classical_value(BloqNodeId(1), crate::ClassicalAssignment::Uniform(!bit)),
                Ok(bit)
            );
            assert_eq!(
                pinned.top().value_output(),
                Some(ValueRef {
                    node: BloqNodeId(0),
                    output: ObservableOutput::Flip
                })
            );
            assert_eq!(
                pinned
                    .resolve_classical(BloqNodeId(2), crate::ClassicalAssignment::Uniform(false))
                    .unwrap()
                    .decoder_observables,
                std::collections::BTreeSet::from([0])
            );
            assert_eq!(
                pinned
                    .resolve_classical(BloqNodeId(3), crate::ClassicalAssignment::Uniform(false))
                    .unwrap()
                    .decoder_observables,
                std::collections::BTreeSet::from([1])
            );
        }
    }

    #[test]
    fn pinning_keeps_an_inactive_indexed_observables_ports_false() {
        let program = Bloq::from_text(
            "BLOQIR 1
 template t0 {
   circuit {
     M (0,0):m0
   }
 }
 graph {
   n0 compute 0
   n1 observable 0 measurements i0:m0 when v0
   n2 compute in0
   n3 quantum {
     instance i0 t0 @ (0,0)
     guard 0 i0
   }
   n0 -> n3 value 0
   n3 -> n1 order
   n1 -> n2 value 0 flip
   n0 -> n1 value 0
   result n1:flip
 }",
        )
        .unwrap();
        let pinned = program.pin_membership(&BTreeMap::new()).unwrap();
        pinned.validate().unwrap();
        assert_eq!(
            pinned.top().value_output(),
            Some(ValueRef {
                node: BloqNodeId(1),
                output: ObservableOutput::Flip
            })
        );
        for output in [ObservableOutput::Corrected, ObservableOutput::Flip] {
            let value = ValueRef {
                node: BloqNodeId(1),
                output,
            };
            assert_eq!(
                pinned.classical_ref_value(value, crate::ClassicalAssignment::Uniform(true)),
                Ok(false)
            );
            let resolution = pinned
                .resolve_value(value, crate::ClassicalAssignment::Uniform(true))
                .unwrap();
            assert!(resolution.measurements.is_empty());
            assert!(resolution.decoder_observables.is_empty());
        }
    }

    #[test]
    fn cyclic_pinning_retains_the_causal_error() {
        let mut program = Bloq::new();
        let node = program.add_node(crate::BloqNode::classical(ClassicalNode::Compute {
            expr: ClassicalExpr::Const(true),
        }));
        program.add_edge(node, node, BloqEdge::Order);

        let error = program.pin_membership(&BTreeMap::new()).unwrap_err();
        assert!(matches!(error, MembershipPinError::Cycle(CycleDetected)));
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .is::<CycleDetected>()
        );
    }

    #[test]
    fn partial_pin_renumbers_pending_bundle_only_registration() {
        use crate::{BloqNode, DetectorBundleId, DetectorBundleUse, QuantumGuard};
        let mut node = BloqNode::from_members(Vec::new());
        node.expect_quantum_mut().detector_bundles = (0..2)
            .map(|index| DetectorBundleUse {
                bundle: DetectorBundleId(index),
                instances: Vec::new(),
                offset: glam::IVec2::ZERO,
            })
            .collect();
        node.expect_quantum_mut().guards = vec![
            QuantumGuard {
                input: 0,
                detector_bundles: vec![0],
                ..Default::default()
            },
            QuantumGuard {
                input: 1,
                detector_bundles: vec![1],
                ..Default::default()
            },
        ];
        let pinned = pin_quantum_members(&node, &FxMap::from_iter([(0, false)])).unwrap();
        let quantum = pinned.expect_quantum();
        assert_eq!(quantum.detector_bundles[0].bundle, DetectorBundleId(1));
        assert_eq!(quantum.guards[0].detector_bundles, vec![0]);
        assert_eq!(quantum.guards[0].input, 1);
    }

    #[test]
    fn pinning_does_not_audit_unrelated_observable_indices() {
        let mut program = Bloq::new();
        for _ in 0..2 {
            program.add_node(crate::BloqNode::classical(ClassicalNode::observable(0)));
        }
        assert!(matches!(
            program.validate(),
            Err(BloqValidationError::DuplicateObservableIndex { .. })
        ));
        let pinned = program.pin_membership(&BTreeMap::new()).unwrap();
        assert_eq!(pinned.nodes().count(), 2);
    }

    #[test]
    fn missing_activation_input_is_a_pinning_error() {
        let mut program = Bloq::new();
        let id = program.add_node(crate::BloqNode::classical(ClassicalNode::Compute {
            expr: ClassicalExpr::Const(true),
        }));
        program.node_mut(id).unwrap().activation = Some(0);
        assert!(matches!(
            program.pin_membership(&BTreeMap::new()),
            Err(MembershipPinError::InvalidMembership(
                NodeTemplateInstanceMergeError::MissingMembershipInput(0)
            ))
        ));
    }

    #[test]
    fn compound_alias_pins_fix_shared_guards_without_guessing_other_outcomes() {
        let program = Bloq::from_text(
            "BLOQIR 1
template t0 {
 circuit {
  M (0,0):m0
  M (1,0):m1
  M (2,0):m2
 }
}
template t1 {
 circuit {
  M (0,0):m0
 }
}
graph {
 n0 quantum {
  instance i0 t0 @ (0,0)
 }
 n1 observable fragment measurements i0:m0
 n2 observable fragment measurements i0:m1
 n3 observable fragment measurements i0:m2
 n4 compute in0 & in1
 n5 compute in0 from selector both
 n6 compute !in0 from selector not_both
 n7 compute !(in0 & in1)
 n8 compute in0 & in1 & in2
 n9 quantum {
  instance i1 t1 @ (3,0)
  instance i2 t1 @ (3,0)
  guard 0 i1
  guard 1 i2
 }
 n10 quantum {
  instance i3 t1 @ (4,0)
  guard 0 i3
 }
 n11 observable fragment measurements i1:m0 when v0
 n12 observable fragment measurements i2:m0 when v0
 n13 observable 0
 n14 compute in0 ^ in1
 n15 compute 0 when v0
 n16 compute in0 & in1
 n0 -> n1 order
 n0 -> n2 order
 n0 -> n3 order
 n1 -> n4 value 0
 n2 -> n4 value 1
 n4 -> n5 value 0
 n4 -> n6 value 0
 n1 -> n7 value 0
 n2 -> n7 value 1
 n1 -> n8 value 0
 n2 -> n8 value 1
 n3 -> n8 value 2
 n4 -> n9 value 0
 n7 -> n9 value 1
 n8 -> n10 value 0
 n4 -> n11 value 0
 n7 -> n12 value 0
 n9 -> n11 order
 n9 -> n12 order
 n11 -> n13 compose 0
 n12 -> n13 compose 1
 n3 -> n14 value 0
 n3 -> n14 value 1
 n3 -> n15 value 0
 n15 -> n13 value 2
 n1 -> n16 value 0
 n2 -> n16 value 1
 n16 -> n13 value 3
 result n14
}",
        )
        .unwrap();
        let original = program.to_binary();
        for both in [false, true] {
            let pinned = program
                .pin_membership(&BTreeMap::from([
                    ("both".to_owned(), both),
                    ("not_both".to_owned(), !both),
                ]))
                .unwrap();
            let selected = pinned[BloqNodeId(9)].expect_quantum();
            assert!(selected.guards.is_empty());
            assert_eq!(selected.instances.len(), 1);
            assert_eq!(selected.instances[0].id.0, if both { 1 } else { 2 });
            let read = BloqNodeId(if both { 11 } else { 12 });
            assert_eq!(pinned[read].try_classical(), program[read].try_classical());
            assert_eq!(pinned[read].activation, None);
            let masked = pinned[BloqNodeId(10)].expect_quantum();
            assert_eq!(masked.instances.len(), usize::from(both));
            assert_eq!(masked.guards.len(), usize::from(both));
            assert_eq!(
                pinned[BloqNodeId(3)].try_classical(),
                program[BloqNodeId(3)].try_classical()
            );
            assert_eq!(
                pinned[BloqNodeId(14)].try_classical(),
                program[BloqNodeId(14)].try_classical()
            );
            assert_eq!(pinned.value_inputs(BloqNodeId(14)).count(), 2);
            assert_eq!(pinned[BloqNodeId(15)].activation, Some(0));
            assert_eq!(
                pinned[BloqNodeId(16)].try_classical(),
                program[BloqNodeId(16)].try_classical(),
                "unrelated readout recipes retain their measured inputs"
            );
            let Some(ClassicalNode::Compute { expr }) = pinned[BloqNodeId(14)].try_classical()
            else {
                unreachable!()
            };
            assert_eq!(expr.eval(&mut |_| None), None);
        }
        assert!(matches!(
            program.pin_membership(&BTreeMap::from([
                ("both".to_owned(), true),
                ("not_both".to_owned(), true),
            ])),
            Err(MembershipPinError::UnreachableAssignment)
        ));
        assert_eq!(program.to_binary(), original);
    }

    #[test]
    fn pinning_activated_selector_keeps_its_value_and_shares_unchanged_quantum_data() {
        let program = Bloq::from_text(
            "BLOQIR 1
template t0 {
 circuit {
  M (0,0):m0
 }
}
graph {
 n0 quantum {
  instance i0 t0 @ (0,0)
 }
 n1 observable fragment measurements i0:m0
 n2 compute 1 when v0 from selector choice
 n3 quantum {
  instance i1 t0 @ (1,0)
  guard 0 i1
 }
 n4 observable fragment measurements i1:m0 when v0
 n0 -> n1 order
 n1 -> n2 value 0
 n2 -> n3 value 0
 n2 -> n4 value 0
 n3 -> n4 order
 result n4
}",
        )
        .unwrap();
        let original = program.to_binary();
        for bit in [false, true] {
            let pinned = program
                .pin_membership(&BTreeMap::from([("choice".to_owned(), bit)]))
                .unwrap();
            assert_eq!(pinned[BloqNodeId(2)].activation, None);
            assert!(pinned.value_inputs(BloqNodeId(2)).next().is_none());
            assert_eq!(
                pinned[BloqNodeId(2)].try_classical(),
                Some(&ClassicalNode::Compute {
                    expr: ClassicalExpr::Const(bit),
                })
            );
            assert!(std::ptr::eq(
                program[BloqNodeId(0)].expect_quantum(),
                pinned[BloqNodeId(0)].expect_quantum(),
            ));
            assert!(!pinned.has_conditional_membership());
            assert_eq!(
                pinned[BloqNodeId(3)].expect_quantum().instances.len(),
                usize::from(bit),
            );
            assert_eq!(pinned[BloqNodeId(4)].activation, None);
            if bit {
                assert!(matches!(
                    pinned[BloqNodeId(4)].try_classical(),
                    Some(ClassicalNode::Observable { index: None, .. })
                ));
            } else {
                assert_eq!(
                    pinned[BloqNodeId(4)].try_classical(),
                    Some(&ClassicalNode::Observable {
                        index: None,
                        measurements: Vec::new(),
                        operators: Vec::new(),
                    })
                );
            }
        }
        assert_eq!(program.to_binary(), original);

        let mut inactive = program.clone();
        inactive.node_mut(BloqNodeId(1)).unwrap().kind = BloqNodeKind::Classical(
            ClassicalNode::Compute {
                expr: ClassicalExpr::Const(false),
            }
            .into(),
        );
        inactive
            .pin_membership(&BTreeMap::from([("choice".to_owned(), false)]))
            .unwrap();
        assert!(matches!(
            inactive.pin_membership(&BTreeMap::from([("choice".to_owned(), true)])),
            Err(MembershipPinError::UnreachableAssignment)
        ));
    }

    #[test]
    fn disabled_region_keeps_false_result_and_clears_boundary_exports() {
        let program = Bloq::from_text(
            "BLOQIR 1
graph {
 n0 compute 0
 n1 rus 0 when v0 {
   body {
     n0 observable fragment
     bindings n0
   }
 }
 n0 -> n1 value 0
 result n1
 bindings n1
}",
        )
        .unwrap();
        let pinned = program.pin_membership(&BTreeMap::new()).unwrap();
        assert_eq!(pinned.value_output(), Some(BloqNodeId(1).into()));
        assert!(pinned.boundary_outputs().is_empty());
        assert!(matches!(
            pinned[BloqNodeId(1)].try_classical(),
            Some(ClassicalNode::Compute {
                expr: ClassicalExpr::Const(false)
            })
        ));
        assert_eq!(program.boundary_outputs(), [BloqNodeId(1)]);
    }

    #[test]
    fn expression_cofactors_preserve_every_completion_of_partial_pins() {
        use ClassicalExpr::{And, Const, In, Not, Or, Xor};
        for expr in [
            ClassicalExpr::parity([0, 1, 2], true),
            ClassicalExpr::parity([0, 0, 2], false),
            Xor(Box::new([In(0), In(1), In(2), Const(true)])),
            And(Box::new([In(0), In(1), In(2)])),
            Or(Box::new([In(0), In(1), In(2)])),
            ClassicalExpr::select(In(0), In(1), In(2)),
            ClassicalExpr::select(
                Not(Box::new(In(0))),
                Xor(Box::new([In(1), In(2)])),
                And(Box::new([In(1), In(2)])),
            ),
        ] {
            for encoded in 0..27 {
                let values = (0..3)
                    .map(|slot| {
                        (
                            slot,
                            [None, Some(false), Some(true)][encoded / 3usize.pow(slot) % 3],
                        )
                    })
                    .collect::<FxMap<_, _>>();
                let cofactor = simplified(&expr, &values);
                for bits in 0..8 {
                    if values.iter().any(|(&slot, value)| {
                        value.is_some_and(|value| value != (bits & (1 << slot) != 0))
                    }) {
                        continue;
                    }
                    assert_eq!(
                        cofactor.eval(&mut |slot| Some(bits & (1 << slot) != 0)),
                        expr.eval(&mut |slot| Some(bits & (1 << slot) != 0)),
                        "{expr:?}, {values:?}, {bits}"
                    );
                }
                if values.values().all(Option::is_some) {
                    assert!(
                        matches!(cofactor, Const(_)),
                        "fully pinned expression must become a constant"
                    );
                }
            }
        }
    }
}
