//! Branch-parametric (symbolic) stabilizer basis.
//!
//! Filling a selective node at site `c` acts on the basis through
//! [`StabilizerBasis::apply_tagged_selective_fill`](super::stabilizer::StabilizerBasis::apply_tagged_selective_fill),
//! which is the pointwise ground truth this module models. That transition uses
//! the site's diagonalized fixing row `F_c` as the pivot: it XORs `F_c` into
//! every row whose support at the site column *anticommutes* with the chosen
//! fill axis, then removes `F_c` from the basis.
//!
//! # The affine model
//!
//! Two properties of the canonical basis make the whole evolution under *any*
//! fill combination affine in the fill choices, with per-`(row, site)` behavior
//! fixed at compile time:
//!
//! * **Fixing rows are pairwise diagonalized** — `F_c` carries the forbidden
//!   Pauli on its own selective column and identity on every *other* selective
//!   column. So filling site `d` never changes any row's support at column `c`,
//!   and never disturbs `F_c` itself (which is consumed only when site `c` is
//!   filled). A row's trigger status at site `c` therefore depends only on its
//!   *initial* support at column `c`, independent of the fill order.
//!
//! * Consequently every surviving (non-fixing) row evolves as
//!
//!   ```text
//!   concrete(row) = base ⊕ ⊕ { F_c : site c's chosen axis triggers this row }
//!   ```
//!
//!   an XOR of a compile-time-fixed `base` and a subset of the fixing rows
//!   selected by the fill assignment.
//!
//! # Arm polarity
//!
//! The tagged fill XORs `F_c` into a row iff the row's support `p` at the site
//! column satisfies `p != I && p != chosen_axis` (see
//! `apply_tagged_selective_fill`). Because fixing rows carry the forbidden Pauli
//! and tracking rows are pivoted free of it during canonicalization, a surviving
//! row's support at a site is normally one of `{I, axis0, axis1}` (the two
//! allowed axes of the site's [`SelectiveKind`]):
//!
//! * support `I` → never triggered;
//! * support `axis0` → triggered only by choosing `axis1`;
//! * support `axis1` → triggered only by choosing `axis0`.
//!
//! We store the support Pauli itself, so evaluation mirrors the engine's
//! condition verbatim (`support != chosen`). This also handles the degenerate
//! case where a row carries the forbidden Pauli (`support == forbidden`): it then
//! triggers for *both* axes — still affine, just unconditional.
//!
//! # Eligibility
//!
//! The model covers exactly the tagged-fill path. A graph is eligible iff every
//! selective site owns a fixing row that (a) carries the forbidden Pauli on its
//! own column and (b) is diagonalized against every other selective column, with
//! a one-to-one correspondence between fixing rows and sites. Any violation
//! yields a [`SymbolicBasisError`] naming the offending site — the
//! representational-fallback signal, not a panic. At runtime such a graph takes
//! the generic Gaussian path
//! ([`apply_generic_selective_fill`](super::stabilizer::StabilizerBasis::apply_generic_selective_fill)),
//! which this module deliberately does not represent.
//!
//! Partial evaluation (holding some sites symbolic while fixing others) is a
//! trivial extension of the same structure but is intentionally not built here:
//! no current consumer needs it.

use std::collections::BTreeSet;
#[cfg(test)]
use std::collections::HashMap;

use glam::IVec3;

#[cfg(test)]
use bloq_utils::PauliBasis;
use bloq_utils::boolean::{BooleanLimits, BooleanResourceError};
use bloq_utils::{Pauli, PauliString};

#[cfg(test)]
use super::stabilizer::StabilizerBasis;
use super::stabilizer::{StabilizerRowKind, collect_selective_constraints};
use super::{StabilizerGenerators, ZXGraph};
use crate::SelectiveKind;

/// A selective site's raw descriptor: where it is, its kind, and the column and
/// forbidden Pauli its fixing row must own. The construction input, before a
/// pivot is resolved.
struct SiteSpec {
    pos: IVec3,
    kind: SelectiveKind,
    col: usize,
    forbidden: Pauli,
}

/// One selective site and the fixing row that resolves it.
#[derive(Debug, Clone)]
pub struct SymbolicSite {
    /// Position of the selective node.
    pub pos: IVec3,
    /// The node's kind, defining its two allowed fill axes.
    pub kind: SelectiveKind,
    /// Column (node id) the site occupies in the Pauli strings.
    pub(crate) col: usize,
    /// The fixing row `F_c`, XORed into a row when this site triggers it.
    pub(crate) fixing_row: PauliString,
    /// Ordinal of `F_c` in the initial basis, for provenance composition.
    #[cfg(test)]
    pub(crate) fixing_ordinal: usize,
}

/// A gated contribution of one site's fixing row to one surviving row.
#[derive(Debug, Clone)]
pub(crate) struct SymbolicDelta {
    /// Index of the triggering site in [`SymbolicStabilizerBasis::sites`].
    pub(crate) site: usize,
    /// The surviving row's support Pauli at the site column (never `I`). The
    /// delta triggers for a chosen axis `a` iff `support != Pauli::from(a)`.
    pub(crate) support: Pauli,
}

/// One surviving (non-fixing) basis row as an affine function of the fills.
#[derive(Debug, Clone)]
pub(crate) struct SymbolicRow {
    /// The row's value when no site triggers it.
    pub(crate) base: PauliString,
    /// The role the row plays, preserved from the initial basis.
    pub(crate) kind: StabilizerRowKind,
    /// Ordinal of this row in the initial basis, seeding its provenance.
    #[cfg(test)]
    pub(crate) base_ordinal: usize,
    /// Per-site gated fixing-row contributions (only sites the row touches).
    pub(crate) deltas: Vec<SymbolicDelta>,
}

/// The surviving basis represented as an affine function of the fill assignment.
#[derive(Debug, Clone)]
pub struct SymbolicStabilizerBasis {
    sites: Vec<SymbolicSite>,
    rows: Vec<SymbolicRow>,
}

/// The concrete basis for one fill assignment, plus per-row provenance.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct SymbolicEvaluation {
    /// The surviving rows and their kinds, in survivor order.
    pub(crate) basis: StabilizerBasis,
    /// For each surviving row, the initial-basis ordinals whose product it is.
    pub(crate) combinations: Vec<BTreeSet<usize>>,
}

/// A symbolic basis cannot represent a selective site's fixing structure.
/// This reports an unsupported symbolic model, not an invalid source graph.
/// Callers can fall back to runtime-basis replay.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SymbolicBasisError {
    /// A selective site has no symbolic fixing representation.
    #[error("selective site {0} is unsupported by the symbolic basis model")]
    UnsupportedSite(IVec3),
}

impl SymbolicStabilizerBasis {
    /// The selective sites, in the order fills are keyed against.
    pub fn sites(&self) -> &[SymbolicSite] {
        &self.sites
    }

    pub(crate) fn measurements_close_before_outputs(
        &self,
        zx: &ZXGraph,
        limits: BooleanLimits,
    ) -> Result<bool, BooleanResourceError> {
        let components = zx
            .nodes()
            .iter()
            .filter(|node| node.is_output_port(zx))
            .flat_map(|node| {
                let axes: &[_] = if node.role == crate::PortRole::Multiplex {
                    &[Pauli::X]
                } else {
                    &[Pauli::X, Pauli::Z]
                };
                axes.iter().map(move |&axis| (node.id, axis))
            });
        let components = components.collect::<Vec<_>>();

        for row in self
            .rows
            .iter()
            .filter(|row| matches!(row.kind, StabilizerRowKind::Measurement { .. }))
        {
            for &(col, axis) in &components {
                if !self.component_closes(row, zx.action_graph(), col, axis, limits)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn component_closes(
        &self,
        row: &SymbolicRow,
        actions: &crate::ActionDag,
        col: usize,
        axis: Pauli,
        limits: BooleanLimits,
    ) -> Result<bool, BooleanResourceError> {
        let mut expected = row.base.get(col) & axis;
        let mut targets = Vec::new();
        for delta in &row.deltas {
            let site = &self.sites[delta.site];
            if !(site.fixing_row.get(col) & axis) {
                continue;
            }
            if delta.support == Pauli::from(site.kind.pauli_if_true()) {
                expected ^= true;
                targets.push(site.pos);
            } else if delta.support == Pauli::from(site.kind.pauli_if_false()) {
                targets.push(site.pos);
            } else {
                expected ^= true;
            }
        }
        actions.resolve_xor_is_with_limits(&targets, expected, limits)
    }

    /// Builds the symbolic basis from already-canonicalized generators, reusing
    /// their rows and kinds verbatim so ordinals stay identity-stable with a
    /// [`RuntimeStabilizerBasis::from_generators`](super::runtime_basis::RuntimeStabilizerBasis::from_generators)
    /// replay.
    ///
    /// # Errors
    ///
    /// Returns a [`SymbolicBasisError`] naming the first site whose fixing
    /// structure does not fit the affine model (see the module docs).
    pub fn from_generators(stabilizers: &StabilizerGenerators) -> Result<Self, SymbolicBasisError> {
        let rows: Vec<PauliString> = stabilizers
            .generators
            .iter()
            .map(|generator| generator.stabilizer.paulis.clone())
            .collect();
        let kinds: Vec<StabilizerRowKind> = stabilizers
            .generators
            .iter()
            .map(|generator| generator.kind.clone())
            .collect();

        // Site descriptors carry the column and forbidden Pauli the fixing row
        // must own; `collect_selective_constraints` orders them by node scan
        // order, which we adopt as the canonical site order.
        let specs: Vec<SiteSpec> = collect_selective_constraints(&stabilizers.zx_graph)
            .into_iter()
            .map(|constraint| SiteSpec {
                pos: constraint.pos,
                kind: constraint.kind,
                col: constraint.col,
                forbidden: constraint.forbidden,
            })
            .collect();

        Self::from_raw(rows, kinds, &specs)
    }

    /// Core construction from raw rows, kinds, and site descriptors. Split out
    /// of [`from_generators`](Self::from_generators) so the eligibility rules can
    /// be exercised on hand-built bases without a backing [`ZXGraph`].
    fn from_raw(
        rows: Vec<PauliString>,
        kinds: Vec<StabilizerRowKind>,
        specs: &[SiteSpec],
    ) -> Result<Self, SymbolicBasisError> {
        // Resolve each site to its pivot the same way the engine does — the
        // first row that fixes the site — and validate the fixing structure.
        let mut sites = Vec::with_capacity(specs.len());
        let mut pivot_indices = Vec::with_capacity(specs.len());
        for spec in specs {
            let pivot = kinds
                .iter()
                .position(|kind| kind.fixes_selective(spec.pos))
                .ok_or(SymbolicBasisError::UnsupportedSite(spec.pos))?;

            // The pivot must carry the forbidden Pauli on its own column;
            // otherwise the tagged fill rejects the axis equal to that support.
            if rows[pivot].get(spec.col) != spec.forbidden {
                return Err(SymbolicBasisError::UnsupportedSite(spec.pos));
            }

            // Diagonalization: identity on every other selective column, so
            // filling this site cannot alter another site's trigger status.
            for other in specs {
                if other.pos != spec.pos && rows[pivot].get(other.col) != Pauli::I {
                    return Err(SymbolicBasisError::UnsupportedSite(spec.pos));
                }
            }

            sites.push(SymbolicSite {
                pos: spec.pos,
                kind: spec.kind,
                col: spec.col,
                fixing_row: rows[pivot].clone(),
                #[cfg(test)]
                fixing_ordinal: pivot,
            });
            pivot_indices.push(pivot);
        }

        // Every fixing row must be exactly one site's pivot: distinct pivots and
        // no leftover fixing rows. A leftover fixing row would survive our
        // filter but never be removed by a replay, diverging the two.
        let pivot_set: BTreeSet<usize> = pivot_indices.iter().copied().collect();
        if pivot_set.len() != pivot_indices.len() {
            // Two sites share a pivot; blame the second occurrence's site.
            let mut seen = BTreeSet::new();
            let doubled = pivot_indices
                .iter()
                .position(|pivot| !seen.insert(*pivot))
                .expect("a duplicate pivot exists when the set is smaller");
            return Err(SymbolicBasisError::UnsupportedSite(sites[doubled].pos));
        }
        for (index, kind) in kinds.iter().enumerate() {
            if kind.is_selective_fixing() && !pivot_set.contains(&index) {
                // A fixing row nobody pivots on; attribute it to its first target.
                let pos = kind
                    .selective_fixing_targets()
                    .first()
                    .map(|target| target.pos)
                    .unwrap_or_default();
                return Err(SymbolicBasisError::UnsupportedSite(pos));
            }
        }

        // Survivors are the non-fixing rows in their original relative order,
        // matching the in-place removals of a sequential replay.
        let col_by_site: Vec<usize> = sites.iter().map(|site| site.col).collect();
        let symbolic_rows = kinds
            .iter()
            .enumerate()
            .filter(|(_, kind)| kind.is_readout())
            .map(|(ordinal, kind)| {
                let row = &rows[ordinal];
                let deltas = col_by_site
                    .iter()
                    .enumerate()
                    .filter_map(|(site, &col)| {
                        let support = row.get(col);
                        (support != Pauli::I).then_some(SymbolicDelta { site, support })
                    })
                    .collect();
                SymbolicRow {
                    base: row.clone(),
                    kind: kind.clone(),
                    #[cfg(test)]
                    base_ordinal: ordinal,
                    deltas,
                }
            })
            .collect();

        Ok(Self {
            sites,
            rows: symbolic_rows,
        })
    }

    /// Evaluates the concrete basis for a full fill assignment (one chosen axis
    /// per site), plus each surviving row's initial-ordinal provenance.
    ///
    /// # Panics
    ///
    /// Panics if `fills` omits a site of this basis: the model represents full
    /// assignments only.
    #[cfg(test)]
    pub(crate) fn evaluate(&self, fills: &[(IVec3, PauliBasis)]) -> SymbolicEvaluation {
        let chosen: HashMap<IVec3, PauliBasis> = fills.iter().copied().collect();
        let axes = self
            .sites
            .iter()
            .map(|site| {
                *chosen
                    .get(&site.pos)
                    .expect("fill assignment covers every selective site")
            })
            .collect::<Vec<_>>();

        let mut out_rows = Vec::with_capacity(self.rows.len());
        let mut out_kinds = Vec::with_capacity(self.rows.len());
        let mut combinations = Vec::with_capacity(self.rows.len());

        for row in &self.rows {
            let mut value = row.base.clone();
            let mut combination = BTreeSet::from([row.base_ordinal]);
            for delta in &row.deltas {
                let site = &self.sites[delta.site];
                let axis = axes[delta.site];
                // Mirror the engine's trigger: XOR the fixing row iff the row's
                // support anticommutes with the chosen axis (support is non-I).
                if delta.support != Pauli::from(axis) {
                    value ^= &site.fixing_row;
                    xor_insert(&mut combination, site.fixing_ordinal);
                }
            }
            out_rows.push(value);
            out_kinds.push(row.kind.clone());
            combinations.push(combination);
        }

        SymbolicEvaluation {
            basis: StabilizerBasis::new(out_rows, out_kinds),
            combinations,
        }
    }
}

/// XOR-toggle `value` into `set`: present ⇒ remove, absent ⇒ insert.
#[cfg(test)]
fn xor_insert(set: &mut BTreeSet<usize>, value: usize) {
    if !set.remove(&value) {
        set.insert(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::stabilizer::{SelectiveFixingTarget, coeffs_to_sparse};
    use crate::GalleryItem;

    /// Site count below which we enumerate every fill assignment (`2^n`). Above
    /// it, exhaustive enumeration is infeasible — the gallery's `PhaseGradientK4`
    /// has 22 selective sites — so we fall back to a structured pairwise sample.
    const EXHAUSTIVE_SITE_LIMIT: usize = 12;

    /// Rows, kinds, and `(pos, kind, col)` site descriptors of a canonical basis.
    type BasisParts = (
        Vec<PauliString>,
        Vec<StabilizerRowKind>,
        Vec<(IVec3, SelectiveKind, usize)>,
    );

    /// One fill assignment: a chosen axis per selective site.
    type Assignment = Vec<(IVec3, PauliBasis)>;

    /// The result of an engine replay: surviving rows, their kinds, and each
    /// row's composed initial-ordinal provenance.
    type ReplayResult = (
        Vec<PauliString>,
        Vec<StabilizerRowKind>,
        Vec<BTreeSet<usize>>,
    );

    /// Extracts the raw basis parts from canonicalized generators, mirroring what
    /// [`SymbolicStabilizerBasis::from_generators`] and the engine replay consume.
    fn basis_parts(stabilizers: &crate::StabilizerGenerators) -> BasisParts {
        let rows = stabilizers
            .generators
            .iter()
            .map(|generator| generator.stabilizer.paulis.clone())
            .collect();
        let kinds = stabilizers
            .generators
            .iter()
            .map(|generator| generator.kind.clone())
            .collect();
        let sites = collect_selective_constraints(&stabilizers.zx_graph)
            .into_iter()
            .map(|constraint| (constraint.pos, constraint.kind, constraint.col))
            .collect();
        (rows, kinds, sites)
    }

    /// The assignment for a bitmask: bit `i` set ⇒ site `i` takes its `if_true`
    /// axis, else `if_false`.
    fn assignment_from_mask(sites: &[(IVec3, SelectiveKind, usize)], mask: u64) -> Assignment {
        sites
            .iter()
            .enumerate()
            .map(|(index, &(pos, kind, _))| {
                let axis = if (mask >> index) & 1 == 1 {
                    kind.pauli_if_true()
                } else {
                    kind.pauli_if_false()
                };
                (pos, axis)
            })
            .collect()
    }

    /// Fill assignments exercising `sites`. Up to [`EXHAUSTIVE_SITE_LIMIT`] sites
    /// we enumerate the full `2^n` product. Beyond it we sample structurally:
    /// all-false, all-true, every single site toggled from each pole, and every
    /// pair. Pairwise coverage is sufficient here because the model is affine
    /// (each site contributes an independent XOR), so any cross-site coupling
    /// would already surface in some two-site assignment.
    fn sampled_assignments(sites: &[(IVec3, SelectiveKind, usize)]) -> Vec<Assignment> {
        let n = sites.len();
        if n <= EXHAUSTIVE_SITE_LIMIT {
            return (0..(1u64 << n))
                .map(|mask| assignment_from_mask(sites, mask))
                .collect();
        }

        let full = (1u64 << n) - 1;
        let mut masks = BTreeSet::from([0, full]);
        for i in 0..n {
            masks.insert(1u64 << i);
            masks.insert(full ^ (1u64 << i));
            for j in (i + 1)..n {
                masks.insert((1u64 << i) | (1u64 << j));
            }
        }
        masks
            .into_iter()
            .map(|mask| assignment_from_mask(sites, mask))
            .collect()
    }

    /// Replays one full assignment through the engine's tagged fill — the
    /// pointwise ground truth — composing each step's combinations down to
    /// initial ordinals. Returns `None` if any site rejects the tagged path.
    fn replay_tagged(
        rows: &[PauliString],
        kinds: &[StabilizerRowKind],
        sites: &[(IVec3, SelectiveKind, usize)],
        assignment: &[(IVec3, PauliBasis)],
    ) -> Option<ReplayResult> {
        let chosen: HashMap<IVec3, PauliBasis> = assignment.iter().copied().collect();
        let mut basis = StabilizerBasis::new(rows.to_vec(), kinds.to_vec());
        let mut composed: Vec<BTreeSet<usize>> = (0..rows.len())
            .map(|index| BTreeSet::from([index]))
            .collect();

        for &(pos, kind, col) in sites {
            let axis = chosen[&pos];
            let target = Pauli::from(axis);
            let (next, coeffs) = basis
                .apply_tagged_selective_fill(&[(col, target)], pos, kind)
                .ok()?;
            composed = coeffs_to_sparse(&coeffs)
                .iter()
                .map(|combination| {
                    let mut set = BTreeSet::new();
                    for &index in combination {
                        for &ordinal in &composed[index] {
                            xor_insert(&mut set, ordinal);
                        }
                    }
                    set
                })
                .collect();
            basis = next;
        }
        Some((basis.rows, basis.kinds, composed))
    }

    fn fixing(pos: IVec3, forbidden: Pauli) -> StabilizerRowKind {
        StabilizerRowKind::SelectiveFixing {
            targets: vec![SelectiveFixingTarget { pos, forbidden }],
        }
    }

    /// Full-combo equivalence across the gallery: for every eligible graph with
    /// selective sites, every fill assignment must reproduce the engine replay's
    /// rows, kinds, and composed provenance exactly.
    #[test]
    fn eligibility_and_fill_order_match_tagged_replay_across_gallery() {
        let mut eligible_graphs = 0;
        let mut total_assignments = 0;
        let mut multi_site_exercised = false;

        for entry in GalleryItem::iter().filter(|entry| {
            !entry.in_category(crate::GalleryCategory::AnalysisOnly)
                && entry.build().root().instances.is_empty()
        }) {
            let graph = entry
                .build()
                .materialize_root_graph()
                .expect("gallery flat projection");
            // This root replays selective fills on fixed topology. Structural
            // choices require explicit projections, checked by guarded suites.
            if !graph.branch_definitions().is_empty() {
                continue;
            }
            let zx = crate::ZXGraph::try_from(&graph)
                .unwrap_or_else(|error| panic!("gallery {entry:?}: ZX conversion failed: {error}"));
            let stabilizers = zx
                .stabilizers()
                .unwrap_or_else(|error| panic!("gallery {entry:?}: stabilizers failed: {error}"));
            let (rows, kinds, sites) = basis_parts(&stabilizers);
            if sites.is_empty() {
                continue;
            }
            let symbolic = match SymbolicStabilizerBasis::from_generators(&stabilizers) {
                Ok(symbolic) => symbolic,
                Err(SymbolicBasisError::UnsupportedSite(_)) => {
                    assert!(
                        sampled_assignments(&sites).iter().any(|assignment| {
                            replay_tagged(&rows, &kinds, &sites, assignment).is_none()
                        }),
                        "gallery {entry:?}: symbolic model rejected a fully tagged basis"
                    );
                    continue;
                }
            };
            eligible_graphs += 1;

            for assignment in sampled_assignments(&sites) {
                let evaluated = symbolic.evaluate(&assignment);
                let (replay_rows, replay_kinds, replay_combos) =
                    replay_tagged(&rows, &kinds, &sites, &assignment)
                        .expect("eligible graph takes the tagged path for every assignment");
                if sites.len() >= 2 {
                    multi_site_exercised = true;
                    let mut reversed = sites.clone();
                    reversed.reverse();
                    let backward = replay_tagged(&rows, &kinds, &reversed, &assignment)
                        .expect("eligible graph takes the reversed tagged path");
                    assert_eq!(
                        (&replay_rows, &replay_kinds, &replay_combos),
                        (&backward.0, &backward.1, &backward.2),
                        "gallery {entry:?}: fill order matters"
                    );
                }

                assert_eq!(
                    evaluated.basis.rows, replay_rows,
                    "gallery {entry:?}: rows differ for {assignment:?}"
                );
                assert_eq!(
                    evaluated.basis.kinds, replay_kinds,
                    "gallery {entry:?}: kinds differ for {assignment:?}"
                );
                assert_eq!(
                    evaluated.combinations, replay_combos,
                    "gallery {entry:?}: combinations differ for {assignment:?}"
                );
                total_assignments += 1;
            }
        }

        assert!(eligible_graphs > 0, "no eligible gallery graph exercised");
        assert!(total_assignments > 1, "too few assignments exercised");
        assert!(multi_site_exercised, "no multi-site fill order exercised");
    }

    /// Coupled detection: a fixing row that is not diagonalized against another
    /// site's column is ineligible, blaming the coupled site pair. The gallery
    /// has no such graph, so this pins the rule on a hand-built basis.
    #[test]
    fn non_diagonalized_fixing_rows_are_ineligible() {
        let site_a = IVec3::new(0, 0, 0);
        let site_b = IVec3::new(1, 0, 0);
        // Fixing row for A carries Z at both columns: forbidden on its own (col 0)
        // but non-identity on B's column (col 1), so the two fills are coupled.
        let rows = vec![
            PauliString::try_from("ZZ").unwrap(),
            PauliString::try_from("_Z").unwrap(),
        ];
        let kinds = vec![fixing(site_a, Pauli::Z), fixing(site_b, Pauli::Z)];
        let specs = [
            SiteSpec {
                pos: site_a,
                kind: SelectiveKind::XY,
                col: 0,
                forbidden: Pauli::Z,
            },
            SiteSpec {
                pos: site_b,
                kind: SelectiveKind::XY,
                col: 1,
                forbidden: Pauli::Z,
            },
        ];

        let error = SymbolicStabilizerBasis::from_raw(rows, kinds, &specs)
            .expect_err("non-diagonalized fixing rows must be ineligible");
        assert_eq!(error, SymbolicBasisError::UnsupportedSite(site_a));
    }

    /// A fixing row that does not carry the forbidden Pauli on its own column is
    /// ineligible: the tagged fill would reject the axis equal to that support.
    #[test]
    fn fixing_row_without_forbidden_support_is_ineligible() {
        let site = IVec3::new(0, 0, 0);
        let rows = vec![PauliString::try_from("X").unwrap()];
        let kinds = vec![fixing(site, Pauli::Z)];
        let specs = [SiteSpec {
            pos: site,
            kind: SelectiveKind::XY,
            col: 0,
            forbidden: Pauli::Z,
        }];

        let error = SymbolicStabilizerBasis::from_raw(rows, kinds, &specs)
            .expect_err("a fixing row lacking forbidden support must be ineligible");
        assert_eq!(error, SymbolicBasisError::UnsupportedSite(site));
    }
}
