//! Reusable module certificates, summaries, and composed correlation supports.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::Arc;

use glam::IVec3;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::program::{projection_limit, qualified_name, resource_limit};
use crate::zx::{
    EliminationRow, FlowWitness, ModuleSeam, ProjectionLimits, ZXGraph,
    leading_pivot_elimination_from, signed_gaussian_elimination, signed_gaussian_elimination_from,
    xor_pauli_maps,
};
use crate::{
    Action, Basis, Block, BlockGraph, BlockKind, Direction, InstancePort, MeasurementObservable,
    ModuleCertificationError, ModuleCertificationLimits, ModuleError, ModuleInstance,
    ModuleOrientation, Pauli, PauliBasis, PauliString, PhasedPauliString, Pipe, QuantumConnection,
    QuantumPort, Stabilizer, StabilizerGenerators, StabilizerRowKind, UDirection,
    checked_add_position,
};

/// Geometry and basis data recorded when a module Port is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModulePortSignature {
    /// Direction from the removed port toward its interior neighbor.
    pub direction: Direction,
    /// Whether the removed port seam carries a Hadamard basis change.
    pub hadamard: bool,
    /// Interior block's boundary basis on each lattice axis.
    pub interior_bases: [Option<Basis>; 3],
}

impl ModulePortSignature {
    /// Returns this port geometry in `orientation`.
    pub(crate) fn with_orientation(self, orientation: ModuleOrientation) -> Self {
        Self {
            direction: orientation.rotate_direction(self.direction),
            hadamard: self.hadamard,
            interior_bases: orientation.rotate_axis_values(self.interior_bases),
        }
    }
}

/// Canonical signed Pauli relation over one module's declared Ports.
#[derive(Debug, Clone)]
pub struct ModuleSummary {
    name: String,
    limits: ModuleCertificationLimits,
    dependencies: HashMap<String, Arc<ModuleSummary>>,
    ports: Vec<QuantumPort>,
    signatures: Vec<ModulePortSignature>,
    rows: Vec<SupportedRow>,
    /// Branch-safe logical rows exported by this module. Unlike `rows`, these
    /// are public readout representatives rather than the complete boundary
    /// relation used to compose seams.
    logical_rows: Vec<SupportedRow>,
    /// Complete signed certificate basis over this definition's public Ports
    /// and positioned interior support. Public rows are kept separately in
    /// `rows`; this basis supplies only the private rank completion at link.
    certificate_rows: Vec<SupportedRow>,
    adjustments: Vec<SupportedRow>,
    measurements: Vec<SummaryMeasurement>,
    discharged_measurements: Vec<SummaryMeasurement>,
    protected: Vec<ProtectedSupport>,
    frontier: Vec<SupportTarget>,
    protected_groups: Vec<ProtectedGroup>,
    branch_variants: Vec<ModuleBranchVariant>,
    projection: Option<Box<ModuleProjection>>,
}

/// A certificate recipe, evaluated only for a projection explicitly requested
/// by a caller. The compiler composes guarded surfaces directly instead.
#[derive(Debug, Clone)]
struct ModuleProjection {
    definition: BlockGraph,
    targets: Vec<IVec3>,
    limits: ModuleCertificationLimits,
}

#[derive(Debug, Clone)]
struct ModuleBranchVariant {
    assignments: Vec<(IVec3, bool)>,
    summary: Box<ModuleSummary>,
}

impl ModuleSummary {
    /// Returns the summarized module definition name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Finds a definition summary already built as part of this root summary.
    #[doc(hidden)]
    pub fn definition(&self, name: &str) -> Option<&ModuleSummary> {
        let mut pending = vec![self];
        let mut seen = FxHashSet::default();
        while let Some(summary) = pending.pop() {
            if summary.name == name {
                return Some(summary);
            }
            if seen.insert(summary.name.as_str()) {
                pending.extend(summary.dependencies.values().map(Arc::as_ref));
            }
        }
        None
    }

    /// Returns public quantum ports in interface declaration order.
    pub fn quantum_ports(&self) -> &[QuantumPort] {
        &self.ports
    }

    /// Iterates over signed public boundary relations.
    pub fn boundary_rows(&self) -> impl ExactSizeIterator<Item = &PhasedPauliString> {
        self.rows.iter().map(|row| &row.signed)
    }

    /// Materialize and present the complete composed certificate without
    /// regenerating the linked graph's external stabilizer table. Rejects
    /// named measurements without output-safe branch representatives (C0).
    #[doc(hidden)]
    pub fn materialize_stabilizers(
        &self,
        zx: &ZXGraph,
        offset: IVec3,
    ) -> Result<StabilizerGenerators, ModuleCertificationError> {
        self.materialize_stabilizers_from(zx, ModuleOrientation::IDENTITY, offset)
    }

    /// Materialize one requested structural projection. Projection topology is
    /// still selected by the caller, but its stabilizer row space comes from
    /// bottom-up module composition rather than the flattened graph.
    #[doc(hidden)]
    pub fn materialize_projection_stabilizers(
        &self,
        zx: &ZXGraph,
        offset: IVec3,
        assignments: &[(IVec3, bool)],
    ) -> Result<StabilizerGenerators, ModuleCertificationError> {
        let mut key = assignments
            .iter()
            .filter(|(_, value)| !value)
            .map(|&(position, value)| {
                checked_add_position(position, -offset)
                    .map(|position| (position, value))
                    .map_err(|source| ModuleCertificationError::Graph {
                        module: self.name.clone(),
                        source,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        key.sort_unstable_by_key(|(position, value)| (position.to_array(), *value));
        if key.is_empty() {
            return self.materialize_stabilizers_from(zx, ModuleOrientation::IDENTITY, offset);
        }
        if self.projection.is_some() {
            let absent = key.iter().map(|&(position, _)| position).collect();
            return self
                .select_projection(&absent)?
                .materialize_stabilizers_from(zx, ModuleOrientation::IDENTITY, offset);
        }
        let variant = self
            .branch_variants
            .iter()
            .find(|variant| variant.assignments == key)
            .ok_or_else(|| {
                invalid(
                    &self.name,
                    "reachable structural projection has no composed module certificate",
                )
            })?;
        variant
            .summary
            .materialize_stabilizers_from(zx, ModuleOrientation::IDENTITY, offset)
    }

    fn select_projection(
        &self,
        absent: &FxHashSet<IVec3>,
    ) -> Result<ModuleSummary, ModuleCertificationError> {
        let Some(projection) = &self.projection else {
            return Ok(self.clone());
        };
        if absent
            .iter()
            .any(|target| !projection.targets.contains(target))
        {
            return Err(invalid(&self.name, "projection selects an unknown branch"));
        }
        let module = &projection.definition;
        let graph_error = |source| ModuleCertificationError::Graph {
            module: module.name.clone(),
            source,
        };
        let mut selected = module.clone_local_definition();
        selected.replace_local_body(
            module
                .local_body()
                .project_branches_in_definition(
                    module
                        .local_body()
                        .branch_regions()
                        .map_err(graph_error)?
                        .iter()
                        .map(|region| (region.target, !absent.contains(&region.target))),
                )
                .map_err(graph_error)?,
        );
        if module.instances.is_empty() {
            return summarize_leaf_body(module, selected.local_body(), projection.limits);
        }
        let children = module
            .instances
            .iter()
            .map(|instance| {
                let base = &self.dependencies[&instance.definition];
                let mut key = FxHashSet::default();
                if let Some(child) = &base.projection {
                    for &target in &child.targets {
                        if absent.contains(
                            &instance
                                .try_transform_position(target)
                                .map_err(graph_error)?,
                        ) {
                            key.insert(target);
                        }
                    }
                }
                let child = if key.is_empty() {
                    Arc::clone(base)
                } else {
                    Arc::new(base.select_projection(&key)?)
                };
                Ok((instance.name.as_str(), (instance, child)))
            })
            .collect::<Result<HashMap<_, _>, ModuleCertificationError>>()?;
        summarize_composite(&selected, module, &children, projection.limits)
    }

    fn materialize_stabilizers_from(
        &self,
        zx: &ZXGraph,
        orientation: ModuleOrientation,
        offset: IVec3,
    ) -> Result<StabilizerGenerators, ModuleCertificationError> {
        // PauliString holds two BitVecs, each rounded to a 512-bit block.
        let words = [
            self.certificate_rows.len(),
            self.measurements.len(),
            self.discharged_measurements.len(),
            self.logical_rows.len(),
            self.adjustments.len(),
        ]
        .into_iter()
        .try_fold(0usize, usize::checked_add)
        .and_then(|rows| rows.checked_mul(zx.total_ids().div_ceil(512)))
        .and_then(|blocks| blocks.checked_mul(16));
        if words.is_none_or(|words| words > self.limits.max_matrix_words) {
            return Err(resource_limit(
                &self.name,
                "dense matrix words",
                words.unwrap_or(usize::MAX),
                self.limits.max_matrix_words,
            ));
        }
        let external_basis = self
            .certificate_rows
            .iter()
            .map(|row| {
                let stabilizer = materialize_summary_row(
                    &row.signed,
                    &row.support,
                    &self.ports,
                    zx,
                    orientation,
                    offset,
                    &self.name,
                )?;
                let mut paulis = stabilizer.paulis;
                zx.clear_cross_centers(std::slice::from_mut(&mut paulis));
                let witness = row
                    .witness
                    .transformed(orientation, offset)
                    .map_err(|source| ModuleCertificationError::Graph {
                        module: self.name.clone(),
                        source,
                    })?;
                let witnessed =
                    zx.materialize_flow_witness(&witness, &paulis)
                        .ok_or_else(|| {
                            invalid(
                                &self.name,
                                "composed certificate witness does not fit the linked graph",
                            )
                        })?;
                if witnessed.paulis != paulis {
                    return Err(invalid(
                        &self.name,
                        "composed certificate witness does not reproduce its positioned support",
                    ));
                }
                Ok(PhasedPauliString::new(paulis, witnessed.phase()))
            })
            .collect::<Result<Vec<_>, ModuleCertificationError>>()?;
        let cached_prefix = self
            .measurements
            .iter()
            .chain(&self.discharged_measurements)
            .map(|row| {
                materialize_summary_row(
                    &row.signed,
                    &row.support,
                    &self.ports,
                    zx,
                    orientation,
                    offset,
                    &self.name,
                )
                .and_then(|stabilizer| {
                    Ok((
                        transformed_row_kind(&row.kind, orientation, offset, &self.name)?,
                        stabilizer.paulis,
                    ))
                })
            })
            .chain(self.logical_rows.iter().map(|row| {
                materialize_summary_row(
                    &row.signed,
                    &row.support,
                    &self.ports,
                    zx,
                    orientation,
                    offset,
                    &self.name,
                )
                .map(|stabilizer| (StabilizerRowKind::Logical, stabilizer.paulis))
            }))
            .collect::<Result<Vec<_>, ModuleCertificationError>>()?;
        let adjustment_basis = self
            .adjustments
            .iter()
            .map(|row| {
                let stabilizer = materialize_summary_row(
                    &row.signed,
                    &row.support,
                    &self.ports,
                    zx,
                    orientation,
                    offset,
                    &self.name,
                )?;
                let mut paulis = stabilizer.paulis;
                zx.clear_cross_centers(std::slice::from_mut(&mut paulis));
                Ok(paulis)
            })
            .collect::<Result<Vec<_>, ModuleCertificationError>>()?;
        zx.stabilizers_from_composed_basis(
            &external_basis,
            &adjustment_basis,
            &cached_prefix,
            self.limits,
        )
        .map_err(crate::RuntimeBasisError::from)
        .and_then(|stabilizers| {
            // C0 / LIM-027: leaf certification alone does not establish
            // closure after composition. Check the presented readouts,
            // including selective fills, before using them for decoding.
            stabilizers.validate_measurements_close_before_outputs_with_limits(self.limits)?;
            Ok(stabilizers)
        })
        .map_err(|source| match source {
            crate::RuntimeBasisError::Stabilizer(crate::StabilizerError::ResourceLimited {
                phase,
                observed,
                limit,
            }) => resource_limit(&self.name, phase, observed, limit),
            source => ModuleCertificationError::Runtime {
                module: self.name.clone(),
                source,
            },
        })
    }

    fn port_index(&self, name: &str) -> Option<usize> {
        self.ports.iter().position(|port| port.name == name)
    }
}

fn transformed_row_kind(
    kind: &StabilizerRowKind,
    orientation: ModuleOrientation,
    translation: IVec3,
    module: &str,
) -> Result<StabilizerRowKind, ModuleCertificationError> {
    let mut kind = kind.clone();
    if let StabilizerRowKind::SelectiveFixing { targets } = &mut kind {
        for target in targets {
            target.pos = orientation
                .try_transform_position(target.pos, translation)
                .map_err(|source| ModuleCertificationError::Graph {
                    module: module.to_string(),
                    source,
                })?;
        }
    }
    Ok(kind)
}

fn materialize_summary_row(
    signed: &PhasedPauliString,
    support: &PositionedSupport,
    ports: &[QuantumPort],
    zx: &ZXGraph,
    orientation: ModuleOrientation,
    translation: IVec3,
    module: &str,
) -> Result<Stabilizer, ModuleCertificationError> {
    let mut support = support
        .transformed(orientation, translation)
        .map_err(|source| ModuleCertificationError::Graph {
            module: module.to_string(),
            source,
        })?;
    for (column, port) in ports.iter().enumerate() {
        let pauli = signed.paulis.get(column);
        if pauli == Pauli::I {
            continue;
        }
        let position = orientation
            .try_transform_position(port.position, translation)
            .map_err(|source| ModuleCertificationError::Graph {
                module: module.to_string(),
                source,
            })?;
        if let Some(existing) = support.nodes.insert(position, pauli)
            && existing != pauli
        {
            return Err(invalid(
                module,
                &format!(
                    "certificate support repeats public Port {position}: interior {existing}, boundary {pauli}"
                ),
            ));
        }
    }
    support.materialize(zx, signed.phase()).ok_or_else(|| {
        invalid(
            module,
            "composed certificate support does not fit the linked graph",
        )
    })
}

#[derive(Clone)]
struct BindSeam {
    endpoint: InstancePort,
    block: IVec3,
    signature: ModulePortSignature,
    hadamard: bool,
    transpose: bool,
}

#[derive(Default)]
struct ChildRelation {
    /// Columns precede `endpoints` and name ports in the parent's interface.
    parent_ports: Vec<usize>,
    endpoints: Vec<InstancePort>,
    rows: Vec<SupportedRow>,
    logical_rows: Vec<SupportedRow>,
    certificate_rows: Vec<SupportedRow>,
    adjustments: Vec<SupportedRow>,
    measurements: Vec<SummaryMeasurement>,
    discharged_measurements: Vec<SummaryMeasurement>,
    protected: Vec<ProtectedSupport>,
    frontier: Vec<SupportTarget>,
    protected_groups: Vec<ProtectedGroup>,
}

impl ChildRelation {
    fn width(&self) -> usize {
        self.parent_ports.len() + self.endpoints.len()
    }

    fn append(&mut self, other: Self, mapping: &[usize], width: usize) {
        for (target, rows) in [
            (&mut self.rows, other.rows),
            (&mut self.logical_rows, other.logical_rows),
            (&mut self.certificate_rows, other.certificate_rows),
            (&mut self.adjustments, other.adjustments),
        ] {
            target.extend(rows.into_iter().map(|mut row| {
                row.signed = embed_signed_row(&row.signed, mapping, width);
                row
            }));
        }
        for (target, rows) in [
            (&mut self.measurements, other.measurements),
            (
                &mut self.discharged_measurements,
                other.discharged_measurements,
            ),
        ] {
            target.extend(rows.into_iter().map(|mut row| {
                row.signed = embed_signed_row(&row.signed, mapping, width);
                row
            }));
        }
        self.parent_ports.extend(other.parent_ports);
        self.endpoints.extend(other.endpoints);
        self.protected.extend(other.protected);
        self.frontier.extend(other.frontier);
        self.protected_groups.extend(other.protected_groups);
    }

    fn retire_completed(
        &mut self,
        completed: &mut Self,
        obligations: &FxHashSet<SupportTarget>,
        module: &str,
    ) -> Result<(), ModuleCertificationError> {
        // Boundary-zero certificate rows complete the final physical rank, but
        // cannot change a later seam equation. Keep their witnesses off the live table.
        completed.certificate_rows.extend(
            self.certificate_rows
                .extract_if(.., |row| row.signed.paulis.is_identity())
                .map(|mut row| {
                    row.signed.paulis = PauliString::new(0);
                    row
                }),
        );
        // A closed adjustment that is zero on every pending obligation cannot
        // change a future seam solve. Retain it only for final presentation.
        completed.adjustments.extend(
            self.adjustments
                .extract_if(.., |row| {
                    row.signed.paulis.is_identity()
                        && !row
                            .support
                            .nodes
                            .keys()
                            .any(|&position| obligations.contains(&SupportTarget::Node(position)))
                        && !row.support.edges.keys().any(|&(left, right)| {
                            obligations.contains(&SupportTarget::Edge(left, right))
                                || obligations.contains(&SupportTarget::Edge(right, left))
                        })
                })
                .map(|mut row| {
                    row.signed.paulis = PauliString::new(0);
                    row
                }),
        );
        resize_discharged_boundaries(&mut self.discharged_measurements, 0, module)?;
        completed
            .discharged_measurements
            .append(&mut self.discharged_measurements);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SupportTarget {
    Node(IVec3),
    Edge(IVec3, IVec3),
}

fn support_target_key(target: &SupportTarget) -> (u8, [i32; 3], [i32; 3]) {
    match target {
        SupportTarget::Node(position) => (0, position.to_array(), [0; 3]),
        SupportTarget::Edge(left, right) => (1, left.to_array(), right.to_array()),
    }
}

#[derive(Debug, Clone, Copy)]
struct ProtectedSupport {
    target: SupportTarget,
    forbidden: Option<Pauli>,
}

#[derive(Debug, Clone)]
struct ProtectedGroup {
    targets: Vec<SupportTarget>,
    permitted_support: Vec<Vec<Pauli>>,
}

impl ProtectedGroup {
    fn transformed(
        self,
        orientation: ModuleOrientation,
        translation: IVec3,
    ) -> Result<Self, crate::BlockGraphError> {
        Ok(Self {
            targets: self
                .targets
                .into_iter()
                .map(|target| target.transformed(orientation, translation))
                .collect::<Result<_, _>>()?,
            permitted_support: self.permitted_support,
        })
    }
}

fn semantic_protected_supports(module: &BlockGraph, body: &BlockGraph) -> Vec<ProtectedSupport> {
    let mut protected = HashMap::<SupportTarget, Option<Pauli>>::new();
    let mut insert = |target, forbidden| {
        protected
            .entry(target)
            .and_modify(|current| {
                if current.is_none() {
                    *current = forbidden;
                }
            })
            .or_insert(forbidden);
    };
    for block in body.blocks() {
        if let BlockKind::Selective(kind) = block.kind() {
            let forbidden = match kind {
                crate::SelectiveKind::XY => Pauli::Z,
                crate::SelectiveKind::XZ => Pauli::Y,
                crate::SelectiveKind::YZ => Pauli::X,
            };
            insert(SupportTarget::Node(block.pos()), Some(forbidden));
        }
    }
    for region in module.local_body().branch_definitions() {
        insert(SupportTarget::Node(region.target), None);
        for cut in region
            .incoming_for(false)
            .iter()
            .chain(region.incoming_for(true))
        {
            insert(SupportTarget::Edge(cut.past, cut.inside), None);
        }
    }
    let mut protected = protected
        .into_iter()
        .map(|(target, forbidden)| ProtectedSupport { target, forbidden })
        .collect::<Vec<_>>();
    protected.sort_unstable_by_key(|constraint| support_target_key(&constraint.target));
    protected
}

/// Positioned support that must survive certificate reduction, but does not
/// constrain which seam adjustments are legal.
fn semantic_frontier_supports(
    module: &BlockGraph,
    body: &BlockGraph,
) -> Result<Vec<SupportTarget>, ModuleCertificationError> {
    let mut frontier = body
        .blocks()
        .filter(|block| block.kind().is_t())
        .map(|block| SupportTarget::Node(block.pos()))
        .collect::<FxHashSet<_>>();
    for action in body.actions() {
        match action {
            Action::Measure { target, .. } => {
                let target = match target {
                    crate::MeasureTarget::Node(position) => SupportTarget::Node(position),
                    crate::MeasureTarget::Edge { src, dir } => SupportTarget::Edge(
                        src,
                        checked_add_position(src, dir.to_ivec3()).map_err(|source| {
                            ModuleCertificationError::Graph {
                                module: module.name.clone(),
                                source,
                            }
                        })?,
                    ),
                };
                frontier.insert(target);
            }
            Action::Resolve { target, .. } | Action::Branch { target, .. } => {
                frontier.insert(SupportTarget::Node(target));
            }
            Action::Feedback { targets, .. } => {
                for target in targets {
                    frontier.insert(match target.direction {
                        Some(dir) => SupportTarget::Edge(
                            target.target,
                            checked_add_position(target.target, dir.to_ivec3()).map_err(
                                |source| ModuleCertificationError::Graph {
                                    module: module.name.clone(),
                                    source,
                                },
                            )?,
                        ),
                        None => SupportTarget::Node(target.target),
                    });
                }
            }
            Action::Let { .. } | Action::DiscardIf(_) => {}
        }
    }
    for region in module.local_body().branch_definitions() {
        frontier.insert(SupportTarget::Node(region.target));
        frontier.extend(
            region
                .incoming_for(false)
                .iter()
                .chain(region.incoming_for(true))
                .map(|cut| SupportTarget::Edge(cut.past, cut.inside)),
        );
    }
    let mut frontier = frontier.into_iter().collect::<Vec<_>>();
    frontier.sort_unstable_by_key(support_target_key);
    Ok(frontier)
}

fn semantic_protected_groups(
    body: &BlockGraph,
    measurements: &[SummaryMeasurement],
    zx: &ZXGraph,
    limits: ModuleCertificationLimits,
    module: &str,
) -> Result<Vec<ProtectedGroup>, ModuleCertificationError> {
    measurements
        .iter()
        .filter_map(|measurement| {
            let targets = measurement.kind.selective_fixing_targets();
            (targets.len() > 1).then_some(targets)
        })
        .map(|targets| {
            let positions = targets.iter().map(|target| target.pos).collect::<Vec<_>>();
            let kinds = positions
                .iter()
                .map(|&position| {
                    let Some(BlockKind::Selective(kind)) =
                        body.get_block(position).map(Block::kind)
                    else {
                        return Err(invalid(module, "selective fixing target is not selective"));
                    };
                    Ok(kind)
                })
                .collect::<Result<Vec<_>, ModuleCertificationError>>()?;
            let domain = zx
                .action_graph()
                .resolve_value_domain_bounded_with_limits(
                    &positions,
                    limits.max_guarded_domain_size,
                    limits.boolean_limits(),
                )
                .map_err(|error| {
                    match error
                        .into_stabilizer("selective value domain", limits.max_guarded_domain_size)
                    {
                        crate::StabilizerError::ResourceLimited {
                            phase,
                            observed,
                            limit,
                        } => resource_limit(module, phase, observed, limit),
                        source => ModuleCertificationError::Graph {
                            module: module.to_string(),
                            source: source.into(),
                        },
                    }
                })?;
            let mut permitted_support = domain
                .values()
                .iter()
                .map(|values| {
                    kinds
                        .iter()
                        .zip(values)
                        .map(|(kind, &value)| {
                            Pauli::from(if value {
                                kind.pauli_if_true()
                            } else {
                                kind.pauli_if_false()
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let reachable = permitted_support.iter().cloned().collect::<FxHashSet<_>>();
            permitted_support.retain(|support| {
                reachable.contains(
                    &support
                        .iter()
                        .zip(targets)
                        .map(|(&pauli, target)| pauli ^ target.forbidden)
                        .collect::<Vec<_>>(),
                )
            });
            Ok(ProtectedGroup {
                targets: positions.into_iter().map(SupportTarget::Node).collect(),
                permitted_support,
            })
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum SupportConstraint {
    Boundary(usize, Pauli),
    Target(SupportTarget, Pauli),
}

#[derive(Debug, Clone)]
struct SupportedRow {
    signed: PhasedPauliString,
    support: PositionedSupport,
    witness: FlowWitness,
}

#[derive(Debug, Clone)]
struct SummaryMeasurement {
    name: String,
    kind: StabilizerRowKind,
    signed: PhasedPauliString,
    support: PositionedSupport,
    witness: FlowWitness,
    target: SupportTarget,
    observable: Pauli,
    self_readers: Vec<SupportTarget>,
    owned_positions: FxHashSet<IVec3>,
}

/// Sparse full-surface support. Keeping positions rather than a hierarchy-wide
/// dense row makes instance transforms O(surface weight), and connected Port
/// support can be removed before same-position parent blocks are merged.
#[derive(Debug, Clone, Default)]
struct PositionedSupport {
    nodes: FxHashMap<IVec3, Pauli>,
    /// Directed edge support in the first endpoint's Pauli frame.
    edges: FxHashMap<(IVec3, IVec3), Pauli>,
}

#[derive(Clone, Copy)]
struct SupportSeam {
    edge: (IVec3, IVec3),
    boundary_to_edge_hadamard: bool,
}

impl PositionedSupport {
    fn from_stabilizer(stabilizer: &Stabilizer, zx: &ZXGraph) -> Self {
        // Cross centers are ORs of incident support, not independent GF(2) bits.
        // Keep raw support through composition and reconstruct at materialization.
        let mut raw = stabilizer.paulis.clone();
        zx.clear_cross_centers(std::slice::from_mut(&mut raw));
        Self {
            nodes: stabilizer
                .interior_nodes
                .iter()
                .filter(|(position, _)| !stabilizer.port_stabilizer.contains_key(position))
                .filter_map(|(&p, _)| {
                    let pauli = raw.get(zx.node_at(p).expect("surface node exists").id);
                    (pauli != Pauli::I).then_some((p, pauli))
                })
                .collect(),
            edges: stabilizer
                .interior_edges
                .iter()
                .map(|(&edge, &pauli)| (edge, pauli))
                .collect(),
        }
    }

    fn multiply_assign(&mut self, other: &Self) {
        debug_assert!(
            self.edges.keys().all(|&(left, right)| {
                !other.edges.contains_key(&(right, left)) || left == right
            }),
            "positioned support edge orientation mismatch"
        );
        xor_pauli_maps(&mut self.nodes, &other.nodes);
        xor_pauli_maps(&mut self.edges, &other.edges);
    }

    fn get(&self, target: SupportTarget) -> Pauli {
        match target {
            SupportTarget::Node(position) => self.nodes.get(&position).copied().unwrap_or(Pauli::I),
            SupportTarget::Edge(left, right) => self
                .edges
                .get(&(left, right))
                .or_else(|| self.edges.get(&(right, left)))
                .copied()
                .unwrap_or(Pauli::I),
        }
    }

    fn weight(&self) -> usize {
        self.nodes.len() + self.edges.len()
    }

    fn strip_ports(&mut self, ports: &FxHashSet<IVec3>, retained: &[SupportTarget]) {
        self.nodes.retain(|position, _| !ports.contains(position));
        self.edges.retain(|&(left, right), _| {
            (!ports.contains(&left) && !ports.contains(&right))
                || retained.contains(&SupportTarget::Edge(left, right))
                || retained.contains(&SupportTarget::Edge(right, left))
        });
    }

    fn transformed(
        &self,
        orientation: ModuleOrientation,
        translation: IVec3,
    ) -> Result<Self, crate::BlockGraphError> {
        let transform = |position| orientation.try_transform_position(position, translation);
        Ok(Self {
            nodes: self
                .nodes
                .iter()
                .map(|(&position, &pauli)| Ok((transform(position)?, pauli)))
                .collect::<Result<_, crate::BlockGraphError>>()?,
            edges: self
                .edges
                .iter()
                .map(|(&(left, right), &pauli)| Ok(((transform(left)?, transform(right)?), pauli)))
                .collect::<Result<_, crate::BlockGraphError>>()?,
        })
    }

    fn add_seam(&mut self, seam: SupportSeam, mut pauli: Pauli) -> u8 {
        if pauli == Pauli::I {
            return 0;
        }
        let phase = 2 * u8::from(seam.boundary_to_edge_hadamard && pauli == Pauli::Y);
        if seam.boundary_to_edge_hadamard {
            pauli = pauli.flip();
        }
        self.edges.remove(&(seam.edge.1, seam.edge.0));
        self.edges.insert(seam.edge, pauli);
        phase
    }

    fn materialize(&self, zx: &ZXGraph, mut phase: u8) -> Option<Stabilizer> {
        let mut row = PauliString::new(zx.total_ids());
        for (&position, &pauli) in &self.nodes {
            row.set(zx.node_at(position)?.id, pauli);
        }
        for (&(source, target), &pauli_at_source) in &self.edges {
            let source_id = zx.node_at(source)?.id;
            let target_id = zx.node_at(target)?.id;
            let forward = zx.edge_id(source_id, target_id)?;
            let reverse = zx.edge_id(target_id, source_id)?;
            let edge = &zx.edges()[forward - zx.nodes().len()];
            let mut stored = pauli_at_source;
            if source_id > target_id && edge.hadamard {
                if stored == Pauli::Y {
                    phase = (phase + 2) % 4;
                }
                stored = stored.flip();
            }
            row.set(forward, stored);
            row.set(reverse, stored);
        }
        // A spatial rotation can recolor a degree-two identity spider, changing
        // which component was stored as raw node support. Recover its native
        // component from the broadcast on all incident arms before rebuilding
        // crossing centers. Isolated nodes retain their explicit raw support.
        for node in zx
            .nodes()
            .iter()
            .filter(|node| matches!(node.kind, crate::NodeKind::X | crate::NodeKind::Z))
        {
            let Some(neighbors) = zx
                .neighbors(node.id)
                .filter(|neighbors| !neighbors.is_empty())
            else {
                continue;
            };
            let native = node.kind.cross_pauli().flip();
            let broadcast = neighbors.iter().all(|&neighbor| {
                let edge = zx.edge_between(node.id, neighbor).expect("incident edge");
                let axis = if edge.hadamard && node.id > neighbor {
                    native.flip()
                } else {
                    native
                };
                row.get(edge.id) & axis
            });
            let current = row.get(node.id);
            if (current & native) != broadcast {
                row.set(node.id, current ^ native);
            }
        }
        zx.reconstruct_cross_center(std::slice::from_mut(&mut row));
        Some(zx.pauli_string_to_stabilizer_with_row_phase(row, phase))
    }
}

impl SupportTarget {
    fn transformed(
        self,
        orientation: ModuleOrientation,
        translation: IVec3,
    ) -> Result<Self, crate::BlockGraphError> {
        let transform = |position| orientation.try_transform_position(position, translation);
        Ok(match self {
            Self::Node(position) => Self::Node(transform(position)?),
            Self::Edge(left, right) => Self::Edge(transform(left)?, transform(right)?),
        })
    }
}

impl ProtectedSupport {
    fn transformed(
        self,
        orientation: ModuleOrientation,
        translation: IVec3,
    ) -> Result<Self, crate::BlockGraphError> {
        Ok(Self {
            target: self.target.transformed(orientation, translation)?,
            forbidden: self.forbidden,
        })
    }
}

fn stabilizer_phase(stabilizer: &Stabilizer) -> u8 {
    let yy = stabilizer
        .interior_edges
        .values()
        .filter(|&&pauli| pauli == Pauli::Y)
        .count()
        % 2
        == 1;
    2 * u8::from(stabilizer.sign ^ yy)
}

#[derive(Clone)]
struct ModuleGeometry {
    ports: Vec<QuantumPort>,
    signatures: Vec<ModulePortSignature>,
}

#[derive(Clone, Copy)]
struct PortGeometry<'a> {
    instance: &'a ModuleInstance,
    port: &'a QuantumPort,
    signature: ModulePortSignature,
}

impl BlockGraph {
    /// Builds the root's signed quantum summary without flattening child bodies.
    /// Native concurrency defaults to the available cores capped at four.
    ///
    /// # Errors
    ///
    /// Returns an error if validation, certification, or a resource limit fails.
    pub fn summarize_root(
        &self,
        limits: ModuleCertificationLimits,
    ) -> Result<Arc<ModuleSummary>, ModuleCertificationError> {
        self.summarize_root_with_jobs(limits, default_module_jobs())
    }

    /// Builds the root summary using at most `jobs` concurrent module workers.
    ///
    /// Definitions at the same dependency depth run in parallel on native
    /// targets. WebAssembly remains serial.
    ///
    /// # Errors
    ///
    /// Returns an error if validation, certification, or a resource limit fails.
    pub fn summarize_root_with_jobs(
        &self,
        limits: ModuleCertificationLimits,
        jobs: NonZeroUsize,
    ) -> Result<Arc<ModuleSummary>, ModuleCertificationError> {
        if !self.has_module_structure() {
            let source = self
                .clone_local_definition()
                .with_inferred_interface()
                .map_err(|error| ModuleCertificationError::Graph {
                    module: self.name.clone(),
                    source: crate::BlockGraphError::ModuleSource(Arc::new(error)),
                })?;
            return source.summarize_root_with_jobs(limits, jobs);
        }
        self.validate_with_limits(limits)
            .map_err(|error| ModuleCertificationError::Graph {
                module: self.name.clone(),
                source: crate::BlockGraphError::ModuleSource(Arc::new(error)),
            })?;
        summarize_program(self, limits, jobs)
    }

    /// Physical endpoints of direct child-to-child pipes in declaration order.
    ///
    /// # Panics
    ///
    /// Panics if validated module geometry is internally inconsistent.
    pub fn direct_pipe_endpoints(&self, module: &str) -> Option<Vec<(IVec3, IVec3)>> {
        let definition = self.module(module)?;
        let mut memo = HashMap::new();
        module_geometry(self, module, &mut memo).expect("validated module geometry remains valid");
        Some(direct_pipe_endpoints(definition, &memo))
    }

    /// Physical direct-pipe endpoints for every module, sharing geometry work.
    ///
    /// # Panics
    ///
    /// Panics if validated module geometry is internally inconsistent.
    pub fn direct_pipe_endpoints_by_module(&self) -> HashMap<String, Vec<(IVec3, IVec3)>> {
        let mut memo = HashMap::new();
        for module in self.modules() {
            module_geometry(self, &module.name, &mut memo)
                .expect("validated module geometry remains valid");
        }
        self.modules()
            .map(|module| (module.name.clone(), direct_pipe_endpoints(module, &memo)))
            .collect()
    }
}

fn direct_pipe_endpoints(
    definition: &BlockGraph,
    geometry: &HashMap<String, ModuleGeometry>,
) -> Vec<(IVec3, IVec3)> {
    let children = definition
        .instances
        .iter()
        .map(|instance| {
            (
                instance.name.as_str(),
                (instance, geometry[&instance.definition].clone()),
            )
        })
        .collect::<HashMap<_, _>>();
    let position = |endpoint| {
        let port = validated_port(&children, endpoint);
        exposed_position(port.instance, port.port, port.signature)
            .expect("validated direct endpoint position fits")
    };
    definition
        .quantum_connections
        .iter()
        .filter_map(|connection| match connection {
            QuantumConnection::Pipe { output, input, .. } => {
                Some((position(output), position(input)))
            }
            _ => None,
        })
        .collect()
}

impl BlockGraph {
    /// Checks the whole source hierarchy's conservative expansion budgets.
    ///
    /// # Errors
    ///
    /// Returns typed declaration, cycle, graph, or resource-limit errors.
    pub fn validate_resource_limits(
        &self,
        limits: ModuleCertificationLimits,
    ) -> Result<(), crate::BlockGraphError> {
        if self.has_module_structure() {
            return self
                .validate_hierarchy_resource_limits(limits)
                .map_err(|error| crate::BlockGraphError::ModuleSource(Arc::new(error)));
        }
        self.validate_local_resource_limits(limits)
    }

    /// Checks authored geometry size before cloning or enumerating occupied cells.
    /// Both branch arms count; cube footprints use their cached height in cells.
    /// This checks `max_expanded_blocks` and `max_occupied_cells` only.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::StabilizerError::ResourceLimited`] wrapped in
    /// [`crate::BlockGraphError::Stabilizer`] when a count exceeds its limit or
    /// cannot fit in `usize`.
    pub fn validate_local_resource_limits(
        &self,
        limits: ModuleCertificationLimits,
    ) -> Result<(), crate::BlockGraphError> {
        source_expansion_size(self, limits)
            .map(|_| ())
            .map_err(Into::into)
    }
}

fn add_expansion_size(
    size: &mut [usize; 3],
    amount: [usize; 3],
    limits: ModuleCertificationLimits,
) -> Result<(), crate::StabilizerError> {
    for ((current, amount), (phase, limit)) in size.iter_mut().zip(amount).zip([
        ("expanded blocks", limits.max_expanded_blocks),
        ("occupied footprint cells", limits.max_occupied_cells),
        ("expanded module instances", limits.max_expanded_instances),
    ]) {
        let next = current.checked_add(amount);
        if next.is_none_or(|next| next > limit) {
            return Err(crate::StabilizerError::ResourceLimited {
                phase,
                observed: next.unwrap_or(usize::MAX),
                limit,
            });
        }
        *current = next.expect("checked size fits");
    }
    Ok(())
}

fn source_expansion_size(
    source: &BlockGraph,
    limits: ModuleCertificationLimits,
) -> Result<[usize; 3], crate::StabilizerError> {
    let mut size = [0usize; 3];
    // The shown arm already lives in the body. Count its authored blocks only
    // when they differ from that body, so even an unvalidated source is bounded.
    let authored = source.branch_definitions().iter().flat_map(|region| {
        region.arm(!region.shown_true()).blocks().chain(
            region
                .shown_arm()
                .blocks()
                .filter(|block| source.get_block(block.pos()) != Some(*block)),
        )
    });
    for block in source.blocks().chain(authored) {
        let cells = match block.kind() {
            BlockKind::Walking(kind) => Some(kind.movement_3d()),
            BlockKind::PatchRotation(kind) => Some(kind.movement_3d()),
            _ => None,
        }
        .map_or_else(
            || block.height_cells() as usize,
            |movement| {
                // Moving blocks reserve their axis-aligned start/end bounding box.
                movement
                    .to_array()
                    .into_iter()
                    .map(|delta| delta.unsigned_abs() as usize + 1)
                    .product()
            },
        );
        add_expansion_size(&mut size, [1, cells, 0], limits)?;
    }
    Ok(size)
}

/// Count each definition once, then add its cached size for each instance edge.
/// Both branch arms and Ports consumed by seams count toward the allocation bound.
pub(crate) fn preflight_program_expansion(
    program: &BlockGraph,
    limits: ModuleCertificationLimits,
) -> Result<(), ModuleError> {
    let mut sizes = HashMap::<&str, [usize; 3]>::new();
    for module in module_postorder(program) {
        let error = |source| {
            let crate::StabilizerError::ResourceLimited {
                phase,
                observed,
                limit,
            } = source
            else {
                unreachable!("size counting only reports resource limits")
            };
            program_geometry_error(
                &module.name,
                resource_limit(&module.name, phase, observed, limit),
            )
        };
        let mut size = source_expansion_size(module.local_body(), limits).map_err(error)?;
        add_expansion_size(&mut size, [0, 0, 1], limits).map_err(error)?;
        for instance in &module.instances {
            add_expansion_size(&mut size, sizes[instance.definition.as_str()], limits)
                .map_err(error)?;
        }
        sizes.insert(&module.name, size);
    }
    Ok(())
}

pub(crate) fn validate_program_geometry(program: &BlockGraph) -> Result<(), ModuleError> {
    for module in module_postorder(program)
        .into_iter()
        .filter(|module| !module.instances.is_empty())
    {
        validate_composite_geometry(program, module)
            .map_err(|error| program_geometry_error(&module.name, error))?;
    }
    let mut memo = HashMap::new();
    for module in program.modules() {
        module_geometry(program, &module.name, &mut memo)?;
    }
    Ok(())
}

fn module_postorder(program: &BlockGraph) -> Vec<&BlockGraph> {
    module_postorder_from(
        program,
        program.modules().map(|module| module.name.as_str()),
    )
}

fn module_postorder_from<'a>(
    program: &'a BlockGraph,
    roots: impl IntoIterator<Item = &'a str>,
) -> Vec<&'a BlockGraph> {
    let definitions = program
        .modules()
        .map(|module| (module.name.as_str(), module))
        .collect::<HashMap<_, _>>();
    let mut seen = FxHashSet::default();
    let mut ordered = Vec::new();
    for root in roots {
        let mut pending = vec![(root, false)];
        while let Some((name, exiting)) = pending.pop() {
            let module = definitions[name];
            if exiting {
                ordered.push(module);
            } else if seen.insert(name) {
                pending.push((name, true));
                pending.extend(
                    module
                        .instances
                        .iter()
                        .rev()
                        .map(|instance| (instance.definition.as_str(), false)),
                );
            }
        }
    }
    ordered
}

fn module_geometry(
    program: &BlockGraph,
    name: &str,
    memo: &mut HashMap<String, ModuleGeometry>,
) -> Result<ModuleGeometry, ModuleError> {
    if let Some(geometry) = memo.get(name) {
        return Ok(geometry.clone());
    }
    for module in module_postorder_from(program, [name]) {
        if memo.contains_key(&module.name) {
            continue;
        }
        let name = module.name.as_str();
        let geometry = if module.instances.is_empty() {
            let body = module.copy_local_geometry().fix_shadowed_faces();
            let signatures = module
                .interface
                .quantum_ports
                .iter()
                .map(|port| port_signature(name, &body, port.position))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| program_geometry_error(name, error))?;
            ModuleGeometry {
                ports: module.interface.quantum_ports.clone(),
                signatures,
            }
        } else {
            let mut children = HashMap::new();
            for instance in &module.instances {
                let geometry = memo[&instance.definition].clone();
                children.insert(instance.name.as_str(), (instance, geometry));
            }

            let mut binds = Vec::new();
            for connection in &module.quantum_connections {
                match connection {
                    QuantumConnection::Input {
                        block,
                        input,
                        hadamard,
                    } => {
                        binds.push(geometry_bind_seam(
                            input, *block, *hadamard, true, &children,
                        ));
                    }
                    QuantumConnection::Output {
                        output,
                        block,
                        hadamard,
                    } => {
                        binds.push(geometry_bind_seam(
                            output, *block, *hadamard, false, &children,
                        ));
                    }
                    QuantumConnection::Pipe {
                        output,
                        input,
                        hadamard,
                    } => {
                        geometry_direct_seam(output, input, *hadamard, &children, name)
                            .map_err(|error| program_geometry_error(name, error))?;
                    }
                }
            }
            validate_bind_faces(module, &binds)
                .map_err(|error| program_geometry_error(name, error))?;
            let extra_pipes = binds
                .iter()
                .map(|bind| (bind.block, bind.signature.direction.as_udirection()))
                .collect::<Vec<_>>();
            let body = module
                .copy_local_geometry()
                .fix_shadowed_faces_with(&extra_pipes);
            validate_bind_bases(module, &body, &binds)
                .map_err(|error| program_geometry_error(name, error))?;
            ModuleGeometry {
                ports: module.interface.quantum_ports.clone(),
                signatures: parent_signatures(module, &body, &binds)
                    .map_err(|error| program_geometry_error(name, error))?,
            }
        };
        memo.insert(name.to_string(), geometry);
    }
    Ok(memo[name].clone())
}

fn program_geometry_error(module: &str, error: ModuleCertificationError) -> ModuleError {
    ModuleError::InvalidGeometry {
        module: module.to_string(),
        source: error,
        span: None,
    }
}

/// Default worker count for summarizing or compiling a module hierarchy:
/// the machine's parallelism capped at 4, and always 1 on wasm.
pub fn default_module_jobs() -> NonZeroUsize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        const MAX_JOBS: NonZeroUsize = NonZeroUsize::new(4).expect("4 is nonzero");
        std::thread::available_parallelism().map_or(NonZeroUsize::MIN, |jobs| jobs.min(MAX_JOBS))
    }
    #[cfg(target_arch = "wasm32")]
    {
        NonZeroUsize::MIN
    }
}

/// Maps `f` over `items` across at most `jobs` scoped worker threads, keeping
/// the results in input order.
///
/// Chunked rather than work-stealing: the callers
/// map over module definitions, whose per-item cost is uneven but whose counts
/// are small.
///
/// wasm has no threads, so it always runs serially.
///
/// # Panics
///
/// Panics if `f` panics in a worker thread.
///
// ponytail: scoped workers are enough; add a pool only if spawn profiles hot.
pub fn map_jobs<T: Sync, R: Send>(
    items: &[T],
    jobs: NonZeroUsize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    #[cfg(target_arch = "wasm32")]
    let _ = jobs;
    #[cfg(not(target_arch = "wasm32"))]
    if items.len() > 1 && jobs.get() > 1 {
        let worker_count = jobs.get().min(items.len());
        let f = &f;
        let cancellation = crate::CancellationToken::current();
        return std::thread::scope(|scope| {
            let workers = (0..worker_count)
                .map(|worker| {
                    let start = items.len() * worker / worker_count;
                    let end = items.len() * (worker + 1) / worker_count;
                    let chunk = &items[start..end];
                    let cancellation = cancellation.clone();
                    scope.spawn(move || {
                        let run = || chunk.iter().map(f).collect::<Vec<_>>();
                        match cancellation {
                            Some(token) => token.scope(run),
                            None => run(),
                        }
                    })
                })
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .flat_map(|worker| worker.join().expect("map_jobs worker does not panic"))
                .collect()
        });
    }
    items.iter().map(f).collect()
}

fn summarize_program(
    program: &BlockGraph,
    limits: ModuleCertificationLimits,
    jobs: NonZeroUsize,
) -> Result<Arc<ModuleSummary>, ModuleCertificationError> {
    preflight_program_expansion(program, limits).map_err(|error| match error {
        ModuleError::InvalidGeometry { source, .. } => source,
        _ => unreachable!("size preflight only reports resource limits"),
    })?;
    let mut depths = HashMap::new();
    let mut levels: Vec<Vec<&BlockGraph>> = Vec::new();
    let mut guarded = false;
    for module in module_postorder_from(program, [program.root().name.as_str()]) {
        guarded |= module.local_body().has_continuing_branches();
        let depth = module
            .instances
            .iter()
            .map(|instance| depths[instance.definition.as_str()] + 1)
            .max()
            .unwrap_or(0);
        levels.resize_with(levels.len().max(depth + 1), Vec::new);
        levels[depth].push(module);
        depths.insert(module.name.as_str(), depth);
    }

    let mut summaries = HashMap::new();
    for level in levels {
        let results = map_jobs(&level, jobs, |&module| {
            summarize_definition(program, module, limits, &summaries, guarded)
        });
        for (module, summary) in level.iter().zip(results) {
            summaries.insert(module.name.clone(), Arc::new(summary?));
        }
    }
    Ok(Arc::clone(
        summaries
            .get(&program.root().name)
            .expect("dependency order includes the root module"),
    ))
}

fn summarize_definition(
    program: &BlockGraph,
    module: &BlockGraph,
    limits: ModuleCertificationLimits,
    summaries: &HashMap<String, Arc<ModuleSummary>>,
    guarded: bool,
) -> Result<ModuleSummary, ModuleCertificationError> {
    let projection = if guarded {
        let linked = crate::flatten_module_definition(program, module, "").map_err(|source| {
            ModuleCertificationError::Graph {
                module: module.name.clone(),
                source,
            }
        })?;
        let targets = linked
            .graph
            .branch_regions()
            .map_err(|source| ModuleCertificationError::Graph {
                module: module.name.clone(),
                source,
            })?
            .into_iter()
            .map(|region| region.target)
            .collect::<Vec<_>>();
        if !targets.is_empty() {
            let certify = || -> Result<(), crate::BlockGraphError> {
                let topology = crate::GuardedTopology::new(&linked.graph, limits)?;
                crate::GuardedSurfaceSpace::new(topology, &linked.sites, limits)?
                    .plan_readouts()
                    .map(drop)
            };
            certify().map_err(|source| ModuleCertificationError::Graph {
                module: module.name.clone(),
                source,
            })?;
            Some(Box::new(ModuleProjection {
                definition: module.clone_local_definition(),
                targets,
                limits,
            }))
        } else {
            None
        }
    } else {
        None
    };
    if module.instances.is_empty() {
        let mut summary = summarize_leaf(module, limits)?;
        if guarded {
            summary.projection = projection;
            return Ok(summary);
        }
        summary.branch_variants = branch_assignments(module, limits)?
            .into_iter()
            .map(|assignments| {
                let mut key = assignments
                    .iter()
                    .copied()
                    .filter(|(_, value)| !value)
                    .collect::<Vec<_>>();
                key.sort_unstable_by_key(|(position, value)| (position.to_array(), *value));
                let body = module
                    .local_body()
                    .project_branches_deferred(assignments.iter().copied())
                    .map_err(|source| ModuleCertificationError::Graph {
                        module: module.name.clone(),
                        source,
                    })?;
                Ok(ModuleBranchVariant {
                    assignments: key,
                    summary: Box::new(summarize_leaf_body(module, &body, limits)?),
                })
            })
            .collect::<Result<_, ModuleCertificationError>>()?;
        return Ok(summary);
    }
    let children = module
        .instances
        .iter()
        .map(|instance| {
            let summary = summaries
                .get(&instance.definition)
                .expect("dependency summary is ready");
            (instance.name.as_str(), (instance, Arc::clone(summary)))
        })
        .collect();
    let mut canonical = module.clone_local_definition();
    canonical.replace_local_body(module.local_body().canonical_true_branch_view().map_err(
        |source| ModuleCertificationError::Graph {
            module: module.name.clone(),
            source,
        },
    )?);
    let mut summary = summarize_composite(&canonical, module, &children, limits)?;
    if guarded {
        summary.projection = projection;
    } else {
        summary.branch_variants = summarize_composite_variants(module, summaries, limits)?;
    }
    Ok(summary)
}

fn branch_assignments(
    module: &BlockGraph,
    limits: ModuleCertificationLimits,
) -> Result<Vec<Vec<(IVec3, bool)>>, ModuleCertificationError> {
    let targets = module
        .local_body()
        .branch_regions()
        .map_err(|source| ModuleCertificationError::Graph {
            module: module.name.clone(),
            source,
        })?
        .into_iter()
        .map(|region| region.target)
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    if targets.len() > limits.max_guarded_domain_size {
        return Err(resource_limit(
            &module.name,
            "structural module alternatives",
            targets.len(),
            limits.max_guarded_domain_size,
        ));
    }
    Ok((0..targets.len())
        .map(|alternative| {
            targets
                .iter()
                .enumerate()
                .map(|(index, &target)| (target, index != alternative))
                .collect()
        })
        .collect())
}

fn summarize_composite_variants(
    module: &BlockGraph,
    summaries: &HashMap<String, Arc<ModuleSummary>>,
    limits: ModuleCertificationLimits,
) -> Result<Vec<ModuleBranchVariant>, ModuleCertificationError> {
    let local_assignments = branch_assignments(module, limits)?;
    let variant_count = local_assignments.len().saturating_add(
        module
            .instances
            .iter()
            .map(|instance| summaries[&instance.definition].branch_variants.len())
            .sum::<usize>(),
    );
    if variant_count > limits.max_guarded_domain_size {
        return Err(resource_limit(
            &module.name,
            "structural module alternatives",
            variant_count,
            limits.max_guarded_domain_size,
        ));
    }
    let mut variants = Vec::with_capacity(variant_count);
    let canonical_children = module
        .instances
        .iter()
        .map(|instance| {
            (
                instance.name.as_str(),
                (instance, Arc::clone(&summaries[&instance.definition])),
            )
        })
        .collect::<HashMap<_, _>>();
    for assignments in local_assignments {
        let body = module
            .local_body()
            .project_branches_in_definition(assignments.iter().copied())
            .map_err(|source| ModuleCertificationError::Graph {
                module: module.name.clone(),
                source,
            })?;
        let mut projected = module.clone_local_definition();
        projected.replace_local_body(body);
        variants.push(ModuleBranchVariant {
            assignments: assignments
                .into_iter()
                .filter(|(_, value)| !value)
                .collect(),
            summary: Box::new(summarize_composite(
                &projected,
                module,
                &canonical_children,
                limits,
            )?),
        });
    }

    let mut canonical = module.clone_local_definition();
    canonical.replace_local_body(module.local_body().canonical_true_branch_view().map_err(
        |source| ModuleCertificationError::Graph {
            module: module.name.clone(),
            source,
        },
    )?);
    for (selected, instance) in module.instances.iter().enumerate() {
        let child = &summaries[&instance.definition];
        for alternative in &child.branch_variants {
            let children = module
                .instances
                .iter()
                .enumerate()
                .map(|(index, instance)| {
                    let summary = if index == selected {
                        Arc::new(alternative.summary.as_ref().clone())
                    } else {
                        Arc::clone(&summaries[&instance.definition])
                    };
                    (instance.name.as_str(), (instance, summary))
                })
                .collect::<HashMap<_, _>>();
            let mut assignments = alternative
                .assignments
                .iter()
                .map(|&(target, value)| {
                    instance
                        .try_transform_position(target)
                        .map(|target| (target, value))
                        .map_err(|source| ModuleCertificationError::Graph {
                            module: module.name.clone(),
                            source,
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            assignments.sort_unstable_by_key(|(position, value)| (position.to_array(), *value));
            variants.push(ModuleBranchVariant {
                assignments,
                summary: Box::new(summarize_composite(&canonical, module, &children, limits)?),
            });
        }
    }
    Ok(variants)
}

fn summarize_leaf(
    module: &BlockGraph,
    limits: ModuleCertificationLimits,
) -> Result<ModuleSummary, ModuleCertificationError> {
    let body = module
        .local_body()
        .canonical_true_branch_view()
        .map_err(|source| ModuleCertificationError::Graph {
            module: module.name.clone(),
            source,
        })?;
    summarize_leaf_body(module, &body, limits)
}

fn summarize_leaf_body(
    module: &BlockGraph,
    body: &BlockGraph,
    limits: ModuleCertificationLimits,
) -> Result<ModuleSummary, ModuleCertificationError> {
    let certificate = crate::program::certify_leaf_body(
        &module.name,
        body,
        &module.interface.quantum_ports,
        limits,
    )?;
    summarize_leaf_certificate(module, body, certificate, limits)
}

fn summarize_readouts(
    body: &BlockGraph,
    zx: &ZXGraph,
    stabilizers: &StabilizerGenerators,
    table: &crate::zx::ProjectedExternalTable,
    port_columns: &[usize],
) -> Vec<SummaryMeasurement> {
    let measurement_columns = zx.measurement_columns();
    let measurement_observables = zx
        .action_graph()
        .ordered_nodes()
        .filter_map(|node| {
            let Action::Measure { name, .. } = &node.action else {
                return None;
            };
            let observable = match node.measurement? {
                MeasurementObservable::Concrete(PauliBasis::X) => Pauli::X,
                MeasurementObservable::Concrete(PauliBasis::Z) => Pauli::Z,
                MeasurementObservable::Concrete(PauliBasis::Y)
                | MeasurementObservable::Selective(_) => Pauli::Y,
            };
            Some((name.as_str(), observable))
        })
        .collect::<HashMap<_, _>>();
    let owned_positions = body
        .blocks()
        .filter(|block| !block.kind().is_port())
        .map(Block::pos)
        .collect::<FxHashSet<_>>();
    // One basis per certificate, reduced against every generator below.
    let flow_basis = table.flow_witness_basis();
    stabilizers
        .generators
        .iter()
        .enumerate()
        .filter_map(|(index, generator)| {
            let (name, target, observable, self_readers) = match &generator.kind {
                StabilizerRowKind::Measurement { name } => (
                    name.clone(),
                    support_target(zx, measurement_columns[name.as_str()]),
                    measurement_observables[name.as_str()],
                    self_reader_targets(zx, name),
                ),
                StabilizerRowKind::SelectiveFixing { targets } => (
                    format!("#fix{index}"),
                    SupportTarget::Node(targets[0].pos),
                    Pauli::I,
                    Vec::new(),
                ),
                StabilizerRowKind::Logical => return None,
            };
            let support = PositionedSupport::from_stabilizer(&generator.stabilizer, zx);
            let mut raw = generator.stabilizer.paulis.clone();
            zx.clear_cross_centers(std::slice::from_mut(&mut raw));
            let witness = flow_basis
                .witness_for_row(&raw)
                .expect("certified generator belongs to the projected row space");
            Some(SummaryMeasurement {
                name,
                kind: generator.kind.clone(),
                signed: PhasedPauliString::new(
                    PauliString::from_terms(
                        port_columns.len(),
                        port_columns.iter().enumerate().map(|(target, &source)| {
                            (target, generator.stabilizer.paulis.get(source))
                        }),
                    ),
                    stabilizer_phase(&generator.stabilizer),
                ),
                support,
                witness,
                target,
                observable,
                self_readers,
                owned_positions: owned_positions.clone(),
            })
        })
        .collect::<Vec<_>>()
}

fn summarize_leaf_certificate(
    module: &BlockGraph,
    source: &BlockGraph,
    certificate: crate::LeafModuleCertificate,
    limits: ModuleCertificationLimits,
) -> Result<ModuleSummary, ModuleCertificationError> {
    let body = source.fix_shadowed_faces();
    let signatures = module
        .interface
        .quantum_ports
        .iter()
        .map(|port| port_signature(&module.name, &body, port.position))
        .collect::<Result<Vec<_>, _>>()?;
    let zx = &certificate.stabilizers.zx_graph;
    let rows = certificate
        .table
        .boundary_rows
        .iter()
        .map(|row| {
            let full = certificate.table.materialize(row);
            // Only support is consumed; `row.signed` already owns the certificate phase.
            let stabilizer = zx.materialize_stabilizer_with_sign(full.paulis, false);
            SupportedRow {
                signed: row.signed.clone(),
                support: PositionedSupport::from_stabilizer(&stabilizer, zx),
                witness: row.flow_witness.clone(),
            }
        })
        .collect::<Vec<_>>();
    let port_columns = module
        .interface
        .quantum_ports
        .iter()
        .map(|port| {
            zx.node_at(port.position)
                .expect("validated module port is a ZX node")
                .id
        })
        .collect::<Vec<_>>();
    let measurements = summarize_readouts(
        &body,
        zx,
        &certificate.stabilizers,
        &certificate.table,
        &port_columns,
    );
    let logical_rows = certificate
        .stabilizers
        .generators
        .iter()
        .filter(|generator| matches!(generator.kind, StabilizerRowKind::Logical))
        .map(|generator| SupportedRow {
            signed: PhasedPauliString::new(
                PauliString::from_terms(
                    port_columns.len(),
                    port_columns
                        .iter()
                        .enumerate()
                        .map(|(target, &source)| (target, generator.stabilizer.paulis.get(source))),
                ),
                stabilizer_phase(&generator.stabilizer),
            ),
            support: PositionedSupport::from_stabilizer(&generator.stabilizer, zx),
            witness: FlowWitness::default(),
        })
        .collect::<Vec<_>>();
    let protected_groups =
        semantic_protected_groups(&body, &measurements, zx, limits, &module.name)?;
    let (discharged_measurements, measurements): (Vec<_>, Vec<_>) = measurements
        .into_iter()
        .partition(measurement_can_discharge);
    let protected = semantic_protected_supports(module, &body);
    let frontier = semantic_frontier_supports(module, &body)?;
    let adjustments = adjustment_rows(
        zx,
        &certificate.table,
        &rows,
        &measurements,
        &protected,
        &frontier,
        module.interface.quantum_ports.len(),
    );
    let certificate_rows = retained_certificate_rows(
        zx,
        &certificate.table,
        &rows,
        module.interface.quantum_ports.len(),
    );
    Ok(ModuleSummary {
        name: module.name.clone(),
        limits,
        dependencies: HashMap::new(),
        ports: module.interface.quantum_ports.clone(),
        signatures,
        rows,
        logical_rows,
        certificate_rows,
        adjustments,
        measurements,
        discharged_measurements,
        protected,
        frontier,
        protected_groups,
        branch_variants: Vec::new(),
        projection: None,
    })
}

fn measurement_can_discharge(measurement: &SummaryMeasurement) -> bool {
    matches!(measurement.kind, StabilizerRowKind::Measurement { .. })
        && measurement.signed.paulis.is_identity()
        && support_target_is_owned(measurement.target, &measurement.owned_positions)
        && measurement
            .self_readers
            .iter()
            .all(|&target| support_target_is_owned(target, &measurement.owned_positions))
}

fn support_target_is_owned(target: SupportTarget, owned: &FxHashSet<IVec3>) -> bool {
    match target {
        SupportTarget::Node(position) => owned.contains(&position),
        SupportTarget::Edge(left, right) => owned.contains(&left) && owned.contains(&right),
    }
}

fn support_target(zx: &ZXGraph, column: usize) -> SupportTarget {
    if column < zx.nodes().len() {
        SupportTarget::Node(zx.nodes()[column].pos)
    } else {
        let edge = &zx.edges()[column - zx.nodes().len()];
        SupportTarget::Edge(zx.nodes()[edge.n1].pos, zx.nodes()[edge.n2].pos)
    }
}

fn self_reader_targets(zx: &ZXGraph, measurement: &str) -> Vec<SupportTarget> {
    let actions = zx.action_graph().ordered_nodes().collect::<Vec<_>>();
    let Some(start) = actions.iter().position(
        |node| matches!(&node.action, Action::Measure { name, .. } if name == measurement),
    ) else {
        return Vec::new();
    };
    let mut successors = vec![Vec::new(); actions.len()];
    for (from, to, _) in zx.action_graph().dependencies() {
        successors[from].push(to);
    }
    let mut seen = FxHashSet::default();
    let mut stack = successors[start].clone();
    while let Some(ordinal) = stack.pop() {
        if seen.insert(ordinal) {
            stack.extend(successors[ordinal].iter().copied());
        }
    }
    let mut targets = FxHashSet::default();
    for ordinal in seen {
        match &actions[ordinal].action {
            Action::Resolve { target, .. } | Action::Branch { target, .. } => {
                targets.insert(SupportTarget::Node(*target));
            }
            Action::Feedback {
                targets: feedbacks, ..
            } => {
                for feedback in feedbacks {
                    let node = zx
                        .node_at(feedback.target)
                        .expect("validated feedback target");
                    let neighbor = match feedback.direction {
                        Some(dir) => zx
                            .node_at(
                                checked_add_position(feedback.target, dir.to_ivec3())
                                    .expect("validated feedback edge"),
                            )
                            .map(|node| node.id),
                        None if node.kind == crate::NodeKind::Port => zx
                            .neighbors(node.id)
                            .and_then(|neighbors| (neighbors.len() == 1).then(|| neighbors[0])),
                        None => None,
                    };
                    targets.insert(match neighbor {
                        Some(neighbor) => support_target(
                            zx,
                            zx.edge_id(node.id, neighbor).expect("feedback wire exists"),
                        ),
                        None => SupportTarget::Node(feedback.target),
                    });
                }
            }
            Action::Let { .. } | Action::Measure { .. } | Action::DiscardIf(_) => {}
        }
    }
    targets.into_iter().collect()
}

fn retained_certificate_rows(
    zx: &ZXGraph,
    table: &crate::zx::ProjectedExternalTable,
    boundary_rows: &[SupportedRow],
    boundary_width: usize,
) -> Vec<SupportedRow> {
    let mut rows = boundary_rows.to_vec();
    rows.extend(table.closed_rows.iter().map(|row| SupportedRow {
        signed: PhasedPauliString::new(PauliString::new(boundary_width), row.signed.phase()),
        support: PositionedSupport::from_stabilizer(
            &zx.materialize_stabilizer_with_sign(row.signed.paulis.clone(), false),
            zx,
        ),
        witness: row.flow_witness.clone(),
    }));
    rows
}

fn adjustment_rows(
    zx: &ZXGraph,
    table: &crate::zx::ProjectedExternalTable,
    boundary_rows: &[SupportedRow],
    measurements: &[SummaryMeasurement],
    protected: &[ProtectedSupport],
    frontier: &[SupportTarget],
    boundary_width: usize,
) -> Vec<SupportedRow> {
    let mut rows = retained_certificate_rows(zx, table, boundary_rows, boundary_width);
    let mut targets = measurements
        .iter()
        .map(|measurement| measurement.target)
        .chain(
            measurements
                .iter()
                .flat_map(|measurement| measurement.self_readers.iter().copied()),
        )
        .chain(protected.iter().map(|constraint| constraint.target))
        .chain(frontier.iter().copied())
        .collect::<FxHashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    targets.sort_unstable_by_key(support_target_key);
    let rank = signed_gaussian_elimination(
        &mut rows,
        (0..boundary_width)
            .flat_map(|column| {
                [Pauli::X, Pauli::Z].map(move |axis| SupportConstraint::Boundary(column, axis))
            })
            .chain(targets.into_iter().flat_map(|target| {
                [Pauli::X, Pauli::Z].map(move |axis| SupportConstraint::Target(target, axis))
            })),
        |row, constraint| match *constraint {
            SupportConstraint::Boundary(column, axis) => row.signed.paulis.get(column) & axis,
            SupportConstraint::Target(target, axis) => row.support.get(target) & axis,
        },
    );
    rows.truncate(rank);
    rows
}

fn summarize_composite(
    module: &BlockGraph,
    semantic_module: &BlockGraph,
    children: &HashMap<&str, (&ModuleInstance, Arc<ModuleSummary>)>,
    limits: ModuleCertificationLimits,
) -> Result<ModuleSummary, ModuleCertificationError> {
    let parent_width = module.interface.quantum_ports.len();
    let mut relations = Vec::with_capacity(module.instances.len());
    let mut owners = HashMap::new();
    for instance in &module.instances {
        let summary = &children[instance.name.as_str()].1;
        let endpoints = summary
            .ports
            .iter()
            .map(|port| InstancePort {
                instance: instance.name.clone(),
                port: port.name.clone(),
            })
            .collect::<Vec<_>>();
        let owner = relations.len();
        for endpoint in &endpoints {
            owners.insert(endpoint.clone(), owner);
        }
        let port_positions = summary
            .ports
            .iter()
            .map(|port| port.position)
            .collect::<Vec<_>>();
        let port_position_set = port_positions.iter().copied().collect::<FxHashSet<_>>();
        let retained_targets = summary
            .measurements
            .iter()
            .chain(&summary.discharged_measurements)
            .flat_map(|measurement| {
                std::iter::once(measurement.target).chain(measurement.self_readers.iter().copied())
            })
            .collect::<Vec<_>>();
        let rows = instantiate_rows(
            &summary.rows,
            instance,
            &port_position_set,
            &retained_targets,
            &module.name,
        )?;
        let logical_rows = instantiate_rows(
            &summary.logical_rows,
            instance,
            &port_position_set,
            &retained_targets,
            &module.name,
        )?;
        let certificate_rows = instantiate_rows(
            &summary.certificate_rows,
            instance,
            &port_position_set,
            &retained_targets,
            &module.name,
        )?;
        let adjustments = instantiate_rows(
            &summary.adjustments,
            instance,
            &port_position_set,
            &retained_targets,
            &module.name,
        )?;
        let measurements = instantiate_measurements(
            &summary.measurements,
            instance,
            &port_positions,
            &retained_targets,
            &module.name,
        )?;
        let discharged_measurements = instantiate_measurements(
            &summary.discharged_measurements,
            instance,
            &port_positions,
            &retained_targets,
            &module.name,
        )?;
        let orientation = instance.orientation();
        let graph_error = |source| ModuleCertificationError::Graph {
            module: module.name.clone(),
            source,
        };
        let protected = summary
            .protected
            .iter()
            .map(|&target| {
                target
                    .transformed(orientation, instance.translation)
                    .map_err(graph_error)
            })
            .collect::<Result<Vec<_>, ModuleCertificationError>>()?;
        let frontier = summary
            .frontier
            .iter()
            .map(|&target| {
                target
                    .transformed(orientation, instance.translation)
                    .map_err(graph_error)
            })
            .collect::<Result<Vec<_>, ModuleCertificationError>>()?;
        let protected_groups = summary
            .protected_groups
            .iter()
            .cloned()
            .map(|group| {
                group
                    .transformed(orientation, instance.translation)
                    .map_err(graph_error)
            })
            .collect::<Result<Vec<_>, ModuleCertificationError>>()?;
        relations.push(Some(ChildRelation {
            parent_ports: Vec::new(),
            endpoints,
            rows,
            logical_rows,
            certificate_rows,
            adjustments,
            measurements,
            discharged_measurements,
            protected,
            frontier,
            protected_groups,
        }));
    }

    let mut binds = Vec::new();
    for connection in &module.quantum_connections {
        match connection {
            QuantumConnection::Input {
                block,
                input,
                hadamard,
            } => {
                binds.push(bind_seam(input, *block, *hadamard, true, children));
            }
            QuantumConnection::Output {
                output,
                block,
                hadamard,
            } => {
                binds.push(bind_seam(output, *block, *hadamard, false, children));
            }
            QuantumConnection::Pipe {
                output,
                input,
                hadamard,
            } => {
                let effective_hadamard =
                    direct_seam_hadamard(output, input, *hadamard, children, &module.name)?;
                let output_geometry = summarized_port(children, output);
                let input_geometry = summarized_port(children, input);
                let support_seam = SupportSeam {
                    edge: (
                        exposed_position(
                            output_geometry.instance,
                            output_geometry.port,
                            output_geometry.signature,
                        )?,
                        exposed_position(
                            input_geometry.instance,
                            input_geometry.port,
                            input_geometry.signature,
                        )?,
                    ),
                    boundary_to_edge_hadamard: output_geometry.signature.hadamard,
                };
                compose_child_seam(
                    (output, input),
                    effective_hadamard,
                    support_seam,
                    &mut relations,
                    &mut owners,
                    limits.max_frontier_width,
                    &module.name,
                )?;
            }
        }
    }

    ensure_composition_width(&module.name, parent_width, limits.max_frontier_width)?;
    validate_bind_faces(module, &binds)?;
    let seams = binds
        .iter()
        .map(|bind| ModuleSeam {
            block: bind.block,
            direction: bind.signature.direction,
        })
        .collect::<Vec<_>>();
    let extra_pipes = seams
        .iter()
        .map(|seam| (seam.block, seam.direction.as_udirection()))
        .collect::<Vec<_>>();
    let fixed_body = module
        .copy_local_geometry()
        .fix_shadowed_faces_with(&extra_pipes);
    validate_bind_bases(module, &fixed_body, &binds)?;
    let signatures = parent_signatures(module, &fixed_body, &binds)?;
    let parent_protected = semantic_protected_supports(semantic_module, &fixed_body);
    let parent_frontier = semantic_frontier_supports(semantic_module, &fixed_body)?;
    // Include obligations from all children, even those not admitted yet.
    let obligations = relations
        .iter()
        .flatten()
        .flat_map(|relation| {
            relation
                .measurements
                .iter()
                .flat_map(|measurement| {
                    std::iter::once(measurement.target)
                        .chain(measurement.self_readers.iter().copied())
                })
                .chain(relation.protected.iter().map(|support| support.target))
                .chain(
                    relation
                        .protected_groups
                        .iter()
                        .flat_map(|group| group.targets.iter().copied()),
                )
        })
        .chain(parent_protected.iter().map(|support| support.target))
        .collect::<FxHashSet<_>>();
    let mut completed = ChildRelation::default();
    for relation in relations.iter_mut().flatten() {
        relation.retire_completed(&mut completed, &obligations, &module.name)?;
    }

    let (connector, seam_nodes) =
        ZXGraph::from_module_body(&fixed_body, &seams).map_err(|source| {
            ModuleCertificationError::Graph {
                module: module.name.clone(),
                source: source.into(),
            }
        })?;
    if connector.total_ids() > limits.max_local_columns {
        return Err(resource_limit(
            &module.name,
            "local ZX columns",
            connector.total_ids(),
            limits.max_local_columns,
        ));
    }
    let parent_ports = module
        .interface
        .quantum_ports
        .iter()
        .enumerate()
        .map(|(index, port)| {
            (
                connector
                    .node_at(port.position)
                    .expect("validated parent port")
                    .id,
                index,
            )
        })
        .collect::<HashMap<_, _>>();
    let seam_indices = seam_nodes
        .iter()
        .enumerate()
        .map(|(index, &node)| (node, index))
        .collect::<HashMap<_, _>>();
    // Single-wire Port wrappers already fit in six columns. Keeping their
    // binds together avoids paying for several tiny projections per definition.
    let connector_components = if connector.action_graph().ordered_nodes().next().is_some()
        || parent_width <= 2
            && fixed_body.block_count() == parent_width
            && binds.len() == parent_width
            && parent_width + 2 * binds.len() <= limits.max_frontier_width
            && fixed_body.blocks().all(|block| block.kind().is_port())
    {
        let nodes = (0..connector.nodes().len()).collect();
        vec![(connector, nodes)]
    } else {
        connector.into_connector_components()
    };
    // Each connector admits only the child relations on its own boundary.
    // Finished components stay separate until the public interface is assembled.
    for (connector, original_nodes) in connector_components {
        let connector_parents = original_nodes
            .iter()
            .filter_map(|node| parent_ports.get(node).copied())
            .collect::<Vec<_>>();
        let connector_binds = original_nodes
            .iter()
            .enumerate()
            .filter_map(|(local, original)| {
                seam_indices
                    .get(original)
                    .map(|&index| (&binds[index], local))
            })
            .collect::<Vec<_>>();
        let mut touched = connector_binds
            .iter()
            .map(|(bind, _)| owners[&bind.endpoint])
            .collect::<Vec<_>>();
        touched.sort_unstable();
        touched.dedup();
        let slot = touched.first().copied().unwrap_or_else(|| {
            relations.push(None);
            relations.len() - 1
        });
        let children_to_join = touched
            .iter()
            .map(|&owner| {
                relations[owner]
                    .take()
                    .expect("live endpoint has a relation")
            })
            .collect::<Vec<_>>();
        let mut joined = compose_parent_connector(
            module,
            children,
            children_to_join,
            connector,
            connector_parents,
            &connector_binds,
            &parent_protected,
            &parent_frontier,
            limits,
        )?;
        joined.retire_completed(&mut completed, &obligations, &module.name)?;
        for (bind, _) in &connector_binds {
            owners.remove(&bind.endpoint);
        }
        for endpoint in &joined.endpoints {
            owners.insert(endpoint.clone(), slot);
        }
        relations[slot] = Some(joined);
    }
    let mut combined = ChildRelation::default();
    for relation in relations.into_iter().flatten() {
        assert!(
            relation.endpoints.is_empty(),
            "all child endpoints are bound"
        );
        let mapping = relation.parent_ports.clone();
        combined.append(relation, &mapping, parent_width);
    }
    combined.append(completed, &[], parent_width);
    let ChildRelation {
        mut rows,
        mut logical_rows,
        mut certificate_rows,
        mut adjustments,
        mut measurements,
        mut discharged_measurements,
        mut protected,
        mut frontier,
        protected_groups,
        ..
    } = combined;
    merge_protected_supports(&mut protected, &parent_protected);
    frontier.extend(parent_frontier);
    frontier.sort_unstable_by_key(support_target_key);
    frontier.dedup();
    let retained = (0..parent_width).collect::<Vec<_>>();
    rows = compose_supported_rows(rows, &mut [], &[], &[], &[], &retained, &module.name)?;
    logical_rows = compose_supported_rows(
        logical_rows,
        &mut [],
        &[],
        &[],
        &[],
        &retained,
        &module.name,
    )?;
    certificate_rows = compose_certificate_rows(certificate_rows, &[], &retained);
    adjustments = compose_supported_rows(
        adjustments,
        &mut measurements,
        &protected,
        &frontier,
        &[],
        &retained,
        &module.name,
    )?;
    normalize_fixing_rows(
        &mut measurements,
        &adjustments,
        &protected,
        limits,
        &module.name,
    )?;
    normalize_measurement_rows(
        &mut measurements,
        &adjustments,
        &protected,
        &protected_groups,
        limits,
        &module.name,
    )?;
    let (newly_discharged, remaining) = measurements
        .into_iter()
        .partition(measurement_can_discharge);
    measurements = remaining;
    discharged_measurements.extend(newly_discharged);
    Ok(ModuleSummary {
        name: module.name.clone(),
        limits,
        dependencies: children
            .values()
            .map(|(_, summary)| (summary.name.clone(), Arc::clone(summary)))
            .collect(),
        ports: module.interface.quantum_ports.clone(),
        signatures,
        rows,
        logical_rows,
        certificate_rows,
        adjustments,
        measurements,
        discharged_measurements,
        protected,
        frontier,
        protected_groups,
        branch_variants: Vec::new(),
        projection: None,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "connector composition consumes independent certified inputs"
)]
fn compose_parent_connector(
    module: &BlockGraph,
    children: &HashMap<&str, (&ModuleInstance, Arc<ModuleSummary>)>,
    relations: Vec<ChildRelation>,
    connector: ZXGraph,
    connector_parents: Vec<usize>,
    binds: &[(&BindSeam, usize)],
    parent_protected: &[ProtectedSupport],
    parent_frontier: &[SupportTarget],
    limits: ModuleCertificationLimits,
) -> Result<ChildRelation, ModuleCertificationError> {
    let existing_parents = relations
        .iter()
        .map(|r| r.parent_ports.len())
        .sum::<usize>();
    let parent_width = existing_parents + connector_parents.len();
    let child_width = relations.iter().map(|r| r.endpoints.len()).sum::<usize>();
    let connector_start = parent_width + child_width;
    let width = connector_start + binds.len();
    ensure_composition_width(&module.name, width, limits.max_frontier_width)?;

    let mut joined = ChildRelation::default();
    let mut next_parent = 0;
    let mut next_child = parent_width;
    for relation in relations {
        let parents = next_parent..next_parent + relation.parent_ports.len();
        let endpoints = next_child..next_child + relation.endpoints.len();
        next_parent = parents.end;
        next_child = endpoints.end;
        joined.append(
            relation,
            &parents.chain(endpoints).collect::<Vec<_>>(),
            width,
        );
    }
    joined
        .parent_ports
        .extend(connector_parents.iter().copied());
    let endpoint_columns = joined
        .endpoints
        .iter()
        .enumerate()
        .map(|(index, endpoint)| (endpoint, parent_width + index))
        .collect::<HashMap<_, _>>();
    let connector_columns = connector_parents
        .iter()
        .map(|&index| {
            connector
                .node_at(module.interface.quantum_ports[index].position)
                .expect("component contains its parent port")
                .id
        })
        .chain(binds.iter().map(|&(_, node)| node))
        .collect::<Vec<_>>();
    let table = connector
        .projected_external_table(
            &connector_columns,
            ProjectionLimits {
                max_frontier_width: limits.max_frontier_width,
                max_witness_nodes: limits.max_witness_nodes,
            },
        )
        .map_err(|error| projection_limit(&module.name, error))?;
    let seam_positions = binds
        .iter()
        .map(|&(_, node)| connector.nodes()[node].pos)
        .collect::<FxHashSet<_>>();
    let connector_rows = table
        .boundary_rows
        .iter()
        .map(|row| {
            let full = table.materialize(row);
            // The boundary row already carries the authoritative certificate phase.
            let stabilizer = connector.materialize_stabilizer_with_sign(full.paulis, false);
            let mut support = PositionedSupport::from_stabilizer(&stabilizer, &connector);
            support.strip_ports(&seam_positions, &[]);
            let mut witness = row.flow_witness.clone();
            witness.strip_positions(&seam_positions);
            SupportedRow {
                signed: row.signed.clone(),
                support,
                witness,
            }
        })
        .collect::<Vec<_>>();
    let connector_certificates =
        retained_certificate_rows(&connector, &table, &connector_rows, connector_columns.len());
    let mapping = (existing_parents..parent_width)
        .chain(connector_start..width)
        .collect::<Vec<_>>();
    // Parent actions own readouts just like leaf actions. Preserve their signed
    // witnesses while the connector's virtual Ports are composed with children.
    if connector.action_graph().ordered_nodes().next().is_some() {
        let stabilizers = connector
            .stabilizers_with_limits(limits)
            .map_err(|source| ModuleCertificationError::Runtime {
                module: module.name.clone(),
                source: source.into(),
            })?;
        let mut measurements = summarize_readouts(
            module.local_body(),
            &connector,
            &stabilizers,
            &table,
            &connector_columns,
        );
        joined.protected_groups.extend(semantic_protected_groups(
            module.local_body(),
            &measurements,
            &connector,
            limits,
            &module.name,
        )?);
        for measurement in &mut measurements {
            measurement.signed = embed_signed_row(&measurement.signed, &mapping, width);
            measurement.support.strip_ports(&seam_positions, &[]);
            measurement.witness.strip_positions(&seam_positions);
        }
        joined.measurements.extend(measurements);
    }
    joined
        .rows
        .extend(embed_rows(&connector_rows, &mapping, width));
    joined
        .logical_rows
        .extend(embed_rows(&connector_rows, &mapping, width));
    joined
        .certificate_rows
        .extend(embed_rows(&connector_certificates, &mapping, width));
    let mut adjustments = embed_rows(&connector_rows, &mapping, width);
    adjustments.append(&mut joined.adjustments);
    joined.adjustments = adjustments;

    let seam_pairs = binds
        .iter()
        .enumerate()
        .map(|(index, &(bind, _))| {
            let port = summarized_port(children, &bind.endpoint);
            (
                endpoint_columns[&bind.endpoint],
                connector_start + index,
                bind.hadamard,
                bind.transpose,
                SupportSeam {
                    edge: (
                        bind.block,
                        exposed_position(port.instance, port.port, port.signature)
                            .expect("validated bind endpoint fits"),
                    ),
                    boundary_to_edge_hadamard: bind.hadamard,
                },
            )
        })
        .collect::<Vec<_>>();
    let bound = binds
        .iter()
        .map(|(bind, _)| &bind.endpoint)
        .collect::<HashSet<_>>();
    let retained = (0..parent_width)
        .chain(
            joined
                .endpoints
                .iter()
                .enumerate()
                .filter_map(|(index, endpoint)| {
                    (!bound.contains(endpoint)).then_some(parent_width + index)
                }),
        )
        .collect::<Vec<_>>();
    let mut protected = joined.protected.clone();
    merge_protected_supports(&mut protected, parent_protected);
    let mut frontier = joined.frontier.clone();
    frontier.extend_from_slice(parent_frontier);
    for rows in [&mut joined.rows, &mut joined.logical_rows] {
        *rows = compose_supported_rows(
            std::mem::take(rows),
            &mut [],
            &[],
            &[],
            &seam_pairs,
            &retained,
            &module.name,
        )?;
    }
    joined.certificate_rows =
        compose_certificate_rows(joined.certificate_rows, &seam_pairs, &retained);
    joined.adjustments = compose_supported_rows(
        joined.adjustments,
        &mut joined.measurements,
        &protected,
        &frontier,
        &seam_pairs,
        &retained,
        &module.name,
    )?;
    resize_discharged_boundaries(
        &mut joined.discharged_measurements,
        retained.len(),
        &module.name,
    )?;
    joined
        .endpoints
        .retain(|endpoint| !bound.contains(endpoint));
    Ok(joined)
}

fn merge_protected_supports(protected: &mut Vec<ProtectedSupport>, added: &[ProtectedSupport]) {
    for constraint in added {
        if let Some(existing) = protected
            .iter_mut()
            .find(|existing| existing.target == constraint.target)
        {
            if existing.forbidden.is_none() {
                existing.forbidden = constraint.forbidden;
            }
        } else {
            protected.push(*constraint);
        }
    }
    protected.sort_unstable_by_key(|constraint| support_target_key(&constraint.target));
}

fn resize_discharged_boundaries(
    measurements: &mut [SummaryMeasurement],
    width: usize,
    module: &str,
) -> Result<(), ModuleCertificationError> {
    for measurement in measurements {
        if !measurement.signed.paulis.is_identity() {
            return Err(invalid(
                module,
                &format!(
                    "discharged measurement '{}' retains boundary support",
                    measurement.name
                ),
            ));
        }
        measurement.signed.paulis = PauliString::new(width);
    }
    Ok(())
}

fn normalize_fixing_rows(
    rows: &mut [SummaryMeasurement],
    adjustments: &[SupportedRow],
    protected: &[ProtectedSupport],
    limits: ModuleCertificationLimits,
    module: &str,
) -> Result<(), ModuleCertificationError> {
    let mut visited = 0;
    for row in rows.iter_mut().filter(|row| row.kind.is_selective_fixing()) {
        let own = row
            .kind
            .selective_fixing_targets()
            .iter()
            .map(|target| (SupportTarget::Node(target.pos), target.forbidden))
            .collect::<HashMap<_, _>>();
        let constraints = protected
            .iter()
            .flat_map(|constraint| {
                if own.contains_key(&constraint.target) {
                    vec![
                        (constraint.target, Pauli::X, false),
                        (constraint.target, Pauli::Z, false),
                    ]
                } else {
                    let support = row.support.get(constraint.target);
                    vec![
                        (constraint.target, Pauli::X, support & Pauli::X),
                        (constraint.target, Pauli::Z, support & Pauli::Z),
                    ]
                }
            })
            .collect::<Vec<_>>();
        let combination = MeasurementNormalization {
            row,
            adjustments,
            visited: &mut visited,
            limits,
            module,
        }
        .solve(&constraints)?
        .ok_or_else(|| {
            invalid(
                module,
                &format!(
                    "selective fixing '{}' cannot be decoupled after module composition",
                    row.name
                ),
            )
        })?;
        for index in combination {
            multiply_measurement(row, &adjustments[index]);
        }
        for (&target, &forbidden) in &own {
            if row.support.get(target) != forbidden {
                return Err(invalid(
                    module,
                    &format!("selective fixing '{}' lost its target", row.name),
                ));
            }
        }
        for constraint in protected {
            if !own.contains_key(&constraint.target)
                && row.support.get(constraint.target) != Pauli::I
            {
                return Err(invalid(
                    module,
                    &format!(
                        "selective fixing '{}' remains coupled after module composition",
                        row.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

enum NormalizationFactor<'a> {
    Group(&'a ProtectedGroup),
    Site(&'a ProtectedSupport),
}

enum NormalizationPoint {
    Factor(usize),
    Partial {
        factor: usize,
        support: usize,
        start: usize,
    },
}

enum NormalizationChoices<'a> {
    Site {
        target: SupportTarget,
        values: std::vec::IntoIter<Pauli>,
    },
    Group {
        group: &'a ProtectedGroup,
        current: Vec<Pauli>,
        permitted: bool,
        next: usize,
    },
    Partial {
        group: &'a ProtectedGroup,
        support: usize,
        start: usize,
        next: usize,
    },
}

struct NormalizationFrame<'a> {
    factor: usize,
    constraints: usize,
    partial: usize,
    choices: NormalizationChoices<'a>,
}

struct MeasurementNormalization<'a> {
    row: &'a SummaryMeasurement,
    adjustments: &'a [SupportedRow],
    visited: &'a mut usize,
    limits: ModuleCertificationLimits,
    module: &'a str,
}

impl MeasurementNormalization<'_> {
    fn solve(
        &mut self,
        constraints: &[(SupportTarget, Pauli, bool)],
    ) -> Result<Option<Vec<usize>>, ModuleCertificationError> {
        let limit = self.limits.max_normalization_states;
        if *self.visited == limit {
            return Err(resource_limit(
                self.module,
                "measurement normalization states",
                self.visited.saturating_add(1),
                limit,
            ));
        }
        *self.visited += 1;
        let rows = self.adjustments.len();
        let columns = constraints.len();
        let bytes = rows.checked_mul(columns).and_then(|projection| {
            columns
                .checked_add(rows)?
                .checked_mul(columns.min(rows))?
                .checked_add(projection)?
                .checked_add(columns)?
                .checked_add(rows)
        });
        let words = bytes.map(|bytes| bytes.div_ceil(8));
        if words.is_none_or(|words| words > self.limits.max_matrix_words) {
            return Err(resource_limit(
                self.module,
                "dense matrix words",
                words.unwrap_or(usize::MAX),
                self.limits.max_matrix_words,
            ));
        }
        let projected = self
            .adjustments
            .iter()
            .map(|candidate| {
                constraints
                    .iter()
                    .map(|&(target, axis, _)| {
                        pauli_component_parity(candidate.support.get(target), axis)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let target = constraints
            .iter()
            .map(|&(_, _, value)| value)
            .collect::<Vec<_>>();
        Ok(solve_binary_combination(&projected, &target))
    }

    fn constrain(
        &self,
        constraints: &mut Vec<(SupportTarget, Pauli, bool)>,
        target: SupportTarget,
        desired: Pauli,
    ) {
        constraints.extend([Pauli::X, Pauli::Z].map(|axis| {
            (
                target,
                axis,
                (self.row.support.get(target) ^ desired) & axis,
            )
        }));
    }

    fn search(
        &mut self,
        factors: &[NormalizationFactor<'_>],
        constraints: &mut Vec<(SupportTarget, Pauli, bool)>,
    ) -> Result<Option<Vec<usize>>, ModuleCertificationError> {
        let mut point = NormalizationPoint::Factor(0);
        let mut frames = Vec::<NormalizationFrame<'_>>::new();
        let mut partial = Vec::new();
        loop {
            let combination = self.solve(constraints)?;
            if combination.is_some() {
                let next = match point {
                    NormalizationPoint::Factor(index) => {
                        let Some(factor) = factors.get(index) else {
                            return Ok(combination);
                        };
                        let choices = match factor {
                            NormalizationFactor::Site(constraint) => {
                                let current = self.row.support.get(constraint.target);
                                let mut values = match constraint.forbidden {
                                    Some(forbidden) => [Pauli::I, Pauli::X, Pauli::Z, Pauli::Y]
                                        .into_iter()
                                        .filter(|&pauli| pauli != forbidden)
                                        .collect::<Vec<_>>(),
                                    None => vec![current],
                                };
                                values.sort_by_key(|&pauli| pauli != current);
                                NormalizationChoices::Site {
                                    target: constraint.target,
                                    values: values.into_iter(),
                                }
                            }
                            NormalizationFactor::Group(group) => {
                                let current = group
                                    .targets
                                    .iter()
                                    .map(|&target| self.row.support.get(target))
                                    .collect::<Vec<_>>();
                                let permitted = current.iter().all(|&pauli| pauli == Pauli::I)
                                    || group
                                        .permitted_support
                                        .iter()
                                        .any(|support| partial_support_matches(&current, support));
                                NormalizationChoices::Group {
                                    group,
                                    current,
                                    permitted,
                                    next: 0,
                                }
                            }
                        };
                        Some((index, choices))
                    }
                    NormalizationPoint::Partial {
                        factor,
                        support,
                        start,
                    } => {
                        let NormalizationFactor::Group(group) = factors[factor] else {
                            unreachable!("partial choices belong to a group")
                        };
                        let chosen = &partial[start..];
                        let template = &group.permitted_support[support];
                        let repeated = group.permitted_support[..support].iter().any(|earlier| {
                            partial_support_matches(chosen, earlier)
                                && template[chosen.len()..] == earlier[chosen.len()..]
                        });
                        if repeated {
                            None
                        } else if chosen.len() == template.len() {
                            if chosen.iter().any(|&pauli| pauli != Pauli::I)
                                && chosen
                                    .iter()
                                    .zip(&group.targets)
                                    .any(|(&pauli, &target)| pauli != self.row.support.get(target))
                            {
                                point = NormalizationPoint::Factor(factor + 1);
                                continue;
                            }
                            None
                        } else {
                            Some((
                                factor,
                                NormalizationChoices::Partial {
                                    group,
                                    support,
                                    start,
                                    next: 0,
                                },
                            ))
                        }
                    }
                };
                if let Some((factor, choices)) = next {
                    frames.push(NormalizationFrame {
                        factor,
                        constraints: constraints.len(),
                        partial: partial.len(),
                        choices,
                    });
                }
            }
            // Resume the deepest unfinished choice. Both factors and partial
            // supports share heap frames, so source width never grows the call stack.
            loop {
                let Some(frame) = frames.last_mut() else {
                    return Ok(None);
                };
                constraints.truncate(frame.constraints);
                partial.truncate(frame.partial);
                let next = match &mut frame.choices {
                    NormalizationChoices::Site { target, values } => values.next().map(|desired| {
                        self.constrain(constraints, *target, desired);
                        NormalizationPoint::Factor(frame.factor + 1)
                    }),
                    NormalizationChoices::Group {
                        group,
                        current,
                        permitted,
                        next,
                    } => {
                        let choice = *next;
                        *next += 1;
                        match choice {
                            0 => {
                                if !*permitted {
                                    continue;
                                }
                                for (&target, &desired) in group.targets.iter().zip(current.iter())
                                {
                                    self.constrain(constraints, target, desired);
                                }
                                Some(NormalizationPoint::Factor(frame.factor + 1))
                            }
                            1 => {
                                if current.iter().all(|&pauli| pauli == Pauli::I) {
                                    continue;
                                }
                                for &target in &group.targets {
                                    self.constrain(constraints, target, Pauli::I);
                                }
                                Some(NormalizationPoint::Factor(frame.factor + 1))
                            }
                            _ => group.permitted_support.get(choice - 2).map(|support| {
                                // A permitted cap is I or its basis: one affine
                                // equation prunes the entire remaining subtree.
                                constraints.extend(group.targets.iter().zip(support).map(
                                    |(&target, &basis)| {
                                        let axis = basis.flip();
                                        (
                                            target,
                                            axis,
                                            pauli_component_parity(
                                                self.row.support.get(target),
                                                axis,
                                            ),
                                        )
                                    },
                                ));
                                NormalizationPoint::Partial {
                                    factor: frame.factor,
                                    support: choice - 2,
                                    start: partial.len(),
                                }
                            }),
                        }
                    }
                    NormalizationChoices::Partial {
                        group,
                        support,
                        start,
                        next,
                    } => {
                        let column = partial.len() - *start;
                        let basis = group.permitted_support[*support][column];
                        let desired = match *next {
                            0 => Some(Pauli::I),
                            1 if basis != Pauli::I => Some(basis),
                            _ => None,
                        };
                        *next += 1;
                        desired.map(|desired| {
                            partial.push(desired);
                            self.constrain(constraints, group.targets[column], desired);
                            NormalizationPoint::Partial {
                                factor: frame.factor,
                                support: *support,
                                start: *start,
                            }
                        })
                    }
                };
                if let Some(next) = next {
                    point = next;
                    break;
                }
                frames.pop();
            }
        }
    }
}

fn pauli_component_parity(pauli: Pauli, axes: Pauli) -> bool {
    ((pauli & Pauli::X) && (axes & Pauli::X)) ^ ((pauli & Pauli::Z) && (axes & Pauli::Z))
}

fn partial_support_matches(partial: &[Pauli], support: &[Pauli]) -> bool {
    partial
        .iter()
        .zip(support)
        .all(|(&pauli, &basis)| pauli == Pauli::I || pauli == basis)
}

fn normalize_measurement_rows(
    rows: &mut [SummaryMeasurement],
    adjustments: &[SupportedRow],
    protected: &[ProtectedSupport],
    protected_groups: &[ProtectedGroup],
    limits: ModuleCertificationLimits,
    module: &str,
) -> Result<(), ModuleCertificationError> {
    let mut visited = 0;
    for index in 0..rows.len() {
        if !matches!(rows[index].kind, StabilizerRowKind::Measurement { .. })
            || measurement_support_is_permitted(&rows[index], protected, protected_groups)
        {
            continue;
        }

        let row = &rows[index];
        let grouped = protected_groups
            .iter()
            .flat_map(|group| group.targets.iter().copied())
            .collect::<FxHashSet<_>>();
        let choice_factors = protected_groups
            .iter()
            .map(NormalizationFactor::Group)
            .chain(
                protected
                    .iter()
                    .filter(|constraint| !grouped.contains(&constraint.target))
                    .map(NormalizationFactor::Site),
            )
            .collect::<Vec<_>>();

        let mut invariants = vec![(row.target, Pauli::X), (row.target, Pauli::Z)];
        invariants.extend(
            row.self_readers
                .iter()
                .copied()
                .flat_map(|target| [(target, Pauli::X), (target, Pauli::Z)]),
        );
        invariants.extend(
            rows.iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .flat_map(|(_, other)| {
                    other
                        .observable
                        .iter_xz()
                        .map(move |axis| (other.target, axis))
                }),
        );

        let mut constraints = invariants
            .into_iter()
            .map(|(target, axis)| (target, axis, false))
            .collect();
        let combination = MeasurementNormalization {
            row,
            adjustments,
            visited: &mut visited,
            limits,
            module,
        }
        .search(&choice_factors, &mut constraints)?
        .ok_or_else(|| {
            invalid(
                module,
                &format!(
                    "measurement '{}' cannot normalize protected support after module composition",
                    row.name
                ),
            )
        })?;
        let row = &mut rows[index];
        for adjustment in combination {
            multiply_measurement(row, &adjustments[adjustment]);
        }
        if !measurement_support_is_permitted(row, protected, protected_groups) {
            return Err(invalid(
                module,
                &format!(
                    "measurement '{}' retains invalid protected support after module composition",
                    row.name
                ),
            ));
        }
    }
    Ok(())
}

fn measurement_support_is_permitted(
    row: &SummaryMeasurement,
    protected: &[ProtectedSupport],
    groups: &[ProtectedGroup],
) -> bool {
    protected
        .iter()
        .all(|constraint| constraint.forbidden != Some(row.support.get(constraint.target)))
        && groups.iter().all(|group| {
            let support = group
                .targets
                .iter()
                .map(|&target| row.support.get(target))
                .collect::<Vec<_>>();
            support.iter().all(|&pauli| pauli == Pauli::I)
                || group
                    .permitted_support
                    .iter()
                    .any(|choice| partial_support_matches(&support, choice))
        })
}

fn solve_binary_combination(rows: &[Vec<bool>], target: &[bool]) -> Option<Vec<usize>> {
    let mut basis = vec![None::<(Vec<bool>, Vec<bool>)>; target.len()];
    for (index, source) in rows.iter().enumerate() {
        let mut row = source.clone();
        let mut coeff = vec![false; rows.len()];
        coeff[index] = true;
        for column in 0..target.len() {
            if !row[column] {
                continue;
            }
            if let Some((pivot, pivot_coeff)) = &basis[column] {
                xor_bits(&mut row, pivot);
                xor_bits(&mut coeff, pivot_coeff);
            } else {
                basis[column] = Some((row, coeff));
                break;
            }
        }
    }

    let mut residual = target.to_vec();
    let mut coeff = vec![false; rows.len()];
    for column in 0..target.len() {
        if !residual[column] {
            continue;
        }
        let (pivot, pivot_coeff) = basis[column].as_ref()?;
        xor_bits(&mut residual, pivot);
        xor_bits(&mut coeff, pivot_coeff);
    }
    residual.iter().all(|&bit| !bit).then(|| {
        coeff
            .iter()
            .enumerate()
            .filter_map(|(i, &set)| set.then_some(i))
            .collect()
    })
}

fn xor_bits(left: &mut [bool], right: &[bool]) {
    for (left, &right) in left.iter_mut().zip(right) {
        *left ^= right;
    }
}

/// Rebases one leaf's rows into the parent: drop the support that sat on the
/// instance's own ports (the seam the parent now owns), then rotate and
/// translate what remains. Mirrors [`instantiate_measurements`] for the four
/// `SupportedRow` lists a summary carries.
fn instantiate_rows(
    rows: &[SupportedRow],
    instance: &ModuleInstance,
    port_positions: &FxHashSet<IVec3>,
    retained_targets: &[SupportTarget],
    module: &str,
) -> Result<Vec<SupportedRow>, ModuleCertificationError> {
    let shift = |source| ModuleCertificationError::Graph {
        module: module.to_string(),
        source,
    };
    let orientation = instance.orientation();
    rows.iter()
        .map(|row| {
            let mut row = row.clone();
            row.support.strip_ports(port_positions, retained_targets);
            row.witness.strip_positions(port_positions);
            row.witness = row
                .witness
                .transformed(orientation, instance.translation)
                .map_err(shift)?;
            row.support = row
                .support
                .transformed(orientation, instance.translation)
                .map_err(shift)?;
            Ok(row)
        })
        .collect()
}

fn instantiate_measurements(
    measurements: &[SummaryMeasurement],
    instance: &ModuleInstance,
    port_positions: &[IVec3],
    retained_targets: &[SupportTarget],
    module: &str,
) -> Result<Vec<SummaryMeasurement>, ModuleCertificationError> {
    let port_position_set = port_positions.iter().copied().collect::<FxHashSet<_>>();
    let orientation = instance.orientation();
    let graph_error = |source| ModuleCertificationError::Graph {
        module: module.to_string(),
        source,
    };
    measurements
        .iter()
        .map(|measurement| {
            let mut measurement = measurement.clone();
            measurement.name = format!("{}__{}", instance.name, measurement.name);
            match &mut measurement.kind {
                StabilizerRowKind::Measurement { name } => {
                    *name = measurement.name.clone();
                }
                StabilizerRowKind::SelectiveFixing { targets } => {
                    for target in targets {
                        target.pos = instance
                            .try_transform_position(target.pos)
                            .map_err(graph_error)?;
                    }
                }
                StabilizerRowKind::Logical => {}
            }
            measurement
                .support
                .strip_ports(&port_position_set, retained_targets);
            measurement.witness.strip_positions(&port_position_set);
            measurement.witness = measurement
                .witness
                .transformed(orientation, instance.translation)
                .map_err(graph_error)?;
            measurement.support = measurement
                .support
                .transformed(orientation, instance.translation)
                .map_err(graph_error)?;
            measurement.target = measurement
                .target
                .transformed(orientation, instance.translation)
                .map_err(graph_error)?;
            measurement.self_readers = measurement
                .self_readers
                .iter()
                .map(|&target| {
                    target
                        .transformed(orientation, instance.translation)
                        .map_err(graph_error)
                })
                .collect::<Result<Vec<_>, ModuleCertificationError>>()?;
            measurement.owned_positions = measurement
                .owned_positions
                .iter()
                .map(|&position| {
                    orientation
                        .try_transform_position(position, instance.translation)
                        .map_err(graph_error)
                })
                .collect::<Result<FxHashSet<_>, ModuleCertificationError>>()?;
            Ok(measurement)
        })
        .collect()
}

fn validate_composite_geometry(
    program: &BlockGraph,
    module: &BlockGraph,
) -> Result<(), ModuleCertificationError> {
    let mut footprint = CompositeFootprint::default();
    let mut next_branch = 0;
    add_module_footprint(
        program,
        module,
        (ModuleOrientation::IDENTITY, IVec3::ZERO),
        "",
        true,
        &mut footprint,
        &mut next_branch,
    )
    .map_err(|source| ModuleCertificationError::Graph {
        module: module.name.clone(),
        source,
    })
}

struct FootprintOrigin {
    definition: String,
    instance_path: String,
    orientation: ModuleOrientation,
}

#[derive(Clone)]
struct FootprintBlock {
    position: IVec3,
    kind: BlockKind,
    branch: Option<usize>,
    origin: Arc<FootprintOrigin>,
    local_position: IVec3,
}

impl FootprintBlock {
    fn materialized_site(&self) -> Arc<crate::MaterializedModuleSite> {
        Arc::new(crate::MaterializedModuleSite {
            definition: self.origin.definition.clone(),
            instance_path: self.origin.instance_path.clone(),
            local_position: self.local_position,
            orientation: self.origin.orientation,
        })
    }
}

#[derive(Default)]
struct CompositeFootprint {
    occupied: HashMap<IVec3, Vec<FootprintBlock>>,
}

impl CompositeFootprint {
    fn add(
        &mut self,
        block: Block,
        branch: Option<usize>,
        origin: &Arc<FootprintOrigin>,
        local_position: IVec3,
    ) -> Result<(), crate::BlockGraphError> {
        let candidate = FootprintBlock {
            position: block.pos(),
            kind: block.kind(),
            branch,
            origin: Arc::clone(origin),
            local_position,
        };
        if let Some(other) = self
            .occupied
            .get(&block.pos())
            .into_iter()
            .flatten()
            .find(|other| {
                other.position == block.pos() && !mutually_exclusive(branch, other.branch)
            })
        {
            return Err(crate::BlockGraphError::ModuleBlockOverlap {
                position: block.pos(),
                first: other.materialized_site(),
                second: candidate.materialized_site(),
            });
        }
        let positions = block.checked_reserved_positions()?;
        for &position in &positions {
            if let Some(other) = self
                .occupied
                .get(&position)
                .into_iter()
                .flatten()
                .find(|other| {
                    !mutually_exclusive(branch, other.branch)
                        && !candidate.kind.allows_reserved_overlap(
                            candidate.position,
                            other.kind,
                            other.position,
                            position,
                        )
                })
            {
                return Err(crate::BlockGraphError::ModuleBlockOverlap {
                    position,
                    first: other.materialized_site(),
                    second: candidate.materialized_site(),
                });
            }
        }
        for position in positions {
            self.occupied
                .entry(position)
                .or_default()
                .push(candidate.clone());
        }
        Ok(())
    }
}

fn mutually_exclusive(left: Option<usize>, right: Option<usize>) -> bool {
    left.is_some() && left == right
}

fn add_module_footprint(
    program: &BlockGraph,
    definition: &BlockGraph,
    transform: (ModuleOrientation, IVec3),
    instance_path: &str,
    include_ports: bool,
    footprint: &mut CompositeFootprint,
    next_branch: &mut usize,
) -> Result<(), crate::BlockGraphError> {
    let definitions = program
        .modules()
        .map(|module| (module.name.as_str(), module))
        .collect::<HashMap<_, _>>();
    let mut pending = vec![(
        definition,
        transform,
        instance_path.to_string(),
        include_ports,
    )];
    while let Some((definition, (orientation, translation), instance_path, include_ports)) =
        pending.pop()
    {
        let origin = Arc::new(FootprintOrigin {
            definition: definition.name.clone(),
            instance_path: instance_path.to_string(),
            orientation,
        });
        definition
            .copy_local_geometry()
            .with_orientation_lenient(orientation)?;
        let regions = definition.local_body().branch_regions()?;
        let shown = regions
            .iter()
            .flat_map(|region| region.shown_arm().blocks().map(Block::pos))
            .collect::<FxHashSet<_>>();
        for block in definition
            .local_body()
            .blocks()
            .filter(|block| !shown.contains(&block.pos()))
            .filter(|block| include_ports || !block.kind().is_port())
        {
            footprint.add(
                crate::graph::oriented_block(block, orientation)?.try_with_shift(translation)?,
                None,
                &origin,
                block.pos(),
            )?;
        }
        for region in regions {
            let branch = *next_branch;
            *next_branch += 1;
            for block in region.on_false().blocks().chain(region.on_true().blocks()) {
                footprint.add(
                    crate::graph::oriented_block(block, orientation)?
                        .try_with_shift(translation)?,
                    Some(branch),
                    &origin,
                    block.pos(),
                )?;
            }
        }
        for instance in definition.instances.iter().rev() {
            let child_translation =
                orientation.try_transform_position(instance.translation, translation)?;
            let child_orientation = orientation.then(instance.rotation);
            let child_path = qualified_name(&instance_path, &instance.name);
            pending.push((
                definitions[instance.definition.as_str()],
                (child_orientation, child_translation),
                child_path,
                false,
            ));
        }
    }
    Ok(())
}

fn bind_seam(
    endpoint: &InstancePort,
    block: IVec3,
    hadamard: bool,
    transpose: bool,
    children: &HashMap<&str, (&ModuleInstance, Arc<ModuleSummary>)>,
) -> BindSeam {
    let mut signature = summarized_port(children, endpoint).signature;
    signature.hadamard ^= hadamard;
    BindSeam {
        endpoint: endpoint.clone(),
        block,
        signature,
        hadamard,
        transpose,
    }
}

fn geometry_bind_seam(
    endpoint: &InstancePort,
    block: IVec3,
    hadamard: bool,
    transpose: bool,
    children: &HashMap<&str, (&ModuleInstance, ModuleGeometry)>,
) -> BindSeam {
    let mut signature = validated_port(children, endpoint).signature;
    signature.hadamard ^= hadamard;
    BindSeam {
        endpoint: endpoint.clone(),
        block,
        signature,
        hadamard,
        transpose,
    }
}

fn geometry_direct_seam(
    output: &InstancePort,
    input: &InstancePort,
    hadamard: bool,
    children: &HashMap<&str, (&ModuleInstance, ModuleGeometry)>,
    module: &str,
) -> Result<bool, ModuleCertificationError> {
    direct_seam_hadamard_parts(
        validated_port(children, output),
        validated_port(children, input),
        hadamard,
        module,
    )
}

fn direct_seam_hadamard(
    output: &InstancePort,
    input: &InstancePort,
    hadamard: bool,
    children: &HashMap<&str, (&ModuleInstance, Arc<ModuleSummary>)>,
    module: &str,
) -> Result<bool, ModuleCertificationError> {
    direct_seam_hadamard_parts(
        summarized_port(children, output),
        summarized_port(children, input),
        hadamard,
        module,
    )
}

fn direct_seam_hadamard_parts(
    output: PortGeometry<'_>,
    input: PortGeometry<'_>,
    hadamard: bool,
    module: &str,
) -> Result<bool, ModuleCertificationError> {
    let output_pos = exposed_position(output.instance, output.port, output.signature)?;
    let input_pos = exposed_position(input.instance, input.port, input.signature)?;
    let direction = Direction::iter()
        .find(|direction| {
            checked_add_position(output_pos, direction.to_ivec3()).ok() == Some(input_pos)
        })
        .ok_or_else(|| invalid(module, "direct pipe endpoints are not adjacent"))?;
    if direction != output.signature.direction.negate() || direction != input.signature.direction {
        return Err(invalid(
            module,
            "direct pipe faces point in different directions",
        ));
    }
    if !compatible_bases(
        output.signature.interior_bases,
        input.signature.interior_bases,
        direction.as_udirection(),
        hadamard,
    ) {
        return Err(invalid(module, "direct pipe face bases are incompatible"));
    }
    Ok(output.signature.hadamard ^ hadamard ^ input.signature.hadamard)
}

fn compose_child_seam(
    (output, input): (&InstancePort, &InstancePort),
    hadamard: bool,
    support_seam: SupportSeam,
    relations: &mut [Option<ChildRelation>],
    owners: &mut HashMap<InstancePort, usize>,
    limit: usize,
    module: &str,
) -> Result<(), ModuleCertificationError> {
    let output_owner = owners[output];
    let input_owner = owners[input];
    let width = relations[output_owner]
        .as_ref()
        .expect("live endpoint has a relation")
        .endpoints
        .len()
        + if input_owner == output_owner {
            0
        } else {
            relations[input_owner]
                .as_ref()
                .expect("live endpoint has a relation")
                .endpoints
                .len()
        };
    ensure_composition_width(module, width, limit)?;

    let mut relation = relations[output_owner]
        .take()
        .expect("live endpoint has a relation");
    if input_owner != output_owner {
        let input_relation = relations[input_owner]
            .take()
            .expect("live endpoint has a relation");
        let output_width = relation.width();
        let mut combined = ChildRelation::default();
        combined.append(relation, &(0..output_width).collect::<Vec<_>>(), width);
        combined.append(
            input_relation,
            &(output_width..width).collect::<Vec<_>>(),
            width,
        );
        relation = combined;
    }

    let left = relation
        .endpoints
        .iter()
        .position(|endpoint| endpoint == output)
        .expect("output endpoint belongs to its relation");
    let right = relation
        .endpoints
        .iter()
        .position(|endpoint| endpoint == input)
        .expect("input endpoint belongs to its relation");
    let retained = (0..width)
        .filter(|&column| column != left && column != right)
        .collect::<Vec<_>>();
    let seam = [(left, right, hadamard, true, support_seam)];
    for rows in [&mut relation.rows, &mut relation.logical_rows] {
        *rows = compose_supported_rows(
            std::mem::take(rows),
            &mut [],
            &[],
            &[],
            &seam,
            &retained,
            module,
        )?;
    }
    relation.certificate_rows =
        compose_certificate_rows(relation.certificate_rows, &seam, &retained);
    relation.adjustments = compose_supported_rows(
        relation.adjustments,
        &mut relation.measurements,
        &relation.protected,
        &relation.frontier,
        &seam,
        &retained,
        module,
    )?;
    for measurement in relation.discharged_measurements.iter_mut() {
        debug_assert!(measurement.signed.paulis.is_identity());
        measurement.signed.paulis = PauliString::new(retained.len());
    }
    relation
        .endpoints
        .retain(|endpoint| endpoint != output && endpoint != input);

    owners.remove(output);
    owners.remove(input);
    for endpoint in &relation.endpoints {
        owners.insert(endpoint.clone(), output_owner);
    }
    relations[output_owner] = Some(relation);
    Ok(())
}

fn ensure_composition_width(
    module: &str,
    width: usize,
    limit: usize,
) -> Result<(), ModuleCertificationError> {
    if width <= limit {
        Ok(())
    } else {
        Err(resource_limit(module, "composition columns", width, limit))
    }
}

fn summarized_port<'a>(
    children: &'a HashMap<&str, (&ModuleInstance, Arc<ModuleSummary>)>,
    endpoint: &InstancePort,
) -> PortGeometry<'a> {
    let (instance, summary) = children
        .get(endpoint.instance.as_str())
        .map(|(instance, summary)| (*instance, summary.as_ref()))
        .expect("program validation resolved the child instance");
    let index = summary
        .port_index(&endpoint.port)
        .expect("program validation resolved the child port");
    PortGeometry {
        instance,
        port: &summary.ports[index],
        signature: summary.signatures[index].with_orientation(instance.orientation()),
    }
}

fn validated_port<'a>(
    children: &'a HashMap<&str, (&ModuleInstance, ModuleGeometry)>,
    endpoint: &InstancePort,
) -> PortGeometry<'a> {
    let (instance, geometry) = children
        .get(endpoint.instance.as_str())
        .map(|(instance, geometry)| (*instance, geometry))
        .expect("program validation resolved the child instance");
    let index = geometry
        .ports
        .iter()
        .position(|port| port.name == endpoint.port)
        .expect("program validation resolved the child port");
    PortGeometry {
        instance,
        port: &geometry.ports[index],
        signature: geometry.signatures[index].with_orientation(instance.orientation()),
    }
}

fn exposed_position(
    instance: &ModuleInstance,
    port: &QuantumPort,
    signature: ModulePortSignature,
) -> Result<IVec3, ModuleCertificationError> {
    instance
        .try_transform_position(port.position)
        .and_then(|position| checked_add_position(position, signature.direction.to_ivec3()))
        .map_err(|error| ModuleCertificationError::Graph {
            module: instance.name.clone(),
            source: error,
        })
}

fn validate_bind_faces(
    module: &BlockGraph,
    binds: &[BindSeam],
) -> Result<(), ModuleCertificationError> {
    let mut used = FxHashSet::default();
    for bind in binds {
        let neighbor = checked_add_position(bind.block, bind.signature.direction.to_ivec3())
            .map_err(|source| ModuleCertificationError::Graph {
                module: module.name.clone(),
                source,
            })?;
        if module.local_body().has_pipe_between(bind.block, neighbor) {
            return Err(invalid(&module.name, "bind reuses an authored pipe face"));
        }
        if !used.insert((bind.block, bind.signature.direction)) {
            return Err(invalid(&module.name, "two binds reuse one block face"));
        }
    }
    Ok(())
}

fn validate_bind_bases(
    module: &BlockGraph,
    body: &BlockGraph,
    binds: &[BindSeam],
) -> Result<(), ModuleCertificationError> {
    for bind in binds {
        let mut pipe = Pipe::new(bind.block, bind.signature.direction);
        if bind.signature.hadamard {
            pipe = pipe.with_hadamard();
        }
        let parent_bases = body.infer_pipe_endpoint_face_bases(&pipe, bind.block);
        if !compatible_bases(
            parent_bases,
            bind.signature.interior_bases,
            bind.signature.direction.as_udirection(),
            bind.signature.hadamard,
        ) {
            return Err(invalid(&module.name, "bind face bases are incompatible"));
        }
    }
    Ok(())
}

fn parent_signatures(
    module: &BlockGraph,
    body: &BlockGraph,
    binds: &[BindSeam],
) -> Result<Vec<ModulePortSignature>, ModuleCertificationError> {
    module
        .interface
        .quantum_ports
        .iter()
        .map(|port| {
            let local = module.local_body().neighbor_positions(port.position);
            let bound = binds
                .iter()
                .filter(|bind| bind.block == port.position)
                .collect::<Vec<_>>();
            match (local.as_slice(), bound.as_slice()) {
                ([_], []) => port_signature(&module.name, body, port.position),
                ([], [bind]) => Ok(bind.signature),
                _ => Err(invalid(
                    &module.name,
                    "each exported Port must have one local or bound pipe",
                )),
            }
        })
        .collect()
}

fn port_signature(
    module: &str,
    body: &BlockGraph,
    position: IVec3,
) -> Result<ModulePortSignature, ModuleCertificationError> {
    let neighbors = body.neighbor_positions(position);
    let [neighbor] = neighbors.as_slice() else {
        return Err(invalid(module, "module Port must have one pipe"));
    };
    let pipe = body
        .get_pipe(position, *neighbor)
        .expect("neighbor position names the incident pipe");
    let direction = if pipe.src() == position {
        pipe.dir()
    } else {
        pipe.dir().negate()
    };
    Ok(ModulePortSignature {
        direction,
        hadamard: pipe.is_hadamard(),
        interior_bases: body.infer_pipe_endpoint_face_bases(pipe, *neighbor),
    })
}

fn compatible_bases(
    left: [Option<Basis>; 3],
    right: [Option<Basis>; 3],
    pipe_axis: UDirection,
    hadamard: bool,
) -> bool {
    (0..3)
        .filter(|&axis| axis != pipe_axis.index())
        .all(|axis| {
            !matches!(
                (left[axis], right[axis]),
                (Some(left), Some(right)) if (left != right) ^ hadamard
            )
        })
}

fn embed_rows(rows: &[SupportedRow], mapping: &[usize], width: usize) -> Vec<SupportedRow> {
    rows.iter()
        .map(|row| SupportedRow {
            signed: embed_signed_row(&row.signed, mapping, width),
            support: row.support.clone(),
            witness: row.witness.clone(),
        })
        .collect()
}

fn embed_signed_row(row: &PhasedPauliString, mapping: &[usize], width: usize) -> PhasedPauliString {
    PhasedPauliString::new(
        PauliString::from_terms(
            width,
            mapping
                .iter()
                .enumerate()
                .map(|(source, &target)| (target, row.paulis.get(source))),
        ),
        row.phase(),
    )
}

struct FrontierRow {
    order: usize,
    row: SupportedRow,
}

fn remap_row_columns(
    row: &mut SupportedRow,
    source_columns: Option<&[usize]>,
    target_columns: &[usize],
) {
    // Pending rows still use global columns; active rows use `source_columns`.
    row.signed.paulis = PauliString::from_terms(
        target_columns.len(),
        row.signed
            .paulis
            .iter_support()
            .filter_map(|(source, pauli)| {
                let source = source_columns.map_or(source, |columns| columns[source]);
                target_columns
                    .binary_search(&source)
                    .ok()
                    .map(|target| (target, pauli))
            }),
    );
}

fn eliminate_frontier_seam(rows: &mut Vec<FrontierRow>, left: usize, right: usize) {
    let mut solved = 0;
    for axis in [Pauli::X, Pauli::Z] {
        let mismatch = |row: &FrontierRow| {
            (row.row.signed.paulis.get(left) ^ row.row.signed.paulis.get(right)) & axis
        };
        let Some(pivot) = (solved..rows.len()).find(|&row| mismatch(&rows[row])) else {
            continue;
        };
        rows.swap(pivot, solved);
        let (before, pivot_and_after) = rows.split_at_mut(solved);
        let (pivot, after) = pivot_and_after
            .split_first_mut()
            .expect("solved pivot is in bounds");
        let last_use = after
            .iter()
            .rposition(mismatch)
            .map(|index| before.len() + index);
        for (index, row) in before.iter_mut().chain(after).enumerate() {
            if mismatch(row) {
                // This pivot is discarded. Its final surviving user can keep
                // the larger support and witness buffers. These payloads commute;
                // signed Pauli multiplication must retain its original order.
                if Some(index) == last_use {
                    if row.row.support.weight() < pivot.row.support.weight() {
                        std::mem::swap(&mut row.row.support, &mut pivot.row.support);
                    }
                    if row.row.witness.source_count() < pivot.row.witness.source_count() {
                        std::mem::swap(&mut row.row.witness, &mut pivot.row.witness);
                    }
                }
                EliminationRow::multiply_assign(&mut row.row, &pivot.row);
            }
        }
        solved += 1;
    }
    rows.drain(..solved);
}

/// Contracts a measurement-free table without admitting future rows or
/// columns. Rows whose last seam closes keep their support/witness here, then
/// rejoin only for the caller's final canonical reduction.
fn compose_frontier_rows(
    rows: Vec<SupportedRow>,
    seams: &[(usize, usize, bool, bool, SupportSeam)],
    retained: &[usize],
    keep_closed: impl Fn(&SupportedRow) -> bool,
) -> Vec<SupportedRow> {
    let Some(width) = rows.first().map(|row| row.signed.paulis.len()) else {
        return rows;
    };
    let mut seam_stage = vec![None; width];
    for (stage, &(left, right, ..)) in seams.iter().enumerate() {
        debug_assert!(seam_stage[left].is_none());
        debug_assert!(seam_stage[right].is_none());
        seam_stage[left] = Some(stage);
        seam_stage[right] = Some(stage);
    }

    let mut pending = (0..seams.len())
        .map(|_| Vec::<FrontierRow>::new())
        .collect::<Vec<_>>();
    let mut finished = Vec::new();
    for (order, row) in rows.into_iter().enumerate() {
        let first_stage = row
            .signed
            .paulis
            .iter_support()
            .filter_map(|(column, _)| seam_stage[column])
            .min();
        let mut row = FrontierRow { order, row };
        if let Some(stage) = first_stage {
            pending[stage].push(row);
        } else {
            remap_row_columns(&mut row.row, None, retained);
            if !row.row.signed.paulis.is_identity() || keep_closed(&row.row) {
                finished.push(row);
            }
        }
    }

    let mut active = Vec::<FrontierRow>::new();
    let mut columns = retained.to_vec();
    for (stage, &(left, right, hadamard, transpose, support_seam)) in seams.iter().enumerate() {
        let mut next_columns = retained.to_vec();
        next_columns.extend(active.iter().flat_map(|row| {
            row.row
                .signed
                .paulis
                .iter_support()
                .map(|(column, _)| columns[column])
        }));
        next_columns.extend(pending[stage].iter().flat_map(|row| {
            row.row
                .signed
                .paulis
                .iter_support()
                .map(|(column, _)| column)
        }));
        next_columns.extend([left, right]);
        next_columns.sort_unstable();
        next_columns.dedup();

        for row in &mut active {
            remap_row_columns(&mut row.row, Some(&columns), &next_columns);
        }
        for row in &mut pending[stage] {
            remap_row_columns(&mut row.row, None, &next_columns);
        }
        active.append(&mut pending[stage]);
        active.sort_unstable_by_key(|row| row.order);
        columns = next_columns;

        let left = columns
            .binary_search(&left)
            .expect("live frontier contains the seam's left column");
        let right = columns
            .binary_search(&right)
            .expect("live frontier contains the seam's right column");
        if hadamard {
            for row in &mut active {
                conjugate_supported_h(&mut row.row, right);
            }
        }
        eliminate_frontier_seam(&mut active, left, right);
        for row in &mut active {
            contract_supported_row(&mut row.row, left, right, transpose, support_seam);
        }

        let mut next_active = Vec::with_capacity(active.len());
        for mut row in active.drain(..) {
            let reaches_future = row.row.signed.paulis.iter_support().any(|(column, _)| {
                seam_stage[columns[column]].is_some_and(|row_stage| row_stage > stage)
            });
            if reaches_future {
                next_active.push(row);
            } else {
                remap_row_columns(&mut row.row, Some(&columns), retained);
                if !row.row.signed.paulis.is_identity() || keep_closed(&row.row) {
                    finished.push(row);
                }
            }
        }
        active = next_active;
    }
    debug_assert!(active.is_empty());
    debug_assert!(pending.iter().all(Vec::is_empty));

    finished.sort_unstable_by_key(|row| row.order);
    finished.into_iter().map(|row| row.row).collect()
}

fn compose_supported_rows(
    mut rows: Vec<SupportedRow>,
    measurements: &mut [SummaryMeasurement],
    protected: &[ProtectedSupport],
    frontier: &[SupportTarget],
    seams: &[(usize, usize, bool, bool, SupportSeam)],
    retained: &[usize],
    module: &str,
) -> Result<Vec<SupportedRow>, ModuleCertificationError> {
    if measurements.is_empty() && seams.len() > 1 {
        rows = compose_frontier_rows(rows, seams, retained, |row| {
            protected
                .iter()
                .any(|constraint| row.support.get(constraint.target) != Pauli::I)
                || frontier
                    .iter()
                    .any(|&target| row.support.get(target) != Pauli::I)
        });
    } else {
        for &(left, right, hadamard, transpose, support_seam) in seams {
            if hadamard {
                for row in &mut rows {
                    conjugate_supported_h(row, right);
                }
                for measurement in &mut *measurements {
                    conjugate_measurement_h(measurement, right);
                }
            }

            if !measurements.is_empty() {
                let mut adjustment_basis = AdjustmentBasis::new(&rows, measurements, protected);
                for index in 0..measurements.len() {
                    let mut candidates = safe_adjustments(
                        &mut adjustment_basis,
                        measurements,
                        index,
                        protected,
                        module,
                    )?;
                    let measurement = &mut measurements[index];
                    eliminate_supported_seam(
                        &mut candidates,
                        std::slice::from_mut(measurement),
                        left,
                        right,
                    );
                    if measurement.signed.paulis.get(left) != measurement.signed.paulis.get(right) {
                        return Err(invalid(
                            module,
                            &format!(
                                "measurement '{}' cannot close across a module seam",
                                measurement.name
                            ),
                        ));
                    }
                }
            }

            let eliminated = eliminate_supported_seam(&mut rows, &mut [], left, right);
            rows.drain(..eliminated);

            for row in &mut rows {
                contract_supported_row(row, left, right, transpose, support_seam);
            }
            rows.retain(|row| {
                !row.signed.paulis.is_identity()
                    || measurements.iter().any(|measurement| {
                        row.support.get(measurement.target) != Pauli::I
                            || measurement
                                .self_readers
                                .iter()
                                .any(|&target| row.support.get(target) != Pauli::I)
                    })
                    || protected
                        .iter()
                        .any(|constraint| row.support.get(constraint.target) != Pauli::I)
                    || frontier
                        .iter()
                        .any(|&target| row.support.get(target) != Pauli::I)
            });
            for measurement in &mut *measurements {
                contract_measurement(measurement, left, right, transpose, support_seam);
            }
        }

        for row in &mut rows {
            remap_row_columns(row, None, retained);
        }
    }
    let mut rank = signed_gaussian_elimination(
        &mut rows,
        (0..retained.len()).flat_map(|column| [Pauli::X, Pauli::Z].map(move |axis| (column, axis))),
        |row, &(column, axis)| row.signed.paulis.get(column) & axis,
    );
    if rank < rows.len() {
        let mut targets = measurements
            .iter()
            .map(|measurement| measurement.target)
            .chain(
                measurements
                    .iter()
                    .flat_map(|measurement| measurement.self_readers.iter().copied()),
            )
            .chain(protected.iter().map(|constraint| constraint.target))
            .chain(frontier.iter().copied())
            .collect::<FxHashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        targets.sort_unstable_by_key(support_target_key);
        rank = eliminate_support_targets(&mut rows, rank, &targets);
    }
    rows.truncate(rank);
    for measurement in measurements {
        measurement.signed.paulis = PauliString::from_terms(
            retained.len(),
            retained
                .iter()
                .enumerate()
                .map(|(target, &source)| (target, measurement.signed.paulis.get(source))),
        );
    }
    Ok(rows)
}

/// Compose a complete certificate basis without dropping boundary-zero rows.
/// These rows are private rank completion, not public observables.
fn compose_certificate_rows(
    mut rows: Vec<SupportedRow>,
    seams: &[(usize, usize, bool, bool, SupportSeam)],
    retained: &[usize],
) -> Vec<SupportedRow> {
    if seams.len() > 1 {
        rows = compose_frontier_rows(rows, seams, retained, |row| row.support.weight() != 0);
    } else {
        for &(left, right, hadamard, transpose, support_seam) in seams {
            if hadamard {
                for row in &mut rows {
                    conjugate_supported_h(row, right);
                }
            }
            let eliminated = eliminate_supported_seam(&mut rows, &mut [], left, right);
            rows.drain(..eliminated);
            for row in &mut rows {
                contract_supported_row(row, left, right, transpose, support_seam);
            }
        }
        for row in &mut rows {
            remap_row_columns(row, None, retained);
        }
        rows.retain(|row| !row.signed.paulis.is_identity() || row.support.weight() != 0);
    }

    let boundary_rank = signed_gaussian_elimination(
        &mut rows,
        (0..retained.len()).flat_map(|column| [Pauli::X, Pauli::Z].map(move |axis| (column, axis))),
        |row, &(column, axis)| row.signed.paulis.get(column) & axis,
    );
    // Live composition needs a basis only on the remaining boundary. Keep all
    // residual witnesses for final rank completion; reducing their physical
    // support at every connector makes earlier prefixes grow through later rows.
    if !seams.is_empty() || boundary_rank == rows.len() {
        return rows;
    }
    let mut targets = rows
        .iter()
        .flat_map(|row| {
            row.support
                .nodes
                .keys()
                .copied()
                .map(SupportTarget::Node)
                .chain(
                    row.support
                        .edges
                        .keys()
                        .copied()
                        .map(|(left, right)| SupportTarget::Edge(left, right)),
                )
        })
        .collect::<FxHashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    targets.sort_unstable_by_key(support_target_key);
    let rank = eliminate_support_targets(&mut rows, boundary_rank, &targets);
    rows.truncate(rank);
    rows
}

/// Continue the same ordered reduction using cached leading support targets.
fn eliminate_support_targets(
    rows: &mut [SupportedRow],
    solved: usize,
    targets: &[SupportTarget],
) -> usize {
    if targets.is_empty() {
        return solved;
    }
    let columns: FxHashMap<_, _> = targets
        .iter()
        .copied()
        .enumerate()
        .map(|(column, target)| (target, column))
        .collect();
    debug_assert_eq!(columns.len(), targets.len(), "support targets are unique");
    leading_pivot_elimination_from(
        rows,
        solved,
        |row| {
            let present = row
                .support
                .nodes
                .keys()
                .copied()
                .map(SupportTarget::Node)
                .chain(row.support.edges.keys().flat_map(|&(left, right)| {
                    [
                        SupportTarget::Edge(left, right),
                        SupportTarget::Edge(right, left),
                    ]
                }));
            present
                .filter_map(|target| {
                    let &column = columns.get(&target)?;
                    let pauli = row.support.get(target);
                    (pauli != Pauli::I)
                        .then_some((column, if pauli & Pauli::X { Pauli::X } else { Pauli::Z }))
                })
                .min_by_key(|&(column, axis)| (column, axis == Pauli::Z))
        },
        |row, &(column, axis)| row.support.get(targets[column]) & axis,
    )
}

/// A seam's row supports stay fixed while each measurement selects its own
/// constraints. Project them once; individual reductions clone only packed rows.
struct AdjustmentBasis<'a> {
    sources: Vec<&'a SupportedRow>,
    candidates: Vec<PhasedPauliString>,
    columns: FxHashMap<SupportTarget, usize>,
    boundary_width: usize,
    selector_start: usize,
    prefix_rank: usize,
    next_measurement: usize,
}

impl<'a> AdjustmentBasis<'a> {
    fn new(
        rows: &'a [SupportedRow],
        measurements: &[SummaryMeasurement],
        protected: &[ProtectedSupport],
    ) -> Self {
        let mut sources = rows.iter().collect::<Vec<_>>();
        sources.sort_by_key(|row| row.support.weight());
        let boundary_width = sources.first().map_or(0, |row| row.signed.paulis.len());
        let mut targets = measurements
            .iter()
            .flat_map(|measurement| {
                std::iter::once(measurement.target).chain(measurement.self_readers.iter().copied())
            })
            .chain(protected.iter().map(|constraint| constraint.target))
            .collect::<Vec<_>>();
        targets.sort_unstable_by_key(support_target_key);
        targets.dedup();
        // Auxiliary support bits and source selectors use X-only columns, so
        // their XORs cannot contribute phases to the signed boundary rows.
        let selector_start = boundary_width + 2 * targets.len();
        let candidates = sources
            .iter()
            .enumerate()
            .map(|(source, row)| {
                let mut paulis = PauliString::from_terms(
                    selector_start + sources.len(),
                    row.signed.paulis.iter_support(),
                );
                for (column, &target) in targets.iter().enumerate() {
                    let value = row.support.get(target);
                    for (offset, axis) in [Pauli::X, Pauli::Z].into_iter().enumerate() {
                        if value & axis {
                            paulis.set(boundary_width + 2 * column + offset, Pauli::X);
                        }
                    }
                }
                paulis.set(selector_start + source, Pauli::X);
                PhasedPauliString::new(paulis, row.signed.phase())
            })
            .collect();
        let columns = targets
            .into_iter()
            .enumerate()
            .map(|(column, target)| (target, boundary_width + 2 * column))
            .collect();
        Self {
            sources,
            candidates,
            columns,
            boundary_width,
            selector_start,
            prefix_rank: 0,
            next_measurement: 0,
        }
    }
}

fn safe_adjustments(
    basis: &mut AdjustmentBasis<'_>,
    measurements: &[SummaryMeasurement],
    index: usize,
    protected: &[ProtectedSupport],
    module: &str,
) -> Result<Vec<SupportedRow>, ModuleCertificationError> {
    debug_assert_eq!(index, basis.next_measurement);
    let measurement = &measurements[index];
    let mut constraints = measurements
        .iter()
        .enumerate()
        .skip(index)
        .flat_map(|(other, candidate)| {
            let axes = if other == index {
                Pauli::Y
            } else {
                candidate.observable
            };
            axes.iter_xz().map(move |axis| (candidate.target, axis))
        })
        .collect::<Vec<_>>();
    constraints.extend(
        measurement
            .self_readers
            .iter()
            .copied()
            .flat_map(|target| [(target, Pauli::X), (target, Pauli::Z)]),
    );
    // A joint fixing names several caps; preserving only its first target
    // lets seam adjustments change the flip axis on the remaining caps.
    constraints.extend(
        measurement
            .kind
            .selective_fixing_targets()
            .iter()
            .flat_map(|target| {
                [Pauli::X, Pauli::Z].map(|axis| (SupportTarget::Node(target.pos), axis))
            }),
    );
    for constraint in protected {
        if measurement
            .kind
            .selective_fixing_targets()
            .iter()
            .any(|target| SupportTarget::Node(target.pos) == constraint.target)
        {
            continue;
        }
        if let Some(forbidden) = constraint.forbidden {
            let difference = measurement.support.get(constraint.target) ^ forbidden;
            let Some(axis) = difference.iter_xz().next() else {
                return Err(invalid(
                    module,
                    &format!(
                        "measurement '{}' has forbidden selective support",
                        measurement.name
                    ),
                ));
            };
            constraints.push((constraint.target, axis));
        } else {
            constraints.extend([(constraint.target, Pauli::X), (constraint.target, Pauli::Z)]);
        }
    }

    let sources = &basis.sources;
    let boundary_width = basis.boundary_width;
    let selector_start = basis.selector_start;
    let mut candidates = basis.candidates.clone();
    let eliminated = signed_gaussian_elimination_from(
        &mut candidates,
        basis.prefix_rank,
        constraints
            .into_iter()
            .map(|(target, axis)| basis.columns[&target] + usize::from(axis == Pauli::Z)),
        |row, &column| row.paulis.get(column) == Pauli::X,
    );
    let mut candidates = candidates
        .into_iter()
        .skip(eliminated)
        .map(|row| {
            let mut support = PositionedSupport::default();
            let mut witness = FlowWitness::default();
            for (column, _) in row
                .paulis
                .iter_support()
                .filter(|&(column, _)| column >= selector_start)
            {
                let source = sources[column - selector_start];
                support.multiply_assign(&source.support);
                witness.xor_assign(&source.witness);
            }
            SupportedRow {
                signed: PhasedPauliString::new(
                    PauliString::from_terms(
                        boundary_width,
                        (0..boundary_width).map(|column| (column, row.paulis.get(column))),
                    ),
                    row.phase(),
                ),
                support,
                witness,
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|row| row.support.weight());
    // The next measurement has the same constraints through this observable.
    // Retain all prefix rows: later pivots must still update earlier rows' phases.
    let column = basis.columns[&measurement.target];
    basis.prefix_rank = signed_gaussian_elimination_from(
        &mut basis.candidates,
        basis.prefix_rank,
        measurement
            .observable
            .iter_xz()
            .map(|axis| column + usize::from(axis == Pauli::Z)),
        |row, &column| row.paulis.get(column) == Pauli::X,
    );
    basis.next_measurement += 1;
    Ok(candidates)
}

fn eliminate_supported_seam(
    rows: &mut [SupportedRow],
    measurements: &mut [SummaryMeasurement],
    left: usize,
    right: usize,
) -> usize {
    let mut solved = 0;
    for axis in [Pauli::X, Pauli::Z] {
        let mismatch = |row: &SupportedRow| {
            (row.signed.paulis.get(left) ^ row.signed.paulis.get(right)) & axis
        };
        let Some(pivot) = (solved..rows.len()).find(|&row| mismatch(&rows[row])) else {
            continue;
        };
        rows.swap(pivot, solved);
        let (before, pivot_and_after) = rows.split_at_mut(solved);
        let (pivot, after) = pivot_and_after
            .split_first_mut()
            .expect("solved pivot is in bounds");
        for row in before.iter_mut().chain(after) {
            if mismatch(row) {
                EliminationRow::multiply_assign(row, pivot);
            }
        }
        for measurement in &mut *measurements {
            if (measurement.signed.paulis.get(left) ^ measurement.signed.paulis.get(right)) & axis {
                multiply_measurement(measurement, pivot);
            }
        }
        solved += 1;
    }
    solved
}

impl EliminationRow for SupportedRow {
    fn multiply_assign(&mut self, pivot: &Self) {
        self.signed.multiply_assign(&pivot.signed);
        self.support.multiply_assign(&pivot.support);
        self.witness.xor_assign(&pivot.witness);
    }
}

fn conjugate_supported_h(row: &mut SupportedRow, column: usize) {
    row.signed.conjugate_h(column);
}

fn multiply_measurement(measurement: &mut SummaryMeasurement, row: &SupportedRow) {
    measurement.signed.multiply_assign(&row.signed);
    measurement.support.multiply_assign(&row.support);
    measurement.witness.xor_assign(&row.witness);
}

fn conjugate_measurement_h(measurement: &mut SummaryMeasurement, column: usize) {
    measurement.signed.conjugate_h(column);
}

fn contract_supported_row(
    row: &mut SupportedRow,
    left: usize,
    right: usize,
    transpose: bool,
    seam: SupportSeam,
) {
    let pauli = row.signed.paulis.get(left);
    if transpose && pauli == Pauli::Y {
        row.signed.shift_phase(2);
    }
    row.signed.paulis.set(left, Pauli::I);
    row.signed.paulis.set(right, Pauli::I);
    let seam_phase = row.support.add_seam(seam, pauli);
    row.signed.shift_phase(seam_phase);
}

fn contract_measurement(
    measurement: &mut SummaryMeasurement,
    left: usize,
    right: usize,
    transpose: bool,
    seam: SupportSeam,
) {
    let pauli = measurement.signed.paulis.get(left);
    if transpose && pauli == Pauli::Y {
        measurement.signed.shift_phase(2);
    }
    measurement.signed.paulis.set(left, Pauli::I);
    measurement.signed.paulis.set(right, Pauli::I);
    let seam_phase = measurement.support.add_seam(seam, pauli);
    measurement.signed.shift_phase(seam_phase);
}

fn invalid(module: &str, message: &str) -> ModuleCertificationError {
    ModuleCertificationError::InvalidConnection {
        module: module.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod certificate_tests {
    use super::*;
    use crate::{Expr, FeedbackTarget, GalleryItem};

    #[test]
    fn discarded_seam_pivot_preserves_signed_order_and_support_cancellation() {
        let edge = (IVec3::ZERO, IVec3::Z);
        let support =
            |nodes: &[(IVec3, Pauli)], edges: &[((IVec3, IVec3), Pauli)]| PositionedSupport {
                nodes: nodes.iter().copied().collect(),
                edges: edges.iter().copied().collect(),
            };
        let row = |order, pauli, support| FrontierRow {
            order,
            row: SupportedRow {
                signed: PhasedPauliString::positive(PauliString::from_terms(
                    3,
                    [(0, Pauli::X), (2, pauli)],
                )),
                support,
                witness: FlowWitness::default(),
            },
        };
        let mut rows = vec![
            row(
                0,
                Pauli::X,
                support(
                    &[(IVec3::ZERO, Pauli::X), (IVec3::X, Pauli::X)],
                    &[(edge, Pauli::Z)],
                ),
            ),
            row(
                1,
                Pauli::Z,
                support(&[(IVec3::ZERO, Pauli::Z)], &[(edge, Pauli::Z)]),
            ),
            row(2, Pauli::Y, support(&[(IVec3::ZERO, Pauli::X)], &[])),
        ];
        eliminate_frontier_seam(&mut rows, 0, 1);
        assert_eq!(rows.len(), 2);
        // Z*X = iY, while Y*X = -iZ. Moving the signed pivot instead of
        // just its support would reverse these phases on the final user.
        for (actual, (order, phase, pauli)) in rows.iter().zip([(1, 1, Pauli::Y), (2, 3, Pauli::Z)])
        {
            assert_eq!(actual.order, order);
            assert_eq!(
                actual.row.signed,
                PhasedPauliString::new(PauliString::from_terms(3, [(2, pauli)]), phase)
            );
        }
        assert_eq!(
            rows[0].row.support.nodes,
            FxHashMap::from_iter([(IVec3::ZERO, Pauli::Y), (IVec3::X, Pauli::X)])
        );
        assert!(rows[0].row.support.edges.is_empty());
        assert_eq!(
            rows[1].row.support.nodes,
            FxHashMap::from_iter([(IVec3::X, Pauli::X)])
        );
        assert_eq!(
            rows[1].row.support.edges,
            FxHashMap::from_iter([(edge, Pauli::Z)])
        );
    }

    fn test_measurement(
        boundary: PauliString,
        support: PositionedSupport,
        target: SupportTarget,
    ) -> SummaryMeasurement {
        SummaryMeasurement {
            name: "m".into(),
            kind: StabilizerRowKind::Measurement { name: "m".into() },
            signed: PhasedPauliString::positive(boundary),
            support,
            witness: FlowWitness::default(),
            target,
            observable: Pauli::Z,
            self_readers: Vec::new(),
            owned_positions: FxHashSet::default(),
        }
    }

    fn test_supported_rows(seed: u32) -> Vec<SupportedRow> {
        (0..9u32)
            .map(|index| {
                let bits = seed
                    .wrapping_mul(0x9e37_79b9)
                    .wrapping_add(index.wrapping_mul(0x85eb_ca6b));
                let pauli = |shift| {
                    [Pauli::I, Pauli::X, Pauli::Z, Pauli::Y][((bits >> shift) & 3) as usize]
                };
                SupportedRow {
                    signed: PhasedPauliString::new(
                        PauliString::from_terms(
                            3,
                            (0..3).map(|column| (column, pauli(2 * column))),
                        ),
                        (bits >> 18) as u8,
                    ),
                    support: PositionedSupport {
                        nodes: [IVec3::ZERO, IVec3::Y, IVec3::Z]
                            .into_iter()
                            .enumerate()
                            .map(|(i, position)| (position, pauli(6 + i * 2)))
                            .filter(|&(_, pauli)| pauli != Pauli::I)
                            .collect(),
                        edges: [((IVec3::X, IVec3::X * 2), pauli(12))]
                            .into_iter()
                            .filter(|&(_, pauli)| pauli != Pauli::I)
                            .collect(),
                    },
                    witness: FlowWitness::default(),
                }
            })
            .collect()
    }

    #[test]
    fn joining_a_port_preserves_the_record_on_its_edge() {
        let edge = (IVec3::ZERO, IVec3::X);
        let target = SupportTarget::Edge(edge.0, edge.1);
        let support = PositionedSupport {
            edges: [(edge, Pauli::Z)].into_iter().collect(),
            ..PositionedSupport::default()
        };
        let mut measurements = [test_measurement(
            PauliString::from_terms(2, [(0, Pauli::Z)]),
            support.clone(),
            target,
        )];
        measurements[0].observable = Pauli::Z;
        let mut rows = vec![
            SupportedRow {
                signed: measurements[0].signed.clone(),
                support,
                witness: FlowWitness::default(),
            },
            SupportedRow {
                signed: PhasedPauliString::positive(PauliString::from_terms(2, [(1, Pauli::Z)])),
                support: PositionedSupport::default(),
                witness: FlowWitness::default(),
            },
        ];
        let ports = [edge.0].into_iter().collect();
        for row in &mut rows {
            row.support.strip_ports(&ports, &[target]);
        }
        measurements[0].support.strip_ports(&ports, &[target]);
        compose_supported_rows(
            rows,
            &mut measurements,
            &[],
            &[],
            &[(
                0,
                1,
                false,
                false,
                SupportSeam {
                    edge,
                    boundary_to_edge_hadamard: false,
                },
            )],
            &[],
            "test",
        )
        .unwrap();
        assert_eq!(measurements[0].support.get(target), Pauli::Z);
    }

    #[test]
    fn seam_adjustments_preserve_every_joint_fixing_target() {
        let positions = [IVec3::ZERO, IVec3::X];
        let protected = positions.map(|pos| ProtectedSupport {
            target: SupportTarget::Node(pos),
            forbidden: Some(Pauli::Y),
        });
        let mut fixing = test_measurement(
            PauliString::new(1),
            PositionedSupport {
                nodes: positions.into_iter().map(|pos| (pos, Pauli::Y)).collect(),
                ..PositionedSupport::default()
            },
            protected[0].target,
        );
        fixing.kind = StabilizerRowKind::SelectiveFixing {
            targets: positions
                .map(|pos| crate::SelectiveFixingTarget {
                    pos,
                    forbidden: Pauli::Y,
                })
                .to_vec(),
        };
        fixing.observable = Pauli::I;
        let adjustments = [SupportedRow {
            signed: PhasedPauliString::positive(PauliString::from_terms(1, [(0, Pauli::X)])),
            support: PositionedSupport {
                nodes: FxHashMap::from_iter([(positions[1], Pauli::Z)]),
                ..PositionedSupport::default()
            },
            witness: FlowWitness::default(),
        }];
        let measurements = [fixing];
        let mut basis = AdjustmentBasis::new(&adjustments, &measurements, &protected);
        let safe = safe_adjustments(&mut basis, &measurements, 0, &protected, "test").unwrap();
        assert!(
            safe.is_empty(),
            "adjustment would change the second cap from Y to X"
        );
    }

    #[test]
    fn cached_support_targets_preserve_signed_rows_and_witnesses() {
        let summary = GalleryItem::GHZ
            .build()
            .summarize_root(ModuleCertificationLimits::DEFAULT)
            .unwrap();
        assert!(
            summary
                .certificate_rows
                .iter()
                .any(|row| row.witness.source_count() != 0)
        );
        let mut targets = [IVec3::ZERO, IVec3::Y, IVec3::Z]
            .into_iter()
            .map(SupportTarget::Node)
            .chain((0..70).map(|x| SupportTarget::Node(IVec3::new(x, 10, 0))))
            .chain([
                SupportTarget::Edge(IVec3::X, IVec3::X * 2),
                SupportTarget::Edge(IVec3::X * 2, IVec3::X),
            ])
            .collect::<Vec<_>>();
        targets.sort_unstable_by_key(support_target_key);
        for seed in 0..64 {
            for reverse_edges in [false, true] {
                let mut rows = test_supported_rows(seed);
                for (row, source) in rows.iter_mut().zip(summary.certificate_rows.iter().cycle()) {
                    row.witness = source.witness.clone();
                    if reverse_edges {
                        row.support.edges = row
                            .support
                            .edges
                            .drain()
                            .map(|((left, right), value)| ((right, left), value))
                            .collect();
                    }
                }
                let solved = signed_gaussian_elimination(
                    &mut rows,
                    (0..3).flat_map(|column| [Pauli::X, Pauli::Z].map(move |axis| (column, axis))),
                    |row, &(column, axis)| row.signed.paulis.get(column) & axis,
                );
                for targets in [&targets[..0], &targets[..3], targets.as_slice()] {
                    let mut expected = rows.clone();
                    let expected_rank = signed_gaussian_elimination_from(
                        &mut expected,
                        solved,
                        targets.iter().copied().flat_map(|target| {
                            [Pauli::X, Pauli::Z].map(move |axis| (target, axis))
                        }),
                        |row, &(target, axis)| row.support.get(target) & axis,
                    );
                    let mut actual = rows.clone();
                    assert_eq!(
                        eliminate_support_targets(&mut actual, solved, targets),
                        expected_rank
                    );
                    for (actual, expected) in actual.iter().zip(&expected) {
                        assert_eq!(actual.signed, expected.signed, "seed {seed}");
                        assert_eq!(actual.support.nodes, expected.support.nodes, "seed {seed}");
                        assert_eq!(actual.support.edges, expected.support.edges, "seed {seed}");
                        let mut difference = actual.witness.clone();
                        difference.xor_assign(&expected.witness);
                        assert_eq!(difference.source_count(), 0, "seed {seed}");
                    }
                }
            }
        }
    }

    #[test]
    fn packed_adjustments_match_eager_signed_elimination() {
        let target = SupportTarget::Node(IVec3::ZERO);
        let edge = SupportTarget::Edge(IVec3::X, IVec3::X * 2);
        let reader = SupportTarget::Node(IVec3::Z);
        let protected = [ProtectedSupport {
            target: SupportTarget::Node(IVec3::Y),
            forbidden: None,
        }];
        let mut measurements = [
            test_measurement(PauliString::new(3), PositionedSupport::default(), target),
            test_measurement(PauliString::new(3), PositionedSupport::default(), edge),
        ];
        measurements[0].self_readers.push(reader);
        measurements[1].observable = Pauli::X;
        for seed in 0..64u32 {
            let rows = test_supported_rows(seed);
            let mut basis = AdjustmentBasis::new(&rows, &measurements, &protected);
            for index in 0..measurements.len() {
                let constraints = measurements
                    .iter()
                    .enumerate()
                    .flat_map(|(other, measurement)| {
                        let axes = if other == index {
                            Pauli::Y
                        } else {
                            measurement.observable
                        };
                        axes.iter_xz().map(move |axis| (measurement.target, axis))
                    })
                    .chain(
                        measurements[index]
                            .self_readers
                            .iter()
                            .copied()
                            .flat_map(|target| [(target, Pauli::X), (target, Pauli::Z)]),
                    )
                    .chain(
                        protected
                            .iter()
                            .flat_map(|p| [(p.target, Pauli::X), (p.target, Pauli::Z)]),
                    );
                let mut expected = rows.clone();
                expected.sort_by_key(|row| row.support.weight());
                let rank = signed_gaussian_elimination(
                    &mut expected,
                    constraints,
                    |row, &(target, axis)| row.support.get(target) & axis,
                );
                expected.drain(..rank);
                expected.sort_by_key(|row| row.support.weight());
                let actual =
                    safe_adjustments(&mut basis, &measurements, index, &protected, "test").unwrap();
                assert_eq!(
                    actual.len(),
                    expected.len(),
                    "seed {seed}, measurement {index}"
                );
                for (actual, expected) in actual.iter().zip(&expected) {
                    assert_eq!(
                        actual.signed, expected.signed,
                        "seed {seed}, measurement {index}"
                    );
                    assert_eq!(
                        actual.support.nodes, expected.support.nodes,
                        "seed {seed}, measurement {index}"
                    );
                    assert_eq!(
                        actual.support.edges, expected.support.edges,
                        "seed {seed}, measurement {index}"
                    );
                }
            }
        }
    }

    #[test]
    fn composed_normalization_preserves_current_group_before_other_templates() {
        let targets = (0..4)
            .map(|x| SupportTarget::Node(IVec3::new(x, 0, 0)))
            .collect::<Vec<_>>();
        let support = |paulis: [Pauli; 4]| PositionedSupport {
            nodes: targets
                .iter()
                .zip(paulis)
                .filter_map(|(&target, pauli)| {
                    let SupportTarget::Node(position) = target else {
                        unreachable!()
                    };
                    (pauli != Pauli::I).then_some((position, pauli))
                })
                .collect(),
            edges: FxHashMap::default(),
        };
        let mut rows = [test_measurement(
            PauliString::new(0),
            support([Pauli::Z, Pauli::I, Pauli::Z, Pauli::Y]),
            SupportTarget::Node(IVec3::new(10, 0, 0)),
        )];
        let adjustments = [
            [Pauli::Z, Pauli::X, Pauli::Z, Pauli::Z],
            [Pauli::I, Pauli::I, Pauli::I, Pauli::X],
        ]
        .map(|paulis| SupportedRow {
            signed: PhasedPauliString::positive(PauliString::new(0)),
            support: support(paulis),
            witness: FlowWitness::default(),
        });
        normalize_measurement_rows(
            &mut rows,
            &adjustments,
            &[ProtectedSupport {
                target: targets[3],
                forbidden: Some(Pauli::Y),
            }],
            &[ProtectedGroup {
                targets: targets[..3].to_vec(),
                permitted_support: vec![vec![Pauli::X; 3], vec![Pauli::Z; 3]],
            }],
            ModuleCertificationLimits::UNLIMITED,
            "order",
        )
        .unwrap();
        assert_eq!(
            targets
                .iter()
                .map(|&target| rows[0].support.get(target))
                .collect::<Vec<_>>(),
            [Pauli::Z, Pauli::I, Pauli::Z, Pauli::Z]
        );
    }

    #[test]
    fn flat_source_limit_counts_cube_height_before_geometry_work() {
        let graph = crate::parse_blog_to_graph("BLOG 1.0\n0: ZXZ [0,0,0] height=4d\n").unwrap();
        let limits = ModuleCertificationLimits {
            max_expanded_blocks: 1,
            max_occupied_cells: 3,
            ..ModuleCertificationLimits::UNLIMITED
        };
        assert!(matches!(
            graph.validate_resource_limits(limits),
            Err(crate::BlockGraphError::Stabilizer(
                crate::StabilizerError::ResourceLimited {
                    phase: "occupied footprint cells",
                    observed: 4,
                    limit: 3,
                }
            ))
        ));
        graph
            .validate_resource_limits(ModuleCertificationLimits {
                max_occupied_cells: 4,
                ..limits
            })
            .unwrap();
    }

    #[test]
    fn wide_correlated_support_normalizes_without_a_powerset() {
        let positions = (0..64).map(|x| IVec3::new(x, 0, 0)).collect::<Vec<_>>();
        for (initial, adjustment, desired, other) in [
            (Pauli::Y, Pauli::Z, Pauli::X, Pauli::Z),
            (Pauli::Z, Pauli::X, Pauli::Y, Pauli::X),
        ] {
            let group = ProtectedGroup {
                targets: positions.iter().copied().map(SupportTarget::Node).collect(),
                permitted_support: vec![vec![desired; 64], vec![other; 64]],
            };
            let support = |pauli| PositionedSupport {
                nodes: positions
                    .iter()
                    .copied()
                    .map(|position| (position, pauli))
                    .collect(),
                edges: FxHashMap::default(),
            };
            let row = test_measurement(
                PauliString::new(0),
                support(initial),
                SupportTarget::Node(IVec3::new(100, 0, 0)),
            );
            let adjustments = [SupportedRow {
                signed: PhasedPauliString::positive(PauliString::new(0)),
                support: support(adjustment),
                witness: FlowWitness::default(),
            }];
            for (limit, expected) in [(0, 1), (16, 17)] {
                let error = normalize_measurement_rows(
                    &mut [row.clone()],
                    &adjustments,
                    &[],
                    std::slice::from_ref(&group),
                    ModuleCertificationLimits {
                        max_normalization_states: limit,
                        ..ModuleCertificationLimits::UNLIMITED
                    },
                    "wide",
                )
                .unwrap_err();
                assert!(
                    matches!(&error, ModuleCertificationError::ResourceLimited {
                module, phase: "measurement normalization states", observed, limit: actual_limit,
            } if module == "wide" && *observed == expected && *actual_limit == limit),
                    "{error}"
                );
            }
            let mut rows = [row];
            normalize_measurement_rows(
                &mut rows,
                &adjustments,
                &[],
                std::slice::from_ref(&group),
                ModuleCertificationLimits {
                    max_normalization_states: 160,
                    ..ModuleCertificationLimits::UNLIMITED
                },
                "wide",
            )
            .unwrap();
            assert!(
                positions
                    .iter()
                    .all(|&position| rows[0].support.get(SupportTarget::Node(position)) == desired)
            );
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn many_protected_factors_use_heap_frames() {
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let width = 4_096;
                let protected = (0..width)
                    .map(|x| ProtectedSupport {
                        target: SupportTarget::Node(IVec3::new(x as i32, 0, 0)),
                        forbidden: Some(Pauli::X),
                    })
                    .collect::<Vec<_>>();
                let support = PositionedSupport {
                    nodes: (0..width)
                        .map(|x| (IVec3::new(x as i32, 0, 0), Pauli::X))
                        .collect(),
                    edges: FxHashMap::default(),
                };
                let row = test_measurement(
                    PauliString::new(0),
                    support.clone(),
                    SupportTarget::Node(IVec3::new(width as i32, 0, 0)),
                );
                let adjustments = [SupportedRow {
                    signed: PhasedPauliString::positive(PauliString::new(0)),
                    support,
                    witness: FlowWitness::default(),
                }];
                let limits = ModuleCertificationLimits {
                    max_normalization_states: 8,
                    max_matrix_words: 32_768,
                    ..ModuleCertificationLimits::DEFAULT
                };
                assert!(matches!(
                    normalize_measurement_rows(
                        &mut [row.clone()],
                        &adjustments,
                        &protected,
                        &[],
                        limits,
                        "deep"
                    ),
                    Err(ModuleCertificationError::ResourceLimited {
                        phase: "measurement normalization states",
                        observed: 9,
                        limit: 8,
                        ..
                    })
                ));
                let mut rows = [row];
                normalize_measurement_rows(
                    &mut rows,
                    &adjustments,
                    &protected,
                    &[],
                    ModuleCertificationLimits {
                        max_normalization_states: width + 1,
                        ..limits
                    },
                    "deep",
                )
                .unwrap();
                assert!(rows[0].support.nodes.is_empty());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn normalization_limit_counts_visited_assignments() {
        let invalid_position = IVec3::ZERO;
        let targets = (0..13)
            .map(|x| SupportTarget::Node(IVec3::new(x, 0, 0)))
            .collect::<Vec<_>>();
        let protected = targets
            .iter()
            .copied()
            .map(|target| ProtectedSupport {
                target,
                forbidden: Some(Pauli::Z),
            })
            .collect::<Vec<_>>();
        let support = PositionedSupport {
            nodes: FxHashMap::from_iter([(invalid_position, Pauli::Z)]),
            edges: FxHashMap::default(),
        };
        let mut rows = [test_measurement(
            PauliString::new(0),
            support,
            SupportTarget::Node(IVec3::new(20, 0, 0)),
        )];
        let adjustment = |pauli| SupportedRow {
            signed: PhasedPauliString::positive(PauliString::new(0)),
            support: PositionedSupport {
                nodes: FxHashMap::from_iter([(invalid_position, pauli)]),
                edges: FxHashMap::default(),
            },
            witness: FlowWitness::default(),
        };

        normalize_measurement_rows(
            &mut rows,
            &[adjustment(Pauli::X), adjustment(Pauli::Z)],
            &protected,
            &[],
            ModuleCertificationLimits {
                max_normalization_states: 15,
                ..ModuleCertificationLimits::UNLIMITED
            },
            "test",
        )
        .unwrap();

        // The available Z adjustment cancels the original Z support.
        assert!(rows[0].support.nodes.is_empty());
    }

    #[test]
    fn closed_adjustments_stay_live_when_a_pending_obligation_reads_them() {
        let row = |boundary, support| SupportedRow {
            signed: PhasedPauliString::new(PauliString::from_terms(1, [(0, boundary)]), 2),
            support,
            witness: FlowWitness::default(),
        };
        let mut relation = ChildRelation {
            adjustments: vec![
                row(
                    Pauli::I,
                    PositionedSupport {
                        nodes: FxHashMap::from_iter([(IVec3::ZERO, Pauli::Z)]),
                        ..PositionedSupport::default()
                    },
                ),
                row(
                    Pauli::I,
                    PositionedSupport {
                        edges: FxHashMap::from_iter([((IVec3::X, IVec3::X + IVec3::Z), Pauli::Z)]),
                        ..PositionedSupport::default()
                    },
                ),
                row(Pauli::X, PositionedSupport::default()),
            ],
            ..ChildRelation::default()
        };
        // The not-yet-admitted reader names the opposite direction of this edge.
        let obligations =
            FxHashSet::from_iter([SupportTarget::Edge(IVec3::X + IVec3::Z, IVec3::X)]);
        let mut completed = ChildRelation::default();
        relation
            .retire_completed(&mut completed, &obligations, "test")
            .unwrap();
        assert_eq!(relation.adjustments.len(), 2);
        assert_eq!(
            relation.adjustments[0]
                .support
                .get(*obligations.iter().next().unwrap()),
            Pauli::Z
        );
        assert_eq!(relation.adjustments[1].signed.paulis.get(0), Pauli::X);
        assert_eq!(completed.adjustments.len(), 1);
        assert_eq!(completed.adjustments[0].signed.phase(), 2);
        assert_eq!(
            completed.adjustments[0]
                .support
                .get(SupportTarget::Node(IVec3::ZERO)),
            Pauli::Z
        );
    }

    #[test]
    fn discharged_measurement_boundary_is_not_erased() {
        let mut measurements = [test_measurement(
            PauliString::from_terms(1, [(0, Pauli::X)]),
            PositionedSupport::default(),
            SupportTarget::Node(IVec3::ZERO),
        )];

        let error = resize_discharged_boundaries(&mut measurements, 2, "test").unwrap_err();

        assert!(matches!(
            error,
            ModuleCertificationError::InvalidConnection { message, .. }
                if message.contains("discharged measurement 'm' retains boundary support")
        ));
        assert_eq!(measurements[0].signed.paulis.weight(), 1);
    }

    #[test]
    fn multiplying_positioned_support_reconstructs_cross_centers() {
        let zx = ZXGraph::try_from(
            &crate::GalleryItem::CNOT
                .build()
                .materialize_root_graph()
                .expect("gallery flat projection"),
        )
        .unwrap();
        let node = zx
            .nodes()
            .iter()
            .find(|node| {
                matches!(node.kind, crate::NodeKind::X | crate::NodeKind::Z)
                    && zx.edges().iter().filter(|edge| edge.n1 == node.id).count() == 3
            })
            .unwrap();
        let edges = zx
            .edges()
            .iter()
            .filter(|edge| edge.n1 == node.id)
            .map(|edge| edge.id)
            .collect::<Vec<_>>();
        let row = |a: usize, b: usize| {
            let mut row = PauliString::new(zx.total_ids());
            for index in [a, b] {
                let edge = &zx.edges()[edges[index] - zx.nodes().len()];
                let mut pauli = node.kind.cross_pauli();
                if edge.hadamard && node.id == edge.n1.max(edge.n2) {
                    pauli = pauli.flip();
                }
                row.set(edge.id, pauli);
                row.set(zx.edge_id(edge.n2, edge.n1).unwrap(), pauli);
            }
            zx.materialize_stabilizer_with_sign(row, false)
        };
        let left = row(0, 1);
        let right = row(0, 2);
        let mut support = PositionedSupport::from_stabilizer(&left, &zx);
        support.multiply_assign(&PositionedSupport::from_stabilizer(&right, &zx));
        let product = support.materialize(&zx, 0).unwrap();
        assert_eq!(product.paulis.get(node.id), node.kind.cross_pauli());
        assert_eq!(product.paulis.get(edges[0]), Pauli::I);
        assert_ne!(product.paulis.get(edges[1]), Pauli::I);
        assert_ne!(product.paulis.get(edges[2]), Pauli::I);
    }

    fn identity_wrapper(program: &BlockGraph) -> BlockGraph {
        let mut child = program.root().clone();
        child.name = "Child".into();
        let mut body = BlockGraph::new();
        let quantum_connections = child
            .interface
            .quantum_ports
            .iter()
            .map(|port| {
                let role = match port.direction {
                    crate::PortDirection::Input => crate::PortRole::Input,
                    crate::PortDirection::Output => crate::PortRole::Output,
                };
                body.try_add_block(
                    Block::new(port.position, BlockKind::Port)
                        .with_port_role(role)
                        .unwrap(),
                )
                .unwrap();
                let endpoint = InstancePort {
                    instance: "child".into(),
                    port: port.name.clone(),
                };
                match port.direction {
                    crate::PortDirection::Input => QuantumConnection::Input {
                        block: port.position,
                        input: endpoint,
                        hadamard: false,
                    },
                    crate::PortDirection::Output => QuantumConnection::Output {
                        output: endpoint,
                        block: port.position,
                        hadamard: false,
                    },
                }
            })
            .collect();
        let root = BlockGraph::definition(
            "main",
            body,
            child.interface.clone(),
            vec![ModuleInstance {
                name: "child".into(),
                definition: child.name.clone(),
                rotation: crate::ModuleRotation::IDENTITY,
                translation: IVec3::ZERO,
            }],
            quantum_connections,
            Vec::new(),
        );
        BlockGraph::from_definitions(vec![child, root]).unwrap()
    }

    #[test]
    fn shallower_sibling_preserves_queued_dependency_levels() {
        let wire = "in input: data = 0\nout output: data = 2\n
            0: Port [0,0,-1] role=input\n1: XZZ [0,0,0]\n2: Port [0,0,1] role=output\n
            0 -> +Z\n1 -> +Z\n";
        let program = crate::parse_inline_graph(&format!(
            "BLOG 1.0\nmodule Wire {{\n{wire}}}\nmodule Late {{\n{wire}}}\n
            module Nested {{
                in input: data = 0\nout output: data = 2
                0: Port [0,0,-1] role=input\n2: Port [0,0,1] role=output
                wire: Wire @ [0,0,0]
                0 -> wire.input\nwire.output -> 2
            }}
            module main {{
                in input: data = 0\nout output: data = 2
                0: Port [0,0,-1] role=input\n2: Port [0,0,2] role=output
                nested: Nested @ [0,0,0]
                late: Late @ [0,0,1]
                0 -> nested.input\nnested.output -> late.input\nlate.output -> 2
            }}"
        ))
        .unwrap();
        for jobs in [1, 2] {
            let summary = program
                .summarize_root_with_jobs(
                    ModuleCertificationLimits::DEFAULT,
                    NonZeroUsize::new(jobs).unwrap(),
                )
                .unwrap();
            assert_eq!(summary.boundary_rows().len(), 2);
            assert!(summary.definition("Nested").is_some());
            assert!(summary.definition("Late").is_some());
        }
    }

    #[test]
    fn parent_readout_survives_child_composition() {
        let program = crate::parse_inline_graph(
            "BLOG 1.0\n\nmodule Wire {\n\
             in input: data = 0\nout output: data = 2\n\
             0: Port [0,0,-1]\n1: ZXZ [0,0,0]\n2: Port [0,0,1]\n\
             0 -> +Z\n1 -> +Z\n}\n\nmodule main {\n\
             in input: data = 10\nout result = m\n\
             wire: Wire @ [0,0,0]\n\
             10: Port [0,0,-1] role=input\n11: X [0,0,1]\n\
             10 -> wire.input\nwire.output -> 11\nm = measure 11\n}\n",
        )
        .unwrap();
        let summary = program
            .summarize_root(ModuleCertificationLimits::DEFAULT)
            .unwrap();
        assert!(
            summary
                .measurements
                .iter()
                .chain(&summary.discharged_measurements)
                .any(|row| row.name == "m")
        );
        let graph = program
            .materialize_root_graph()
            .unwrap()
            .fix_shadowed_faces();
        let zx = ZXGraph::try_from(&graph).unwrap();
        let composed = summary.materialize_stabilizers(&zx, IVec3::ZERO).unwrap();
        let flat = graph.analyze_actions().unwrap().1;
        assert_eq!(composed.generators.len(), flat.generators.len());
        for (composed, flat) in composed.generators.iter().zip(&flat.generators) {
            assert_eq!(composed.kind, flat.kind);
            assert_eq!(composed.stabilizer.paulis, flat.stabilizer.paulis);
            assert_eq!(composed.stabilizer.sign, flat.stabilizer.sign);
        }
    }

    #[test]
    fn classical_parent_action_keeps_independent_connectors_within_frontier_limit() {
        let program = crate::parse_inline_graph(
            "BLOG 1.0\nmodule Wire {\n\
             in input: data = 0\nout output: data = 2\n\
             0: Port [0,0,-1]\n1: ZXZ [0,0,0]\n2: Port [0,0,1]\n\
             0 -> +Z\n1 -> +Z\n}\nmodule main {\n\
             in a: data = 10\nout a_out: data = 11\n\
             in b: data = 12\nout b_out: data = 13\nin enable\nout done = flag\n\
             a: Wire @ [0,0,0]\nb: Wire @ [2,0,0]\n\
             10: Port [0,0,-1] role=input\n11: Port [0,0,1] role=output\n\
             12: Port [2,0,-1] role=input\n13: Port [2,0,1] role=output\n\
             10 -> a.input\na.output -> 11\n12 -> b.input\nb.output -> 13\n\
             flag = enable | !enable\n}\n",
        )
        .unwrap();
        let summary = program
            .summarize_root(ModuleCertificationLimits {
                max_frontier_width: 6,
                ..ModuleCertificationLimits::DEFAULT
            })
            .unwrap();
        assert_eq!(summary.quantum_ports().len(), 4);
    }

    #[test]
    fn identity_wrapper_preserves_materialized_stabilizers() {
        let program = identity_wrapper(&GalleryItem::CCZInjectedMaj.build());
        let summary = program
            .summarize_root(ModuleCertificationLimits::UNLIMITED)
            .unwrap();
        let graph = program.materialize_root_graph().unwrap();
        let zx = ZXGraph::try_from(&graph).unwrap();
        let composed = summary.materialize_stabilizers(&zx, IVec3::ZERO).unwrap();
        let flat = graph.analyze_actions().unwrap().1;

        assert_eq!(composed.generators.len(), flat.generators.len());
        for (composed, flat) in composed.generators.iter().zip(&flat.generators) {
            assert_eq!(composed.kind, flat.kind);
            assert_eq!(composed.stabilizer.paulis, flat.stabilizer.paulis);
            assert_eq!(composed.stabilizer.sign, flat.stabilizer.sign);
        }
    }

    #[test]
    fn composed_phase_cache_matches_flat_derivation_and_tracks_t_conversion() {
        for item in [
            GalleryItem::CNOT,
            GalleryItem::CZTemporalH,
            GalleryItem::S,
            GalleryItem::T,
            GalleryItem::CCZInjectedMaj,
            GalleryItem::PhaseGradientK4,
        ] {
            let program = item.build();
            let summary = program
                .summarize_root(ModuleCertificationLimits::UNLIMITED)
                .unwrap();
            let graph = program.materialize_root_graph().unwrap();
            let zx = ZXGraph::try_from(&graph).unwrap();
            let composed = summary.materialize_stabilizers(&zx, IVec3::ZERO).unwrap();
            let cached = composed.zx_graph.stabilizer_phase_basis.get().unwrap();
            let mut queries = composed
                .generators
                .iter()
                .map(|row| row.stabilizer.paulis.clone())
                .chain(composed.auxiliary_rows().iter().cloned())
                .collect::<Vec<_>>();
            let products = queries
                .windows(2)
                .map(|rows| {
                    let mut product = rows[0].clone();
                    product ^= &rows[1];
                    product
                })
                .collect::<Vec<_>>();
            queries.extend(products);
            let flat = ZXGraph::try_from(&graph).unwrap();
            let mut raw_queries = queries.clone();
            flat.clear_cross_centers(&mut raw_queries);
            assert_eq!(
                composed.zx_graph.stabilizer_row_phases(&queries),
                flat.external_stabilizer_row_phases(&raw_queries),
                "{item:?}"
            );
            let runtime =
                crate::RuntimeStabilizerBasis::from_generators(&composed).with_t_nodes_as_ports();
            let retained = runtime.zx_graph().stabilizer_phase_basis.get().unwrap();
            assert!(Arc::ptr_eq(cached, retained), "{item:?}");
            let mut fresh_demoted = runtime.zx_graph().clone();
            fresh_demoted.stabilizer_phase_basis.take();
            assert_eq!(
                runtime.zx_graph().stabilizer_row_phases(&queries),
                fresh_demoted.stabilizer_row_phases(&queries),
                "{item:?} after T conversion"
            );
        }
    }

    #[test]
    fn rotated_phase_gradient_summary_preserves_flat_nonlogical_rows() {
        let program = GalleryItem::PhaseGradientK4.build();
        let summary = program
            .summarize_root(ModuleCertificationLimits::DEFAULT)
            .unwrap();
        let graph = program.materialize_root_graph().unwrap();
        let zx = ZXGraph::try_from(&graph).unwrap();
        let composed = summary.materialize_stabilizers(&zx, IVec3::ZERO).unwrap();
        let flat = graph.analyze_actions().unwrap().1;

        assert_eq!(composed.generators.len(), flat.generators.len());
        for (composed, flat) in composed.generators.iter().zip(&flat.generators) {
            assert_eq!(composed.kind, flat.kind);
            // Composition preserves child readout representatives; flat analysis
            // may choose a different logical basis for the same graph.
            if matches!(composed.kind, StabilizerRowKind::Logical) {
                continue;
            }
            assert_eq!(
                composed.stabilizer.paulis, flat.stabilizer.paulis,
                "{:?}",
                composed.kind
            );
            assert_eq!(
                composed.stabilizer.sign, flat.stabilizer.sign,
                "{:?}",
                composed.kind
            );
        }
    }

    #[test]
    fn leaf_certificate_preserves_signed_rows() {
        for item in [
            GalleryItem::CCZInjectedAnd,
            GalleryItem::CCZInjectedMaj,
            GalleryItem::UMA,
        ] {
            let program = item.build();
            let summary = program
                .summarize_root(ModuleCertificationLimits::UNLIMITED)
                .unwrap();
            let graph = program.materialize_root_graph().unwrap();
            let zx = ZXGraph::try_from(&graph).unwrap();
            for row in &summary.certificate_rows {
                let stabilizer = materialize_summary_row(
                    &row.signed,
                    &row.support,
                    &summary.ports,
                    &zx,
                    ModuleOrientation::IDENTITY,
                    IVec3::ZERO,
                    summary.name(),
                )
                .unwrap();
                let mut paulis = stabilizer.paulis.clone();
                zx.clear_cross_centers(std::slice::from_mut(&mut paulis));
                assert_eq!(
                    stabilizer_phase(&stabilizer),
                    zx.external_stabilizer_row_phases(&[paulis])[0],
                    "{item:?} certificate"
                );
            }
            for row in &summary.measurements {
                let stabilizer = materialize_summary_row(
                    &row.signed,
                    &row.support,
                    &summary.ports,
                    &zx,
                    ModuleOrientation::IDENTITY,
                    IVec3::ZERO,
                    summary.name(),
                )
                .unwrap();
                let mut paulis = stabilizer.paulis.clone();
                zx.clear_cross_centers(std::slice::from_mut(&mut paulis));
                assert_eq!(
                    stabilizer_phase(&stabilizer),
                    zx.external_stabilizer_row_phases(&[paulis])[0],
                    "{item:?} {}",
                    row.name
                );
            }
            let composed = summary.materialize_stabilizers(&zx, IVec3::ZERO).unwrap();
            let flat = graph.analyze_actions().unwrap().1;
            assert_eq!(composed.generators.len(), flat.generators.len());
            for (composed, flat) in composed.generators.iter().zip(&flat.generators) {
                assert_eq!(composed.kind, flat.kind);
                assert_eq!(composed.stabilizer.paulis, flat.stabilizer.paulis);
                assert_eq!(composed.stabilizer.sign, flat.stabilizer.sign);
            }
        }
    }

    #[test]
    fn composed_adder_rejects_unavailable_causal_readout() {
        let program = crate::BlockGraph::from_text(bloq_test::ONE_BIT_ADDER_SOURCE).unwrap();
        let summary = program
            .summarize_root(ModuleCertificationLimits::UNLIMITED)
            .unwrap();
        let graph = program
            .materialize_root_graph()
            .unwrap()
            .canonical_true_branch_view()
            .unwrap();
        let offset = IVec3::new(0, 0, -*graph.spans().unwrap().2.start());
        let graph = graph.with_zero_min_z().unwrap().fix_shadowed_faces();
        let zx = ZXGraph::try_from(&graph).unwrap();

        let result = summary.materialize_stabilizers(&zx, offset).map(|_| ());
        assert!(
            matches!(
                result,
                Err(ModuleCertificationError::Runtime {
                    source: crate::RuntimeBasisError::Stabilizer(
                        crate::StabilizerError::UnavailableControlParity { ref mvar, .. }
                    ),
                    ..
                }) if mvar == "uma__m_ikprime"
            ),
            "{result:?}"
        );
    }

    #[test]
    fn composed_adder_certificate_matches_flat_row_space() {
        let program = crate::BlockGraph::from_text(bloq_test::ONE_BIT_ADDER_SOURCE).unwrap();
        let summary = program
            .summarize_root(ModuleCertificationLimits::UNLIMITED)
            .unwrap();
        let graph = program
            .materialize_root_graph()
            .unwrap()
            .canonical_true_branch_view()
            .unwrap();
        let offset = IVec3::new(0, 0, -*graph.spans().unwrap().2.start());
        let graph = graph.with_zero_min_z().unwrap().fix_shadowed_faces();
        let zx = ZXGraph::try_from(&graph).unwrap();

        let composed = summary
            .certificate_rows
            .iter()
            .map(|row| {
                let stabilizer = materialize_summary_row(
                    &row.signed,
                    &row.support,
                    &summary.ports,
                    &zx,
                    ModuleOrientation::IDENTITY,
                    offset,
                    summary.name(),
                )
                .unwrap();
                let mut paulis = stabilizer.paulis;
                zx.clear_cross_centers(std::slice::from_mut(&mut paulis));
                paulis
            })
            .collect::<Vec<_>>();
        let flat = zx.to_external_generator_table();
        let composed_rank = crate::zx::reduce_to_basis(&composed, zx.total_ids()).len();
        let flat_rank = crate::zx::reduce_to_basis(&flat, zx.total_ids()).len();
        let mut combined = composed;
        combined.extend(flat);
        let combined_rank = crate::zx::reduce_to_basis(&combined, zx.total_ids()).len();

        assert_eq!(composed_rank, flat_rank);
        assert_eq!(combined_rank, flat_rank);
    }

    #[test]
    fn semantic_frontier_retains_resource_and_frame_support() {
        let response = IVec3::new(2, 0, 0);
        let resource = IVec3::new(3, 0, 0);
        let mut body = BlockGraph::new();
        body.add_block(Block::new(response, BlockKind::Cube(crate::CubeKind::ZXZ)));
        body.add_block(Block::new(resource, BlockKind::T));
        body.set_actions_with_inputs(
            vec![Action::Feedback {
                targets: vec![FeedbackTarget {
                    pauli: PauliBasis::X,
                    target: response,
                    direction: None,
                }],
                condition: Some(Expr::Var("frame".into())),
            }],
            ["frame".to_string()],
        )
        .unwrap();
        let module = BlockGraph::definition(
            "test",
            body.clone(),
            Default::default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let frontier = semantic_frontier_supports(&module, &body).unwrap();
        assert!(frontier.contains(&SupportTarget::Node(response)));
        assert!(frontier.contains(&SupportTarget::Node(resource)));

        let rows = compose_supported_rows(
            vec![SupportedRow {
                signed: PhasedPauliString::positive(PauliString::try_from("XX").unwrap()),
                support: PositionedSupport {
                    nodes: FxHashMap::from_iter([(response, Pauli::X)]),
                    edges: FxHashMap::default(),
                },
                witness: FlowWitness::default(),
            }],
            &mut [],
            &[],
            &frontier,
            &[(
                0,
                1,
                false,
                false,
                SupportSeam {
                    edge: (IVec3::ZERO, IVec3::Z),
                    boundary_to_edge_hadamard: false,
                },
            )],
            &[],
            "test",
        )
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].support.get(SupportTarget::Node(response)), Pauli::X);
    }

    #[test]
    fn signed_spatial_hadamard_y_seam_has_both_minus_factors() {
        let edge = (IVec3::ZERO, IVec3::Y);
        let row = SupportedRow {
            signed: PhasedPauliString::positive(PauliString::from_terms(
                2,
                [(0, Pauli::Y), (1, Pauli::Y)],
            )),
            support: PositionedSupport::default(),
            witness: FlowWitness::default(),
        };
        let closed = compose_certificate_rows(
            vec![row],
            &[(
                0,
                1,
                true,
                true,
                SupportSeam {
                    edge,
                    boundary_to_edge_hadamard: false,
                },
            )],
            &[],
        );
        assert_eq!(closed.len(), 1);
        assert_eq!(
            closed[0].signed.phase(),
            0,
            "HYH and Y transpose each add -1"
        );
        assert_eq!(closed[0].support.edges[&edge], Pauli::Y);
    }
}
