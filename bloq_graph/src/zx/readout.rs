//! Freeze causal physical readouts from a composed signed correlation space.

use std::collections::{BTreeMap, HashSet};

use glam::IVec3;

use super::stabilizer::SearchBudget;
use super::{
    DerivedSurface, NodeKind, RuntimeBasisError, RuntimeStabilizerBasis, Stabilizer,
    StabilizerError, StabilizerGenerators, ZXGraph,
};
use crate::{Action, ModuleCertificationLimits, Pauli};

/// Coordinates carried by a selected readout's coefficient ordinals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadoutCoordinates {
    /// The stable public generator presentation, including this readout itself.
    PublicRows,
    /// Earlier corrected source outcomes, XORed with the physical surface.
    SourceOutcomes,
}

/// One frozen named readout, indexed by its already available resolve choices.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadoutPlan {
    /// Interpretation of the surface's coefficient ordinals.
    pub coordinates: ReadoutCoordinates,
    /// Resolve sites whose values select this recipe's surface.
    pub sites: Vec<IVec3>,
    /// Reachable assignments and their signed physical parity. Interpret the
    /// surface's readout ordinals according to `coordinates`.
    // ponytail: O(branches × support) materialized cache; pack signed rows if memory dominates.
    pub branches: Vec<(Vec<bool>, DerivedSurface)>,
    /// Certified cut by which every selected physical parity is available.
    pub deadline: i64,
}

impl ReadoutPlan {
    /// Selects the certified surface for one reachable resolve assignment.
    ///
    /// # Panics
    /// Panics if the supplied values are not a jointly reachable assignment.
    pub fn select(&self, mut value: impl FnMut(IVec3) -> bool) -> &DerivedSurface {
        let values = self
            .sites
            .iter()
            .map(|&site| value(site))
            .collect::<Vec<_>>();
        &self
            .branches
            .iter()
            .find(|(branch, _)| *branch == values)
            .expect("reachable assignment has a certified readout")
            .1
    }
}

/// Executable named-readout recipe. Fixed rows borrow their existing storage.
#[derive(Debug, Clone, Copy)]
pub enum NamedReadout<'a> {
    /// The authored physical records need no selective choices.
    Fixed(&'a Stabilizer),
    /// The selected family owns the physical records and their coordinate map.
    Guarded(&'a ReadoutPlan),
}

impl<'a> NamedReadout<'a> {
    /// Physical surface selected by already available resolve values.
    pub fn select(self, value: impl FnMut(IVec3) -> bool) -> &'a Stabilizer {
        match self {
            Self::Fixed(surface) => surface,
            Self::Guarded(recipe) => &recipe.select(value).stabilizer,
        }
    }

    /// All certified physical surfaces, without expanding predecessor readouts.
    pub fn surfaces(self) -> impl Iterator<Item = &'a Stabilizer> {
        let (fixed, branches) = match self {
            Self::Fixed(surface) => (Some(surface), &[][..]),
            Self::Guarded(recipe) => (None, recipe.branches.as_slice()),
        };
        fixed
            .into_iter()
            .chain(branches.iter().map(|(_, surface)| &surface.stabilizer))
    }
}

/// Graph analysis may retain output-supported rows; compilation requires C0.
#[derive(Clone, Copy)]
pub(crate) enum ReadoutClosure {
    Required,
    #[cfg(feature = "verify")]
    Analysis,
}

/// Prepare ordinary readouts from their certified causal action cuts.
pub(crate) fn plan_causal_readouts(
    generators: &StabilizerGenerators,
    dag: &crate::ActionDag,
    limits: ModuleCertificationLimits,
    closure: ReadoutClosure,
) -> Result<BTreeMap<String, ReadoutPlan>, RuntimeBasisError> {
    let limit = limits.max_guarded_domain_size;
    let mut budget = SearchBudget::with_limits(limits);
    let pending = generators
        .generators
        .iter()
        .filter_map(|generator| {
            let name = generator.measurement_name()?;
            if generator.readout_plan().is_some() {
                return None;
            }
            let sites = dag
                .resolve_sites_for_measurement(name)
                .expect("named source action");
            (!sites.is_empty()).then_some((name, generator, sites))
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(BTreeMap::new());
    }
    let rank = generators.basis_rank();
    budget.check_matrix(
        rank.saturating_mul(3),
        generators.zx_graph.total_ids(),
        0,
        0,
    )?;
    let basis = RuntimeStabilizerBasis::from_generators(generators).with_t_nodes_as_ports();
    pending
        .into_iter()
        .map(|(name, generator, sites)| {
            budget.visit("causal readout search states")?;
            let deadline = generator
                .stabilizer
                .measurement_deadline(&generators.zx_graph)
                .expect("measurement has support");
            let domain = dag
                .resolve_value_domain_bounded_with_limits(&sites, limit, limits.boolean_limits())
                .map_err(|error| error.into_stabilizer("readout-domain branches", limit))?;
            let mut branches = Vec::new();
            for values in domain.values().chunks(64) {
                budget.check_matrix(
                    rank.saturating_mul(values.len().saturating_add(3)),
                    generators.zx_graph.total_ids(),
                    0,
                    0,
                )?;
                let fills = values
                    .iter()
                    .map(|values| selective_fills(&generators.zx_graph, &sites, values))
                    .collect::<Vec<_>>();
                for (values, branch) in values
                    .iter()
                    .zip(basis.apply_selective_fill_assignments(&fills)?)
                {
                    budget.visit("causal readout search states")?;
                    let surface = match closure {
                        ReadoutClosure::Required => {
                            branch.derive_causal_measurement_surface(name, deadline)
                        }
                        #[cfg(feature = "verify")]
                        ReadoutClosure::Analysis => branch
                            .derive_causal_measurement_surface(name, deadline)
                            .or_else(|error| match error {
                                RuntimeBasisError::Stabilizer(
                                    StabilizerError::UnavailableControlParity { .. },
                                ) => branch.derive_measurement_surface(name, deadline),
                                error => Err(error),
                            }),
                    }?;
                    branches.push((values.clone(), surface));
                }
            }
            Ok((
                name.to_owned(),
                ReadoutPlan {
                    coordinates: ReadoutCoordinates::PublicRows,
                    sites,
                    branches,
                    deadline,
                },
            ))
        })
        .collect()
}

pub(crate) fn plan_readouts(
    generators: &StabilizerGenerators,
    limits: ModuleCertificationLimits,
) -> Result<BTreeMap<String, ReadoutPlan>, RuntimeBasisError> {
    let zx = &generators.zx_graph;
    let mut budget = SearchBudget::with_limits(limits);
    let rank = generators.basis_rank();
    budget.check_matrix(
        rank.saturating_mul(3),
        zx.total_ids(),
        rank,
        generators
            .generators
            .len()
            .saturating_add(zx.nodes().len().saturating_mul(2))
            .saturating_add(zx.action_graph().ordered_nodes().count()),
    )?;
    let dag = zx.action_graph();
    let final_deadline = zx
        .nodes()
        .iter()
        .map(|node| i64::from(node.pos.z) + 1)
        .max()
        .unwrap_or(0);
    let actions = dag.ordered_nodes().collect::<Vec<_>>();
    let mut predecessors = vec![Vec::new(); actions.len()];
    for (from, to, _) in dag
        .dependencies()
        .filter(|(_, _, kind)| !kind.is_implicit())
    {
        predecessors[to].push(from);
    }
    let basis = RuntimeStabilizerBasis::for_source_readouts(generators)?;
    let mut known = HashSet::new();
    let mut readouts = BTreeMap::new();
    loop {
        budget.visit("composed readout search states")?;
        loop {
            let prior = known.len();
            for node in &actions {
                if !matches!(node.action, Action::Measure { .. })
                    && predecessors[node.ordinal]
                        .iter()
                        .all(|ordinal| known.contains(ordinal))
                {
                    known.insert(node.ordinal);
                }
            }
            if prior == known.len() {
                break;
            }
        }
        let mut sites = actions
            .iter()
            .filter_map(|node| match node.action {
                Action::Resolve { target, .. } if known.contains(&node.ordinal) => Some(target),
                _ => None,
            })
            .collect::<Vec<_>>();
        sites.sort_unstable_by_key(IVec3::to_array);
        let absent = unavailable_feedback(zx, &known);
        let available = generators
            .generators
            .iter()
            .enumerate()
            .filter_map(|(index, generator)| {
                generator
                    .measurement_name()
                    .filter(|name| readouts.contains_key(*name))
                    .map(|_| index)
            })
            .collect::<Vec<_>>();
        let pending = actions
            .iter()
            .filter_map(|node| match &node.action {
                Action::Measure { name, .. } if !known.contains(&node.ordinal) => {
                    Some((node.ordinal, name))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(readouts);
        }
        let baseline = dag
            .first_resolve_values_with_limits(&sites, limits.boolean_limits())?
            .expect("valid resolve domain");
        let baseline = basis.apply_selective_fills(&selective_fills(zx, &sites, &baseline))?;
        let mut candidates = Vec::new();
        for (ordinal, name) in &pending {
            budget.visit("composed readout search states")?;
            let derive = |basis: &RuntimeStabilizerBasis| {
                let mut surface =
                    basis.derive_readout_from_known(name, final_deadline, &absent, &available)?;
                surface
                    .readout_ordinals
                    .retain(|ordinal| available.contains(ordinal));
                Ok(surface)
            };
            let recipe = derive(&baseline).and_then(|surface| {
                plan_guarded_surface(&basis, &sites, surface, &derive, limits, &mut budget)
            });
            match recipe {
                Ok(recipe) => candidates.push((*ordinal, (*name).clone(), recipe)),
                Err(RuntimeBasisError::Stabilizer(StabilizerError::UnavailableControlParity {
                    ..
                })) => {}
                Err(error) => return Err(error),
            }
        }
        if candidates.is_empty() {
            return Err(StabilizerError::UnavailableControlParity {
                mvar: pending[0].1.clone(),
                deadline: final_deadline,
            }
            .into());
        }
        for (ordinal, name, mut recipe) in candidates {
            for &predecessor in recipe
                .branches
                .iter()
                .flat_map(|(_, branch)| &branch.readout_ordinals)
            {
                let name = generators.generators[predecessor]
                    .measurement_name()
                    .expect("known named readout");
                recipe.deadline = recipe.deadline.max(readouts[name].deadline);
            }
            readouts.insert(name, recipe);
            known.insert(ordinal);
        }
    }
}

fn plan_guarded_surface(
    basis: &RuntimeStabilizerBasis,
    sites: &[IVec3],
    initial: DerivedSurface,
    derive: &impl Fn(&RuntimeStabilizerBasis) -> Result<DerivedSurface, RuntimeBasisError>,
    limits: ModuleCertificationLimits,
    budget: &mut SearchBudget,
) -> Result<ReadoutPlan, RuntimeBasisError> {
    let limit = limits.max_guarded_domain_size;
    let zx = basis.zx_graph();
    if sites.is_empty() && limit != 0 {
        // The initial solve already certifies the only assignment.
        return Ok(ReadoutPlan {
            coordinates: ReadoutCoordinates::SourceOutcomes,
            sites: Vec::new(),
            deadline: initial.stabilizer.measurement_deadline(zx).unwrap_or(0),
            branches: vec![(Vec::new(), initial)],
        });
    }
    let support = |surface: &DerivedSurface| {
        sites
            .iter()
            .copied()
            .filter(|&pos| {
                surface
                    .stabilizer
                    .paulis
                    .get(zx.node_at(pos).expect("resolve node").id)
                    != Pauli::I
            })
            .collect::<Vec<_>>()
    };
    let mut local = support(&initial);
    loop {
        budget.visit("guarded readout search states")?;
        let domain = zx
            .action_graph()
            .resolve_value_domain_bounded_with_limits(&local, limit, limits.boolean_limits())
            .map_err(|error| error.into_stabilizer("guarded-domain branches", limit))?;
        let mut branches = Vec::new();
        let mut missing = None;
        'assignments: for values in domain.values().chunks(64) {
            budget.check_matrix(
                basis
                    .generators()
                    .len()
                    .saturating_mul(values.len().saturating_add(3)),
                zx.total_ids(),
                0,
                0,
            )?;
            let fills = values
                .iter()
                .map(|values| selective_fills(zx, &local, values))
                .collect::<Vec<_>>();
            for (values, branch) in values
                .iter()
                .zip(basis.apply_selective_fill_assignments(&fills)?)
            {
                budget.visit("guarded readout search states")?;
                match derive(&branch) {
                    Ok(surface) => branches.push((values.clone(), surface)),
                    Err(RuntimeBasisError::Stabilizer(
                        StabilizerError::UnavailableControlParity { .. },
                    )) => {
                        missing = Some(values.clone());
                        break 'assignments;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if let Some(missing) = missing {
            let fixed = local.iter().copied().zip(missing).collect::<Vec<_>>();
            let values = zx
                .action_graph()
                .complete_resolve_values_with_limits(sites, &fixed, limits.boolean_limits())?
                .expect("projected assignment has an extension");
            let branch = basis.apply_selective_fills(&selective_fills(zx, sites, &values))?;
            let surface = derive(&branch)?;
            let prior = local.len();
            local.extend(support(&surface));
            local.sort_unstable_by_key(IVec3::to_array);
            local.dedup();
            if local.len() == prior {
                local = sites.to_vec();
            }
            continue;
        }
        // Every omitted selector remained unresolved during each successful
        // solve, which required identity at that site. This proves that the
        // same recipe holds for all of its reachable extensions.
        let (latest, deadline) = branches
            .iter()
            .enumerate()
            .map(|(index, (_, surface))| {
                (
                    index,
                    surface.stabilizer.measurement_deadline(zx).unwrap_or(0),
                )
            })
            .max_by_key(|&(_, deadline)| deadline)
            .expect("nonempty resolve domain");
        branches.swap(0, latest);
        if branches
            .iter()
            .all(|(_, surface)| surface == &branches[0].1)
            && support(&branches[0].1).is_empty()
        {
            branches.truncate(1);
            branches[0].0.clear();
            local.clear();
        }
        return Ok(ReadoutPlan {
            coordinates: ReadoutCoordinates::SourceOutcomes,
            sites: local,
            branches,
            deadline,
        });
    }
}

fn selective_fills(
    zx: &ZXGraph,
    sites: &[IVec3],
    values: &[bool],
) -> Vec<(IVec3, crate::PauliBasis)> {
    sites
        .iter()
        .zip(values)
        .map(|(&pos, &value)| {
            let NodeKind::Selective(kind) = zx.node_at(pos).expect("resolve node").kind else {
                unreachable!("resolve sites name selective nodes")
            };
            (
                pos,
                if value {
                    kind.pauli_if_true()
                } else {
                    kind.pauli_if_false()
                },
            )
        })
        .collect()
}

fn unavailable_feedback(zx: &ZXGraph, known: &HashSet<usize>) -> Vec<Vec<(usize, Pauli)>> {
    zx.action_graph()
        .ordered_nodes()
        .filter(|node| !known.contains(&node.ordinal))
        .filter_map(|action| {
            let Action::Feedback { targets, .. } = &action.action else {
                return None;
            };
            Some(
                targets
                    .iter()
                    .map(|target| zx.feedback_column(target).expect("validated feedback wire"))
                    .collect(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readout_matrix_limit_counts_private_completion_rows() {
        let mut generators = crate::GalleryItem::CNOT
            .build()
            .materialize_root_graph()
            .expect("gallery flat projection")
            .stabilizers()
            .unwrap();
        let private = generators.generators.pop().unwrap().stabilizer.paulis;
        let generators = StabilizerGenerators::from_parts(
            generators.zx_graph,
            generators.generators,
            vec![private],
        );
        plan_readouts(&generators, ModuleCertificationLimits::UNLIMITED).unwrap();
        // Three public rows fit (147 words), but the complete four-row
        // relation and its readable coordinates need 196 words.
        assert!(matches!(
            plan_readouts(
                &generators,
                ModuleCertificationLimits {
                    max_matrix_words: 160,
                    ..ModuleCertificationLimits::UNLIMITED
                }
            ),
            Err(RuntimeBasisError::Stabilizer(
                StabilizerError::ResourceLimited {
                    phase: "dense matrix words",
                    observed: 196,
                    limit: 160,
                }
            ))
        ));
    }

    #[test]
    fn unavailable_feedback_uses_the_joint_pauli_action() {
        let source = crate::BlockGraph::from_blog_text(
            "BLOG 1.0
            0: ZXZ [0,0,0]
            1: ZXZ [0,0,1]
            2: ZXZ [0,0,2]
            0 -> +Z
            1 -> +Z
            m = measure 1
            ",
        )
        .unwrap();
        for last in [IVec3::ZERO, 2 * IVec3::Z] {
            let targets = [IVec3::ZERO, last]
                .map(|target| crate::FeedbackTarget {
                    target,
                    pauli: crate::PauliBasis::X,
                    direction: None,
                })
                .to_vec();
            let mut graph = source.clone();
            let mut actions = vec![Action::Feedback {
                targets: targets.clone(),
                condition: Some(crate::Expr::Var("m".into())),
            }];
            actions.extend(source.actions());
            graph.set_actions(actions).unwrap();
            let generators = graph.stabilizers().unwrap();
            let plans = plan_readouts(&generators, ModuleCertificationLimits::DEFAULT).unwrap();
            let surface = &plans["m"].branches[0].1.stabilizer;
            assert!(surface.odd_anticommutes_feedback(&targets[..1], Some(&generators.zx_graph)));
            assert!(!surface.odd_anticommutes_feedback(&targets, Some(&generators.zx_graph)));
        }
    }
}
