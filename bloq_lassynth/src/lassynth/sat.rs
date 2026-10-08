//! SAT encoding and graph reconstruction for fixed-volume synthesis.

use std::collections::{HashMap, HashSet, VecDeque};

use bloq_graph::{Block, BlockGraph, BlockGraphError, BlockKind, CubeKind, MeasureTarget, Pipe};
use bloq_utils::{Basis, Direction, Pauli, UDirection};
use glam::IVec3;
use rustsat::{
    instances::Cnf,
    solvers::SolverResult,
    types::{Clause, Lit, Var},
};

use super::{PipeKey, Port, SynthesisError, SynthesisProblem, in_bounds};
use crate::monitor::SynthesisMonitor;
use crate::{Construction, Deadline};

mod backend;

use backend::Backend;

const AXES: [UDirection; 3] = [UDirection::X, UDirection::Y, UDirection::Z];

fn port_pipe(port: &Port) -> PipeKey {
    port.pipe_key()
        .expect("synthesis problems validate port coordinates before encoding")
}

pub(super) async fn solve(
    problem: &SynthesisProblem,
    deadline: Deadline,
    monitor: &SynthesisMonitor,
) -> Result<Solution, SynthesisError> {
    deadline.check(monitor)?;
    let mut work = Construction::new(&deadline, monitor);
    let mut cnf = CnfBuilder::default();
    let variables = Variables::new(&mut cnf, problem, &mut work).await?;
    encode(problem, &variables, &mut cnf, &mut work).await?;
    let mut backend = Backend::new(cnf.cnf, deadline.clone(), monitor.clone()).await?;
    deadline.check(monitor)?;
    monitor.begin_solve(problem.size);
    match backend.solve()? {
        SolverResult::Sat => {}
        SolverResult::Unsat => return Err(SynthesisError::Unsatisfiable),
        SolverResult::Interrupted => return Err(SynthesisError::Interrupted),
    }

    deadline.check(monitor)?;
    let assignment = Assignment::read(&backend, &variables)?;
    deadline.check(monitor)?;
    let graph = emit_graph(problem, &assignment)?;
    deadline.check(monitor)?;
    let witnesses = measurement_witnesses(problem, &backend, &variables, &assignment, &graph)?;
    deadline.check(monitor)?;
    Ok(Solution { graph, witnesses })
}

pub(super) struct Solution {
    pub(super) graph: BlockGraph,
    pub(super) witnesses: Vec<Vec<MeasureTarget>>,
}

#[derive(Default)]
struct CnfBuilder {
    cnf: Cnf,
    next_var: u32,
}

impl CnfBuilder {
    fn new_var(&mut self) -> Result<Var, SynthesisError> {
        let var = Var::new_with_error(self.next_var)
            .map_err(|error| SynthesisError::Solver(error.into()))?;
        self.next_var += 1;
        Ok(var)
    }

    async fn vars(
        &mut self,
        count: usize,
        work: &mut Construction<'_>,
    ) -> Result<Vec<Var>, SynthesisError> {
        work.deadline.check(work.monitor)?;
        if !u32::try_from(count)
            .ok()
            .and_then(|count| self.next_var.checked_add(count))
            .is_some_and(|end| end <= Var::MAX_IDX + 1)
        {
            return Err(SynthesisError::Solver("SAT variable limit exceeded".into()));
        }
        let mut vars = Vec::new();
        vars.try_reserve_exact(count)
            .map_err(|error| SynthesisError::Solver(error.into()))?;
        for _ in 0..count {
            work.checkpoint().await?;
            vars.push(self.new_var()?);
        }
        Ok(vars)
    }

    fn add(&mut self, literals: impl IntoIterator<Item = Lit>) {
        self.cnf
            .add_clause(literals.into_iter().collect::<Clause>());
    }

    fn force(&mut self, var: Var, value: bool) {
        self.cnf
            .add_unit(if value { var.pos_lit() } else { var.neg_lit() });
    }

    fn guarded_equal(&mut self, guard_false: &[Lit], left: Var, right: Var) {
        self.add(
            guard_false
                .iter()
                .copied()
                .chain([left.neg_lit(), right.pos_lit()]),
        );
        self.add(
            guard_false
                .iter()
                .copied()
                .chain([left.pos_lit(), right.neg_lit()]),
        );
    }

    fn guarded_different(&mut self, guard_false: &[Lit], left: Var, right: Var) {
        self.add(
            guard_false
                .iter()
                .copied()
                .chain([left.pos_lit(), right.pos_lit()]),
        );
        self.add(
            guard_false
                .iter()
                .copied()
                .chain([left.neg_lit(), right.neg_lit()]),
        );
    }

    fn guarded_even_parity(&mut self, guard_false: &[Lit], terms: &[Var]) {
        for assignment in 0usize..(1usize << terms.len()) {
            if assignment.count_ones() % 2 == 0 {
                continue;
            }
            self.add(
                guard_false
                    .iter()
                    .copied()
                    .chain(terms.iter().enumerate().map(|(index, var)| {
                        if assignment & (1 << index) == 0 {
                            var.pos_lit()
                        } else {
                            var.neg_lit()
                        }
                    })),
            );
        }
    }
}

struct Variables {
    size: IVec3,
    exists: [Vec<Var>; 3],
    y: Vec<Var>,
    colors: [Vec<Var>; 4],
    correlations: Vec<[Vec<Var>; 6]>,
}

impl Variables {
    async fn new(
        cnf: &mut CnfBuilder,
        problem: &SynthesisProblem,
        work: &mut Construction<'_>,
    ) -> Result<Self, SynthesisError> {
        let cell_count = usize::try_from(problem.size.x * problem.size.y * problem.size.z)
            .expect("validated positive synthesis dimensions fit usize");
        let mut vars = Self {
            size: problem.size,
            exists: Default::default(),
            y: Vec::new(),
            colors: Default::default(),
            correlations: Vec::new(),
        };
        for (axis, slots) in vars.exists.iter_mut().enumerate() {
            *slots = cnf
                .vars(pipe_cell_count(problem.size, AXES[axis]), work)
                .await?;
        }
        vars.y = cnf.vars(cell_count, work).await?;
        for (index, slots) in vars.colors.iter_mut().enumerate() {
            *slots = cnf
                .vars(pipe_cell_count(problem.size, AXES[index / 2]), work)
                .await?;
        }
        for _ in &problem.stabilizers {
            let mut row = std::array::from_fn(|_| Vec::new());
            for (index, slots) in row.iter_mut().enumerate() {
                *slots = cnf
                    .vars(pipe_cell_count(problem.size, AXES[index / 2]), work)
                    .await?;
            }
            vars.correlations.push(row);
        }
        Ok(vars)
    }

    fn pipe(&self, key: PipeKey) -> Var {
        self.exists[key.axis.index()][pipe_index(self.size, key)]
    }

    fn y(&self, position: IVec3) -> Var {
        self.y[cell_index(self.size, position)]
    }

    fn color(&self, key: PipeKey, upper: bool) -> Var {
        self.colors[key.axis.index() * 2 + usize::from(upper)][pipe_index(self.size, key)]
    }

    fn correlation(&self, stabilizer: usize, key: PipeKey, surface_axis: UDirection) -> Var {
        self.correlations[stabilizer][correlation_index(key.axis, surface_axis)]
            [pipe_index(self.size, key)]
    }
}

fn pipe_cell_count(size: IVec3, axis: UDirection) -> usize {
    let mut dimensions = size.to_array().map(|value| {
        usize::try_from(value).expect("validated positive synthesis dimensions fit usize")
    });
    dimensions[axis.index()] += 1;
    dimensions.into_iter().product()
}

fn pipe_index(size: IVec3, key: PipeKey) -> usize {
    debug_assert!(pipe_in_bounds(size, key));
    let mut position = key.position;
    position[key.axis.index()] += 1;
    let mut dimensions = size;
    dimensions[key.axis.index()] += 1;
    cell_index(dimensions, position)
}

fn cell_index(size: IVec3, position: IVec3) -> usize {
    usize::try_from((position.x * size.y + position.y) * size.z + position.z)
        .expect("validated in-volume position has a nonnegative index")
}

#[derive(Debug, Clone, Copy)]
struct IncidentPipe {
    key: PipeKey,
    exists: Var,
}

async fn encode(
    problem: &SynthesisProblem,
    vars: &Variables,
    cnf: &mut CnfBuilder,
    work: &mut Construction<'_>,
) -> Result<(), SynthesisError> {
    encode_spatial_colors(problem, vars, cnf, work).await?;
    encode_boundary(problem, vars, cnf, work).await?;
    encode_junctions(problem, vars, cnf, work).await?;
    encode_correlations(problem, vars, cnf, work).await?;
    encode_measurement_anchors(problem, vars, cnf, work).await
}

/// An absent spatial pipe has no color, and without the opt-in its two
/// endpoint colors agree, which is what forbids a Hadamard decoration.
async fn encode_spatial_colors(
    problem: &SynthesisProblem,
    vars: &Variables,
    cnf: &mut CnfBuilder,
    work: &mut Construction<'_>,
) -> Result<(), SynthesisError> {
    for key in pipe_keys(problem.size).filter(|key| key.axis.is_spatial()) {
        work.checkpoint().await?;
        let exists = vars.pipe(key);
        let colors = [vars.color(key, false), vars.color(key, true)];
        for color in colors {
            cnf.add([exists.pos_lit(), color.neg_lit()]);
        }
        if !problem.allow_spatial_hadamard {
            cnf.guarded_equal(&[], colors[0], colors[1]);
        }
    }
    Ok(())
}

/// Ports are the only way through the surface of the box: each one pins its
/// own pipe and color and excludes every other pipe at its position, and every
/// remaining pipe that leaves the volume is forced absent.
async fn encode_boundary(
    problem: &SynthesisProblem,
    vars: &Variables,
    cnf: &mut CnfBuilder,
    work: &mut Construction<'_>,
) -> Result<(), SynthesisError> {
    let mut port_pipes = HashSet::new();
    for port in &problem.ports {
        work.checkpoint().await?;
        let key = port_pipe(port);
        port_pipes.insert(key);
        cnf.force(vars.pipe(key), true);
        if key.axis.is_spatial() {
            cnf.force(
                vars.color(key, pipe_endpoint(key, port.position)),
                port_color(port),
            );
        }
        for incident in incident_pipes(vars, port.position) {
            if incident.key != key {
                cnf.force(incident.exists, false);
            }
        }
    }

    for key in pipe_keys(problem.size) {
        work.checkpoint().await?;
        if !internal_pipe(problem.size, key) && !port_pipes.contains(&key) {
            cnf.force(vars.pipe(key), false);
        }
    }
    Ok(())
}

/// Per-cell legality: which pipes a Y cube may touch, that no cube hosts all
/// three axes at once, that a non-Y cube with a pipe has a second one, and how
/// two spatial pipes meeting at a cube constrain each other's colors.
async fn encode_junctions(
    problem: &SynthesisProblem,
    vars: &Variables,
    cnf: &mut CnfBuilder,
    work: &mut Construction<'_>,
) -> Result<(), SynthesisError> {
    for position in positions(problem.size) {
        work.checkpoint().await?;
        let incident = incident_pipes(vars, position);
        let y = vars.y(position);

        for pipe in incident.iter().filter(|pipe| pipe.key.axis.is_spatial()) {
            cnf.add([y.neg_lit(), pipe.exists.neg_lit()]);
        }
        let temporal = incident
            .iter()
            .filter(|pipe| pipe.key.axis == UDirection::Z)
            .collect::<Vec<_>>();
        if temporal.len() == 2 {
            cnf.add([
                y.neg_lit(),
                temporal[0].exists.neg_lit(),
                temporal[1].exists.neg_lit(),
            ]);
        }

        let by_axis: [Vec<_>; 3] = std::array::from_fn(|axis| {
            incident
                .iter()
                .filter(|pipe| pipe.key.axis.index() == axis)
                .collect()
        });
        for x in &by_axis[0] {
            for y_pipe in &by_axis[1] {
                for z in &by_axis[2] {
                    cnf.add([
                        x.exists.neg_lit(),
                        y_pipe.exists.neg_lit(),
                        z.exists.neg_lit(),
                    ]);
                }
            }
        }

        for pipe in &incident {
            cnf.add(
                [y.pos_lit(), pipe.exists.neg_lit()].into_iter().chain(
                    incident
                        .iter()
                        .filter(|other| other.key != pipe.key)
                        .map(|other| other.exists.pos_lit()),
                ),
            );
        }

        let spatial = incident
            .iter()
            .filter(|pipe| pipe.key.axis.is_spatial())
            .collect::<Vec<_>>();
        for left in 0..spatial.len() {
            for right in left + 1..spatial.len() {
                let guard = [
                    spatial[left].exists.neg_lit(),
                    spatial[right].exists.neg_lit(),
                ];
                let (left_color, right_color) = (
                    vars.color(
                        spatial[left].key,
                        pipe_endpoint(spatial[left].key, position),
                    ),
                    vars.color(
                        spatial[right].key,
                        pipe_endpoint(spatial[right].key, position),
                    ),
                );
                if spatial[left].key.axis == spatial[right].key.axis {
                    cnf.guarded_equal(&guard, left_color, right_color);
                } else {
                    cnf.guarded_different(&guard, left_color, right_color);
                }
            }
        }
    }
    Ok(())
}

async fn encode_measurement_anchors(
    problem: &SynthesisProblem,
    vars: &Variables,
    cnf: &mut CnfBuilder,
    work: &mut Construction<'_>,
) -> Result<(), SynthesisError> {
    // ponytail: port-adjacent owners guarantee connectivity; add SAT
    // reachability before allowing arbitrary spacelike owner edges.
    for &row in &problem.measurement_rows {
        let mut anchors = Vec::new();
        let mut candidates = Vec::new();
        for (index, port) in problem.ports.iter().enumerate() {
            work.checkpoint().await?;
            if problem.stabilizers[row].get(index) != Pauli::I {
                candidates.extend(
                    incident_keys(vars.size, port.position + port.direction.to_ivec3())
                        .into_iter()
                        .filter(|key| key.axis.is_spatial() && internal_pipe(vars.size, *key)),
                );
            }
        }
        if candidates.is_empty() {
            return Err(SynthesisError::NoMeasurementAnchor {
                row,
                size: problem.size,
            });
        }
        candidates.sort_by_key(|key| (key.position.to_array(), key.axis.index()));
        candidates.dedup();
        for key in candidates {
            work.checkpoint().await?;
            let surface_axis = transverse_axes(key.axis)
                .into_iter()
                .find(|axis| axis.is_spatial())
                .expect("a spatial pipe has one spatial surface axis");
            let anchor = cnf.new_var()?;
            anchors.push(anchor.pos_lit());
            cnf.add([
                anchor.neg_lit(),
                vars.correlation(row, key, surface_axis).pos_lit(),
            ]);
            for other in 0..problem.stabilizers.len() {
                work.checkpoint().await?;
                if other != row {
                    cnf.add([
                        anchor.neg_lit(),
                        vars.correlation(other, key, surface_axis).neg_lit(),
                    ]);
                }
            }
        }
        cnf.add(anchors);
    }
    Ok(())
}

async fn encode_correlations(
    problem: &SynthesisProblem,
    vars: &Variables,
    cnf: &mut CnfBuilder,
    work: &mut Construction<'_>,
) -> Result<(), SynthesisError> {
    // A correlation surface only lives on a pipe that exists. Pinning the
    // absent-pipe bits to false lets the parity constraints below read the
    // correlation variable directly instead of an `exists ∧ correlation`
    // Tseitin conjunction, and removes the don't-care symmetry that would
    // otherwise multiply every model by 2^(absent surface bits).
    for stabilizer_index in 0..problem.stabilizers.len() {
        for key in pipe_keys(problem.size) {
            work.checkpoint().await?;
            let exists = vars.pipe(key);
            for surface_axis in transverse_axes(key.axis) {
                cnf.add([
                    exists.pos_lit(),
                    vars.correlation(stabilizer_index, key, surface_axis)
                        .neg_lit(),
                ]);
            }
        }
    }

    for (stabilizer_index, stabilizer) in problem.stabilizers.iter().enumerate() {
        for (port_index, port) in problem.ports.iter().enumerate() {
            work.checkpoint().await?;
            let key = port_pipe(port);
            let pauli = stabilizer.get(port_index);
            for surface_axis in transverse_axes(key.axis) {
                let present = port_surface(port, pauli, surface_axis);
                cnf.force(
                    vars.correlation(stabilizer_index, key, surface_axis),
                    present,
                );
            }
        }

        for position in positions(problem.size) {
            work.checkpoint().await?;
            let incident = incident_pipes(vars, position);
            let y = vars.y(position);
            for pipe in incident
                .iter()
                .filter(|pipe| pipe.key.axis == UDirection::Z)
            {
                cnf.guarded_equal(
                    &[y.neg_lit()],
                    vars.correlation(stabilizer_index, pipe.key, UDirection::X),
                    vars.correlation(stabilizer_index, pipe.key, UDirection::Y),
                );
            }

            for normal in UDirection::iter() {
                let normal_pipes = incident
                    .iter()
                    .filter(|pipe| pipe.key.axis == normal)
                    .collect::<Vec<_>>();
                let crossing = incident
                    .iter()
                    .filter(|pipe| pipe.key.axis != normal)
                    .collect::<Vec<_>>();
                let guard_false = std::iter::once(y.pos_lit())
                    .chain(normal_pipes.iter().map(|pipe| pipe.exists.pos_lit()))
                    .collect::<Vec<_>>();

                for left in 0..crossing.len() {
                    for right in left + 1..crossing.len() {
                        let left_surface = third_axis(normal, crossing[left].key.axis);
                        let right_surface = third_axis(normal, crossing[right].key.axis);
                        let pair_guard = guard_false
                            .iter()
                            .copied()
                            .chain([
                                crossing[left].exists.neg_lit(),
                                crossing[right].exists.neg_lit(),
                            ])
                            .collect::<Vec<_>>();
                        cnf.guarded_equal(
                            &pair_guard,
                            vars.correlation(stabilizer_index, crossing[left].key, left_surface),
                            vars.correlation(stabilizer_index, crossing[right].key, right_surface),
                        );
                    }
                }

                let terms = crossing
                    .iter()
                    .map(|pipe| vars.correlation(stabilizer_index, pipe.key, normal))
                    .collect::<Vec<_>>();
                cnf.guarded_even_parity(&guard_false, &terms);
            }
        }
    }
    Ok(())
}

struct Assignment {
    size: IVec3,
    exists: [Vec<bool>; 3],
    y: Vec<bool>,
    colors: [Vec<bool>; 4],
}

impl Assignment {
    fn read(backend: &Backend, vars: &Variables) -> Result<Self, SynthesisError> {
        let read = |var: Var| backend.value(var);
        let all = |vars: &[Var]| -> Result<Vec<bool>, SynthesisError> {
            vars.iter().copied().map(&read).collect()
        };
        Ok(Self {
            size: vars.size,
            exists: [
                all(&vars.exists[0])?,
                all(&vars.exists[1])?,
                all(&vars.exists[2])?,
            ],
            y: all(&vars.y)?,
            colors: [
                all(&vars.colors[0])?,
                all(&vars.colors[1])?,
                all(&vars.colors[2])?,
                all(&vars.colors[3])?,
            ],
        })
    }

    fn pipe(&self, key: PipeKey) -> bool {
        self.exists[key.axis.index()][pipe_index(self.size, key)]
    }

    fn y(&self, position: IVec3) -> bool {
        in_bounds(self.size, position) && self.y[cell_index(self.size, position)]
    }

    /// Reads one endpoint color of an X- or Y-axis pipe.
    fn color(&self, key: PipeKey, upper: bool) -> bool {
        self.colors[key.axis.index() * 2 + usize::from(upper)][pipe_index(self.size, key)]
    }
}

fn measurement_witnesses(
    problem: &SynthesisProblem,
    backend: &Backend,
    vars: &Variables,
    assignment: &Assignment,
    graph: &BlockGraph,
) -> Result<Vec<Vec<MeasureTarget>>, SynthesisError> {
    let mut witnesses = vec![Vec::new(); vars.correlations.len()];
    for &stabilizer in &problem.measurement_rows {
        let targets = &mut witnesses[stabilizer];
        for key in pipe_keys(vars.size)
            .filter(|key| key.axis.is_spatial() && internal_pipe(vars.size, *key))
        {
            if !assignment.pipe(key)
                || graph
                    .get_pipe(key.position, key.position + key.axis.to_ivec3())
                    .is_none()
            {
                continue;
            }
            for surface_axis in transverse_axes(key.axis) {
                // On a spacelike pipe, the spatial-normal component is
                // the parity record; the time-normal one is transport.
                if !surface_axis.is_spatial()
                    || !backend.value(vars.correlation(stabilizer, key, surface_axis))?
                {
                    continue;
                }
                let mut unique = true;
                for other in 0..vars.correlations.len() {
                    if other != stabilizer
                        && backend.value(vars.correlation(other, key, surface_axis))?
                    {
                        unique = false;
                        break;
                    }
                }
                if unique {
                    targets.push(MeasureTarget::Edge {
                        src: key.position,
                        dir: positive_direction(key.axis),
                    });
                }
            }
        }
        targets.sort_by_key(|target| match target {
            MeasureTarget::Edge { src, dir } => {
                (src.z, src.to_array(), dir.as_udirection().index())
            }
            MeasureTarget::Node(_) => {
                unreachable!("measurement witnesses are collected from pipe edges")
            }
        });
        targets.dedup();
    }
    Ok(witnesses)
}

fn emit_graph(
    problem: &SynthesisProblem,
    assignment: &Assignment,
) -> Result<BlockGraph, SynthesisError> {
    let pipes = connected_pipes(problem, assignment);
    let placed = pipes.iter().copied().collect::<HashSet<_>>();
    // Spatial colors come straight from SAT. Temporal endpoint colors are
    // recovered by the smaller XOR system below.
    let spatial_colors = pipes
        .iter()
        .copied()
        .filter(|key| key.axis.is_spatial())
        .map(|key| {
            (
                key,
                (assignment.color(key, false), assignment.color(key, true)),
            )
        })
        .collect::<HashMap<_, _>>();
    let temporal_colors = color_temporal_pipes(problem, assignment, &pipes, &spatial_colors)?;
    let port_by_position = problem
        .ports
        .iter()
        .map(|port| (port.position, port))
        .collect::<HashMap<_, _>>();
    let mut positions = pipes
        .iter()
        .flat_map(|key| [key.position, key.position + key.axis.to_ivec3()])
        .collect::<Vec<_>>();
    positions.extend(problem.ports.iter().map(|port| port.position));
    positions.sort_by_key(IVec3::to_array);
    positions.dedup();

    let mut graph = BlockGraph::new();
    for position in positions {
        let block = if let Some(port) = port_by_position.get(&position) {
            let block = Block::new(position, BlockKind::Port)
                .with_port_role(port.role)
                .map_err(BlockGraphError::from)?;
            if let Some(tag) = &port.tag {
                block.with_tag(tag.clone())?
            } else {
                block
            }
        } else if assignment.y(position) {
            Block::new(position, BlockKind::Y)
        } else {
            let kind = cube_kind(position, &placed, &spatial_colors, &temporal_colors)?;
            Block::new(position, BlockKind::Cube(kind))
        };
        graph.try_add_block(block)?;
    }

    for key in &pipes {
        let mut pipe = Pipe::new(key.position, positive_direction(key.axis));
        let colors = if key.axis.is_spatial() {
            spatial_colors[key]
        } else {
            temporal_colors[key]
        };
        if colors.0 != colors.1 {
            pipe = pipe.with_hadamard();
        }
        graph.try_add_pipe(pipe)?;
    }
    Ok(graph)
}

fn connected_pipes(problem: &SynthesisProblem, assignment: &Assignment) -> Vec<PipeKey> {
    let all = pipe_keys(problem.size)
        .filter(|key| assignment.pipe(*key))
        .collect::<Vec<_>>();
    let mut adjacency = HashMap::<IVec3, Vec<IVec3>>::new();
    for &key in &all {
        let dst = key.position + key.axis.to_ivec3();
        adjacency.entry(key.position).or_default().push(dst);
        adjacency.entry(dst).or_default().push(key.position);
    }

    let mut reached = HashSet::new();
    let mut queue = problem
        .ports
        .iter()
        .map(|port| port.position)
        .collect::<VecDeque<_>>();
    while let Some(position) = queue.pop_front() {
        if !reached.insert(position) {
            continue;
        }
        for &neighbor in adjacency.get(&position).into_iter().flatten() {
            if !assignment.y(neighbor) && !reached.contains(&neighbor) {
                queue.push_back(neighbor);
            }
        }
    }

    all.into_iter()
        .filter(|key| {
            reached.contains(&key.position)
                || reached.contains(&(key.position + key.axis.to_ivec3()))
        })
        .collect()
}

fn color_temporal_pipes(
    problem: &SynthesisProblem,
    assignment: &Assignment,
    pipes: &[PipeKey],
    spatial_colors: &HashMap<PipeKey, (bool, bool)>,
) -> Result<HashMap<PipeKey, (bool, bool)>, SynthesisError> {
    let temporal = pipes
        .iter()
        .copied()
        .filter(|key| key.axis == UDirection::Z)
        .collect::<Vec<_>>();
    let indices = temporal
        .iter()
        .copied()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect::<HashMap<_, _>>();
    let mut relations = vec![Vec::new(); temporal.len() * 2];
    let mut fixed = vec![None; temporal.len() * 2];

    for (index, &key) in temporal.iter().enumerate() {
        let lower = key.position;
        let upper = key.position + IVec3::Z;
        for (slot, endpoint) in [(index * 2, lower), (index * 2 + 1, upper)] {
            for spatial in incident_keys(problem.size, endpoint)
                .into_iter()
                .filter(|candidate| candidate.axis.is_spatial())
            {
                if let Some(&colors) = spatial_colors.get(&spatial) {
                    let color = if pipe_endpoint(spatial, endpoint) {
                        colors.1
                    } else {
                        colors.0
                    };
                    set_color(&mut fixed[slot], !color)?;
                }
            }
        }
        if assignment.y(lower) || assignment.y(upper) {
            relations[index * 2].push(index * 2 + 1);
            relations[index * 2 + 1].push(index * 2);
        }
    }

    for position in positions(problem.size) {
        if assignment.y(position) {
            continue;
        }
        let below = PipeKey {
            position: position - IVec3::Z,
            axis: UDirection::Z,
        };
        let above = PipeKey {
            position,
            axis: UDirection::Z,
        };
        if let (Some(&below), Some(&above)) = (indices.get(&below), indices.get(&above)) {
            relations[below * 2 + 1].push(above * 2);
            relations[above * 2].push(below * 2 + 1);
        }
    }

    for port in &problem.ports {
        let key = port_pipe(port);
        if let Some(&index) = indices.get(&key) {
            let slot = if port.position == key.position {
                index * 2
            } else {
                index * 2 + 1
            };
            set_color(&mut fixed[slot], port_color(port))?;
        }
    }

    let values = solve_color_components(&relations, &fixed)?;
    Ok(temporal
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, (values[index * 2], values[index * 2 + 1])))
        .collect())
}

fn solve_color_components(
    relations: &[Vec<usize>],
    fixed: &[Option<bool>],
) -> Result<Vec<bool>, SynthesisError> {
    let mut visited = vec![false; relations.len()];
    let mut result = vec![false; relations.len()];
    for start in 0..relations.len() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut component = Vec::new();
        let mut queue = VecDeque::from([start]);
        while let Some(node) = queue.pop_front() {
            component.push(node);
            for &neighbor in &relations[node] {
                if !visited[neighbor] {
                    visited[neighbor] = true;
                    queue.push_back(neighbor);
                }
            }
        }
        let mut value = None;
        for &node in &component {
            if let Some(fixed) = fixed[node] {
                if value.is_some_and(|value| value != fixed) {
                    return Err(SynthesisError::Unsatisfiable);
                }
                value = Some(fixed);
            }
        }
        let value = value.unwrap_or(false);
        for node in component {
            result[node] = value;
        }
    }
    Ok(result)
}

fn set_color(slot: &mut Option<bool>, value: bool) -> Result<(), SynthesisError> {
    if slot.is_some_and(|existing| existing != value) {
        Err(SynthesisError::Unsatisfiable)
    } else {
        *slot = Some(value);
        Ok(())
    }
}

fn cube_kind(
    position: IVec3,
    pipes: &HashSet<PipeKey>,
    spatial_colors: &HashMap<PipeKey, (bool, bool)>,
    temporal_colors: &HashMap<PipeKey, (bool, bool)>,
) -> Result<CubeKind, SynthesisError> {
    let mut bases = [None; 3];
    for key in incident_keys_unbounded(position) {
        if !pipes.contains(&key) {
            continue;
        }
        let color = if key.axis.is_spatial() {
            let colors = spatial_colors[&key];
            if pipe_endpoint(key, position) {
                colors.1
            } else {
                colors.0
            }
        } else {
            let (lower, upper) = temporal_colors[&key];
            if position == key.position {
                lower
            } else {
                upper
            }
        };
        let transverse = transverse_axes(key.axis);
        let z_axis = transverse[usize::from(color)];
        let x_axis = transverse[usize::from(!color)];
        assign_basis(&mut bases, z_axis, Basis::Z)?;
        assign_basis(&mut bases, x_axis, Basis::X)?;
    }
    let missing = bases
        .iter()
        .enumerate()
        .filter_map(|(index, basis)| basis.is_none().then_some(index))
        .collect::<Vec<_>>();
    for mask in 0usize..(1usize << missing.len()) {
        let mut candidate = bases;
        for (bit, &axis) in missing.iter().enumerate() {
            candidate[axis] = Some(if mask & (1 << bit) == 0 {
                Basis::Z
            } else {
                Basis::X
            });
        }
        let candidate = candidate.map(|basis| basis.expect("all missing bases were filled"));
        if let Ok(kind) = CubeKind::try_from(candidate) {
            return Ok(kind);
        }
    }
    Err(SynthesisError::Solver(
        format!("SAT model produced inconsistent pipe colors at {position}").into(),
    ))
}

fn assign_basis(
    bases: &mut [Option<Basis>; 3],
    axis: UDirection,
    basis: Basis,
) -> Result<(), SynthesisError> {
    let slot = &mut bases[axis.index()];
    if slot.is_some_and(|existing| existing != basis) {
        Err(SynthesisError::Unsatisfiable)
    } else {
        *slot = Some(basis);
        Ok(())
    }
}

fn incident_pipes(vars: &Variables, position: IVec3) -> Vec<IncidentPipe> {
    incident_keys(vars.size, position)
        .into_iter()
        .map(|key| IncidentPipe {
            key,
            exists: vars.pipe(key),
        })
        .collect()
}

fn incident_keys(size: IVec3, position: IVec3) -> Vec<PipeKey> {
    incident_keys_unbounded(position)
        .into_iter()
        .filter(|key| pipe_in_bounds(size, *key))
        .collect()
}

fn incident_keys_unbounded(position: IVec3) -> Vec<PipeKey> {
    UDirection::iter()
        .flat_map(|axis| {
            [
                PipeKey { position, axis },
                PipeKey {
                    position: position - axis.to_ivec3(),
                    axis,
                },
            ]
        })
        .collect()
}

fn positions(size: IVec3) -> impl Iterator<Item = IVec3> {
    (0..size.x).flat_map(move |x| {
        (0..size.y).flat_map(move |y| (0..size.z).map(move |z| IVec3::new(x, y, z)))
    })
}

fn pipe_keys(size: IVec3) -> impl Iterator<Item = PipeKey> {
    UDirection::iter().flat_map(move |axis| {
        let step = axis.to_ivec3();
        positions(size)
            .filter(move |position| position[axis.index()] == 0)
            .map(move |position| PipeKey {
                position: position - step,
                axis,
            })
            .chain(positions(size).map(move |position| PipeKey { position, axis }))
    })
}

fn pipe_in_bounds(size: IVec3, key: PipeKey) -> bool {
    in_bounds(size, key.position)
        || key
            .position
            .checked_add(key.axis.to_ivec3())
            .is_some_and(|position| in_bounds(size, position))
}

fn internal_pipe(size: IVec3, key: PipeKey) -> bool {
    in_bounds(size, key.position) && in_bounds(size, key.position + key.axis.to_ivec3())
}

fn port_color(port: &Port) -> bool {
    port.z_basis == transverse_axes(port.direction.as_udirection())[1]
}

fn pipe_endpoint(key: PipeKey, position: IVec3) -> bool {
    position != key.position
}

fn port_surface(port: &Port, pauli: Pauli, surface_axis: UDirection) -> bool {
    match pauli {
        Pauli::I => false,
        Pauli::Y => true,
        Pauli::Z => surface_axis == port.z_basis,
        Pauli::X => surface_axis != port.z_basis,
    }
}

fn transverse_axes(axis: UDirection) -> [UDirection; 2] {
    match axis {
        UDirection::X => [UDirection::Y, UDirection::Z],
        UDirection::Y => [UDirection::Z, UDirection::X],
        UDirection::Z => [UDirection::X, UDirection::Y],
    }
}

fn third_axis(first: UDirection, second: UDirection) -> UDirection {
    UDirection::iter()
        .find(|axis| *axis != first && *axis != second)
        .expect("two distinct axes have exactly one transverse axis")
}

fn correlation_index(pipe_axis: UDirection, surface_axis: UDirection) -> usize {
    match (pipe_axis, surface_axis) {
        (UDirection::X, UDirection::Y) => 0,
        (UDirection::X, UDirection::Z) => 1,
        (UDirection::Y, UDirection::X) => 2,
        (UDirection::Y, UDirection::Z) => 3,
        (UDirection::Z, UDirection::X) => 4,
        (UDirection::Z, UDirection::Y) => 5,
        _ => unreachable!("correlation surface is transverse to its pipe"),
    }
}

fn positive_direction(axis: UDirection) -> Direction {
    match axis {
        UDirection::X => Direction::XPLUS,
        UDirection::Y => Direction::YPLUS,
        UDirection::Z => Direction::ZPLUS,
    }
}

#[cfg(test)]
mod tests {
    use bloq_utils::PauliString;

    use super::*;
    use crate::executor::block_on;

    #[test]
    fn exhausted_sat_variable_indices_return_an_error() {
        use std::error::Error as _;

        let mut cnf = CnfBuilder {
            next_var: Var::MAX_IDX,
            ..CnfBuilder::default()
        };
        assert_eq!(
            cnf.new_var().expect("last variable fits").idx32(),
            Var::MAX_IDX
        );
        let error = cnf.new_var().unwrap_err();
        assert!(matches!(error, SynthesisError::Solver(_)));
        assert!(matches!(
            error.source().unwrap().downcast_ref::<rustsat::types::TypeError>(),
            Some(rustsat::types::TypeError::IdxTooHigh(index, limit))
                if *index == Var::MAX_IDX + 1 && *limit == Var::MAX_IDX
        ));
        assert_eq!(cnf.next_var, Var::MAX_IDX + 1);

        let deadline = Deadline::none();
        let monitor = SynthesisMonitor::new();
        let mut work = Construction::new(&deadline, &monitor);
        assert!(matches!(
            block_on(cnf.vars(usize::MAX, &mut work)),
            Err(SynthesisError::Solver(_))
        ));
    }

    #[test]
    fn measurement_anchor_can_choose_either_adjacent_edge() {
        let problem = SynthesisProblem::new(
            IVec3::new(3, 1, 1),
            [
                Port::new(IVec3::new(1, 0, -1), Direction::ZPLUS, UDirection::Y),
                Port::new(IVec3::new(1, 0, 1), Direction::ZMINUS, UDirection::Y),
            ],
            [PauliString::try_from("ZI").unwrap()],
        )
        .with_measurement_rows([0]);
        let deadline = Deadline::none();
        let monitor = SynthesisMonitor::new();
        let mut work = Construction::new(&deadline, &monitor);
        let mut cnf = CnfBuilder::default();
        let vars = block_on(Variables::new(&mut cnf, &problem, &mut work)).unwrap();
        block_on(encode_measurement_anchors(
            &problem, &vars, &mut cnf, &mut work,
        ))
        .unwrap();
        let choices = cnf.cnf.iter().last().unwrap().clone();
        assert_eq!(choices.len(), 2);
        assert!(choices.iter().all(|literal| literal.is_pos()));
        for chosen in 0..2 {
            let mut formula = cnf.cnf.clone();
            for (index, literal) in choices.iter().enumerate() {
                formula.add_unit(if index == chosen {
                    *literal
                } else {
                    literal.var().neg_lit()
                });
            }
            let mut backend =
                block_on(Backend::new(formula, deadline.clone(), monitor.clone())).unwrap();
            assert_eq!(backend.solve().unwrap(), SolverResult::Sat);
        }
    }

    #[test]
    fn pruning_an_isolated_y_pair_preserves_boundary_checks() {
        let size = IVec3::new(2, 1, 3);
        let problem = SynthesisProblem::new(
            size,
            [
                Port::new(IVec3::NEG_Z, Direction::ZPLUS, UDirection::Y),
                Port::new(IVec3::new(0, 0, 3), Direction::ZMINUS, UDirection::Y),
            ],
            ["XX", "ZZ"].map(|row| PauliString::try_from(row).unwrap()),
        );
        let mut assignment = Assignment {
            size,
            exists: std::array::from_fn(|axis| vec![false; pipe_cell_count(size, AXES[axis])]),
            y: vec![false; 6],
            colors: std::array::from_fn(|index| {
                vec![false; pipe_cell_count(size, AXES[index / 2])]
            }),
        };
        for z in -1..3 {
            let key = PipeKey {
                position: IVec3::new(0, 0, z),
                axis: UDirection::Z,
            };
            assignment.exists[2][pipe_index(size, key)] = true;
        }
        let isolated = PipeKey {
            position: IVec3::new(1, 0, 0),
            axis: UDirection::Z,
        };
        assignment.exists[2][pipe_index(size, isolated)] = true;
        for position in [IVec3::X, IVec3::X + IVec3::Z] {
            assignment.y[cell_index(size, position)] = true;
        }
        let pipes = connected_pipes(&problem, &assignment);
        assert_eq!(pipes.len(), 4);
        assert!(!pipes.contains(&isolated));
        let graph = emit_graph(&problem, &assignment).unwrap();
        graph.validate().unwrap();
        let generated = graph.stabilizers().unwrap();
        super::super::verify_requested_stabilizers(&problem, &generated).unwrap();
        let mut invalid = problem;
        invalid.stabilizers = vec![PauliString::try_from("XI").unwrap()];
        assert!(matches!(
            super::super::verify_requested_stabilizers(&invalid, &generated),
            Err(SynthesisError::StabilizerMismatch { index: 0 })
        ));
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn oversized_sat_variable_allocation_returns_an_error() {
        let mut cnf = CnfBuilder::default();
        let deadline = Deadline::none();
        let monitor = SynthesisMonitor::new();
        let mut work = Construction::new(&deadline, &monitor);
        let count = isize::MAX as usize / std::mem::size_of::<Var>() + 1;
        assert!(matches!(
            block_on(cnf.vars(count, &mut work)),
            Err(SynthesisError::Solver(_))
        ));
        assert_eq!(cnf.next_var, 0);
    }

    #[test]
    fn encoding_checks_cancellation_before_finishing_cnf() {
        let problem = SynthesisProblem::new(IVec3::splat(5), [], []);
        let deadline = Deadline::none();
        let monitor = SynthesisMonitor::new();
        let mut work = Construction::new(&deadline, &monitor);
        let mut cnf = CnfBuilder::default();
        let vars = block_on(Variables::new(&mut cnf, &problem, &mut work)).unwrap();
        work.steps = 0;
        monitor.cancel();
        assert!(matches!(
            block_on(encode(&problem, &vars, &mut cnf, &mut work)),
            Err(SynthesisError::Cancelled)
        ));
        assert!(cnf.cnf.is_empty(), "cancelled encoding emits no clauses");
        assert_eq!(monitor.progress().solves, 0);
    }
}
