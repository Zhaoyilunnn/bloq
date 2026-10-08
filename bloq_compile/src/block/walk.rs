//! Walking block circuit construction.
//!
//! A walking step moves the surface code patch by one diagonal half-cell in
//! bloq coordinates. Bloq uses doubled, Y-up coordinates: data qubits start at
//! odd `(x, y)` positions and measure qubits start at even `(x, y)` positions.
//! Therefore a reference step such as `DOWN_RIGHT = 0.5 + 0.5i` corresponds to
//! the bloq step vector `(1, -1)`.
//!
//! Detector flows are constructed from regions that are tracked through each
//! step. Observable measurements are tracked separately from a
//! bloq-convention midline logical operator.

use std::sync::Arc;

use bloq_circuit::{
    Chunk, ChunkOrLoop, CoordCircuit, Flow, FlowMeasurements, GateType, MeasRecord, Op, PauliBasis,
    PauliMap,
};
use bloq_graph::{Basis, Direction, Pauli, WalkingKind};
use glam::IVec2;
use smallvec::SmallVec;

use crate::block::fixed_bulk::observable::line_qubits;
use crate::block::fixed_bulk::utils::{make_normal_surface_code_patch, reset_gate};
use crate::block::gateway::{
    ChunkMeasurements, GatewayEntry, LocalStabilizer, ObservableGateway, gateway_key_connectivity,
};
use crate::block::measurements::stage_chunks;
use crate::block::patch::{Patch, Tile};
use crate::block::{CompiledTemplate, LoweringTemplate};
use crate::signature::Connectivity;
use crate::{CompileError, FxSet, WalkError};

/// Compile a walking block into `2 * (distance + 1)` half-cell step chunks.
pub(super) fn compile_walking(
    kind: WalkingKind,
    connectivity: Connectivity,
    distance: u32,
) -> Result<CompiledTemplate, CompileError> {
    let top_basis = kind.boundary().y();
    let endpoint_basis = kind.boundary().z();
    let steps = walking_steps(kind.movement(), distance);
    let chunks = make_step_chunks(top_basis, distance, &steps, |index| {
        walking_step_mode(connectivity, endpoint_basis, index, steps.len())
    })?;
    let mut gateway = ObservableGateway::new();
    for basis in [Basis::X, Basis::Z] {
        let measurements = midline_observable_refs(
            &chunks,
            top_basis,
            basis,
            distance,
            &steps,
            !connectivity.has_pipe(Direction::ZPLUS),
        );
        let key = LocalStabilizer::new(
            Pauli::from(basis),
            walking_gateway_connectivity(connectivity),
        );
        // A walk slides the boundary operator without changing its logical
        // basis, so it keeps its basis and moves
        // its support by the walk's total displacement: `operator_in` is the
        // input patch's middle line, `operator_out` the same line shifted by the
        // accumulated steps (built in the template frame, where the step chunks
        // already place the moved patch). The line runs horizontally exactly when
        // the basis differs from `top_basis`, matching the neighbouring cubes.
        let pauli = Pauli::from(basis);
        let line = line_qubits(distance, basis != top_basis);
        let total: IVec2 = steps.iter().copied().sum();
        let face = |present: bool, shift: IVec2| -> PauliMap {
            if !present {
                return PauliMap::empty();
            }
            line.iter().map(|&q| (q + shift, pauli)).collect()
        };
        let operator_in = face(key.has_arm(Direction::ZMINUS), IVec2::ZERO);
        let operator_out = face(key.has_arm(Direction::ZPLUS), total);
        gateway.insert(
            key,
            GatewayEntry {
                measurements,
                operator_in,
                operator_out,
            },
        );
    }

    Ok(Arc::new(LoweringTemplate::from_chunks(chunks, gateway)?))
}

fn walking_step_mode(
    connectivity: Connectivity,
    basis: Basis,
    index: usize,
    step_count: usize,
) -> StepMode {
    if index == 0 && !connectivity.has_pipe(Direction::ZMINUS) {
        StepMode::Init { basis }
    } else if index + 1 == step_count && !connectivity.has_pipe(Direction::ZPLUS) {
        StepMode::Meas { basis }
    } else {
        StepMode::Bulk
    }
}

/// Gateway keys carry only the temporal pipes — a walk's spatial faces never
/// route an observable.
fn walking_gateway_connectivity(connectivity: Connectivity) -> Connectivity {
    gateway_key_connectivity(
        [Direction::ZPLUS, Direction::ZMINUS]
            .into_iter()
            .filter(|&dir| connectivity.has_pipe(dir)),
    )
}

/// Build one chunk per step, translating the steps that repeat.
///
/// Walking movement decomposes into a two-step cycle, so a bulk step and the
/// bulk step two positions later apply the same step vector to the same patch
/// shape, one cycle displacement apart. Their chunks are therefore translates
/// of each other — ops, flows and measurement ids included, because
/// [`emit_measurements`] scans a frame that moves rigidly with the patch — and
/// so are the patch and regions they carry. Such steps are translated instead
/// of rebuilt.
///
/// The one output that does not commute with translation is a flow's center:
/// [`region_center`] truncates a division, and truncation does not distribute
/// over a shift — a translated step's center can sit one cell off from what a
/// fresh build would compute. Centers are detector-coordinate annotations with
/// no semantic weight (they never enter flow matching), so translated steps
/// shift them along with everything else and accept the off-by-one. The
/// invariant for everything semantically observable was verified by an
/// instrumented sweep over every walking template in the registry and this
/// module's tests (81 instances, d=5/9/13, zero breaks).
fn make_step_chunks(
    top_basis: Basis,
    distance: u32,
    steps: &[IVec2],
    mode_at: impl Fn(usize) -> StepMode,
) -> Result<Vec<ChunkOrLoop>, CompileError> {
    let initial = CarriedState::initial(make_normal_surface_code_patch(distance, top_basis));
    // Indexed by step parity: at step `index`, slot `index % 2` holds the state
    // two steps back (what a translated step advances by one cycle), while slot
    // `(index + 1) % 2` holds the previous step's output, the input of a
    // freshly built step.
    let mut states = [initial.clone(), initial];
    let mut chunks = Vec::with_capacity(steps.len());
    let last = steps.len().saturating_sub(1);

    for (index, &step) in steps.iter().enumerate() {
        let mode = mode_at(index);
        let translatable = index >= 3
            && index != last
            && mode == StepMode::Bulk
            && mode_at(index - 2) == StepMode::Bulk
            && steps[index - 2] == step;

        if translatable {
            let cycle = steps[index - 1] + step;
            let chunk = translate_chunk(step_chunk(&chunks[index - 2]), cycle);
            states[index % 2].defer(cycle);
            chunks.push(ChunkOrLoop::Single(Box::new(chunk)));
            continue;
        }

        let previous = (index + 1) % 2;
        states[previous].settle();
        let build = make_step_chunk(
            &states[previous].patch,
            &states[previous].regions,
            step,
            top_basis,
            mode,
        )?;
        states[index % 2] = CarriedState::settled(build.patch, build.regions);
        chunks.push(ChunkOrLoop::Single(Box::new(build.chunk)));
    }

    Ok(chunks)
}

fn step_chunk(entry: &ChunkOrLoop) -> &Chunk {
    match entry {
        ChunkOrLoop::Single(chunk) => chunk,
        ChunkOrLoop::Loop { .. } => unreachable!("walking emits one single chunk per step"),
    }
}

/// Patch and regions crossing a step boundary, plus a translation the covariant
/// steps have deferred: a run of translated steps only bumps `pending`, and the
/// state is materialized when a step is built for real.
#[derive(Clone)]
struct CarriedState {
    patch: Patch,
    regions: Vec<Region>,
    pending: IVec2,
}

impl CarriedState {
    fn initial(patch: Patch) -> Self {
        let regions = initial_contracting_regions(&patch);
        Self::settled(patch, regions)
    }

    fn settled(patch: Patch, regions: Vec<Region>) -> Self {
        Self {
            patch,
            regions,
            pending: IVec2::ZERO,
        }
    }

    fn defer(&mut self, offset: IVec2) {
        self.pending += offset;
    }

    fn settle(&mut self) {
        if self.pending == IVec2::ZERO {
            return;
        }
        self.patch = translate_patch(&self.patch, self.pending);
        for region in &mut self.regions {
            region.translate(self.pending);
        }
        self.pending = IVec2::ZERO;
    }
}

/// Copy a step chunk with every coordinate shifted by `offset`. Measurement ids
/// are kept verbatim — the fresh build assigns the same ids, since it scans the
/// same frame translated.
fn translate_chunk(chunk: &Chunk, offset: IVec2) -> Chunk {
    let mut circuit = CoordCircuit::new();
    let mut records = chunk
        .circuit
        .meas_registry()
        .records()
        .iter()
        .map(|record| MeasRecord {
            id: record.id,
            qubit: record.qubit + offset,
        })
        .collect::<Vec<_>>();
    circuit.register_measurement_records(&mut records);
    let entry = circuit.entry_body();
    circuit
        .body_mut(entry)
        .expect("entry body is created with the circuit")
        .ops_mut()
        .extend(chunk_ops(chunk).iter().map(|op| translate_op(op, offset)));

    let flows = chunk
        .flows
        .iter()
        .map(|flow| Flow {
            start: flow.start.translated(offset),
            end: flow.end.translated(offset),
            measurements: flow.measurements.clone(),
            sign: flow.sign,
            center: flow.center.map(|c| c + offset),
            marker: flow.marker,
        })
        .collect();

    Chunk { circuit, flows }
}

fn translate_op(op: &Op, offset: IVec2) -> Op {
    let shift = |qubits: &[IVec2]| qubits.iter().map(|&q| q + offset).collect();
    match op {
        Op::Gate { gate, qubits } => Op::Gate {
            gate: *gate,
            qubits: shift(qubits),
        },
        Op::Measure {
            basis,
            qubits,
            measurements,
            flip_probability,
        } => Op::Measure {
            basis: *basis,
            qubits: shift(qubits),
            measurements: measurements.clone(),
            flip_probability: *flip_probability,
        },
        Op::Tick => Op::Tick,
        other => unreachable!("walking steps emit only gates, measurements and ticks: {other:?}"),
    }
}

fn midline_observable_refs(
    chunks: &[ChunkOrLoop],
    top_basis: Basis,
    observable_basis: Basis,
    distance: u32,
    steps: &[IVec2],
    include_final_data: bool,
) -> Vec<ChunkMeasurements> {
    let mut selected = Vec::with_capacity(steps.len());
    let mut patch_offset = IVec2::ZERO;
    // Pair each step with its staged chunk in one pass (walking emits one
    // `Single` chunk per step); resolving every stage index from the front
    // via `stage_chunk` made this loop quadratic in the step count.
    for (chunk_index, (&step, chunk)) in steps.iter().zip(stage_chunks(chunks)).enumerate() {
        let final_patch_offset =
            (include_final_data && chunk_index + 1 == steps.len()).then_some(patch_offset + step);
        let measurements = midline_measurement_refs(
            chunk,
            top_basis,
            observable_basis,
            distance,
            patch_offset,
            final_patch_offset,
        );
        if !measurements.is_empty() {
            selected.push(ChunkMeasurements {
                chunk_index,
                measurements,
            });
        }
        patch_offset += step;
    }
    selected
}

fn midline_measurement_refs(
    chunk: &Chunk,
    top_basis: Basis,
    basis: Basis,
    distance: u32,
    current_patch_offset: IVec2,
    final_patch_offset: Option<IVec2>,
) -> Vec<u32> {
    let target_basis = PauliBasis::from(basis);
    let mut measurements = FxSet::default();
    for op in chunk_ops(chunk) {
        let Op::Measure {
            basis: measured_basis,
            qubits: measured_qubits,
            measurements: measured_ids,
            ..
        } = op
        else {
            continue;
        };
        if *measured_basis != target_basis {
            continue;
        }
        for (&q, &measurement_id) in measured_qubits.iter().zip(measured_ids) {
            if on_midline(q, distance, top_basis, basis, current_patch_offset)
                || final_patch_offset
                    .is_some_and(|offset| on_midline(q, distance, top_basis, basis, offset))
            {
                measurements.insert(measurement_id);
            }
        }
    }

    let mut measurements = measurements.into_iter().collect::<Vec<_>>();
    measurements.sort_unstable();
    measurements
}

fn chunk_ops(chunk: &Chunk) -> &[Op] {
    chunk
        .circuit
        .body(chunk.circuit.entry_body())
        .expect("walking chunks use an entry body")
        .ops()
}

fn on_midline(
    q: IVec2,
    distance: u32,
    top_basis: Basis,
    basis: Basis,
    patch_offset: IVec2,
) -> bool {
    let d = distance as i32;
    let horizontal = basis == top_basis.flip();
    let (along, constant, offset_along, offset_constant) = if horizontal {
        (q.x, q.y, patch_offset.x, patch_offset.y)
    } else {
        (q.y, q.x, patch_offset.y, patch_offset.x)
    };
    constant == d + offset_constant
        && (1 + offset_along..=2 * d - 1 + offset_along).contains(&along)
}

/// Expand a graph-space walking movement into the full production walking run.
///
/// Block offsets are separated by `2 * distance + 2` physical half-cell units,
/// so a one-cell graph movement needs one extra two-step cycle beyond the code
/// distance.
fn walking_steps(movement: IVec2, distance: u32) -> Vec<IVec2> {
    let cycle = decompose_walking_movement(movement);
    let mut steps = Vec::with_capacity(2 * (distance as usize + 1));
    for _ in 0..=distance {
        steps.extend(cycle.iter().copied());
    }
    steps
}

/// Decompose one graph-space walking movement into a two-step diagonal cycle.
///
/// Cardinal movements use two diagonal steps whose perpendicular components
/// cancel. Diagonal movements repeat the same diagonal step twice.
fn decompose_walking_movement(movement: IVec2) -> Vec<IVec2> {
    debug_assert!(
        (-1..=1).contains(&movement.x) && (-1..=1).contains(&movement.y) && movement != IVec2::ZERO,
        "walking movement must be a nonzero unit Chebyshev vector"
    );

    match (movement.x, movement.y) {
        (x, y) if x != 0 && y != 0 => vec![IVec2::new(x, y), IVec2::new(x, y)],
        (x, 0) => vec![IVec2::new(x, 1), IVec2::new(x, -1)],
        (0, y) => vec![IVec2::new(1, y), IVec2::new(-1, y)],
        _ => unreachable!("zero movement rejected above"),
    }
}

/// One freshly built step: its chunk and the state crossing into the next step.
struct StepBuild {
    chunk: Chunk,
    patch: Patch,
    regions: Vec<Region>,
}

fn make_step_chunk(
    old_patch: &Patch,
    old_regions: &[Region],
    step: IVec2,
    top_basis: Basis,
    mode: StepMode,
) -> Result<StepBuild, CompileError> {
    let geometry = StepGeometry::new(step, top_basis);
    let frame = StepFrame::covering(old_patch);
    let new_patch = translate_patch(old_patch, step);
    let old_all = used_coord_set(old_patch, frame);

    let mut circuit = CoordCircuit::new();
    if let StepMode::Init { basis } = mode {
        circuit.do_gate(
            reset_gate(basis),
            data_coord_set(old_patch, frame).sorted_coords(),
        )?;
    }
    let mut regions = reset_and_make_regions(
        &mut circuit,
        old_patch,
        &old_all,
        &geometry,
        old_regions,
        mode,
    )?;
    circuit.tick();

    let center = z_measure_center(old_patch);
    let mut tracking_scratch = RegionTrackingScratch::new(frame);
    let mut layer = CxLayer::new(frame);
    let mut emit_layer =
        |allowed: &CoordSet, layer_center: IVec2, direction: IVec2, aligned: bool| {
            make_cx_layer(&mut layer, allowed, layer_center, direction, aligned);
            track_regions_through_cx(&mut regions, &layer, &mut tracking_scratch);
            circuit.do_gate(GateType::CX, layer.stim_targets())?;
            circuit.tick();
            Ok::<_, CompileError>(())
        };
    for (allowed, layer_center, direction, aligned) in [
        (&old_all, center, geometry.forward, true),
        (&old_all, center, geometry.left, false),
    ] {
        emit_layer(allowed, layer_center, direction, aligned)?;
    }

    let mut old_and_new_bulk = old_all;
    extend_bulk_tile_qubits(&mut old_and_new_bulk, &new_patch);
    for (allowed, layer_center, direction, aligned) in [
        (&old_and_new_bulk, center + step, geometry.right, false),
        (&old_and_new_bulk, center + step, geometry.forward, true),
    ] {
        emit_layer(allowed, layer_center, direction, aligned)?;
    }

    let step_flows = measure_and_make_flows(&mut circuit, &new_patch, &regions, frame, mode)?;
    circuit.tick();

    Ok(StepBuild {
        chunk: Chunk {
            circuit,
            flows: step_flows.flows,
        },
        patch: new_patch,
        regions: step_flows.remaining_regions,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepMode {
    Bulk,
    Init { basis: Basis },
    Meas { basis: Basis },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StepGeometry {
    forward: IVec2,
    left: IVec2,
    right: IVec2,
}

impl StepGeometry {
    fn new(forward: IVec2, top_basis: Basis) -> Self {
        debug_assert!(
            forward.x.abs() == 1 && forward.y.abs() == 1,
            "walking steps are diagonal half-cell moves in bloq coordinates"
        );

        let mut left = IVec2::new(-forward.y, forward.x);
        let mut right = IVec2::new(forward.y, -forward.x);
        // The reference schedule conditionally swaps the perpendicular order
        // for DOWN_RIGHT/UP_LEFT. After converting to bloq's Y-up coordinates,
        // the same convention depends on the patch's top boundary basis.
        let same_diagonal = forward.x == forward.y;
        let flip_left_right = top_basis == Basis::X;
        if same_diagonal == flip_left_right {
            std::mem::swap(&mut left, &mut right);
        }

        Self {
            forward,
            left,
            right,
        }
    }

    fn perpendiculars(self) -> [IVec2; 2] {
        [self.left, self.right]
    }
}

fn translate_patch(patch: &Patch, offset: IVec2) -> Patch {
    Patch::from_sorted_tiles(
        patch
            .tiles()
            .iter()
            .map(|tile| {
                Tile::new(
                    tile.basis(),
                    tile.measure_qubit() + offset,
                    tile.data_slots()
                        .iter()
                        .map(|slot| slot.map(|q| q + offset)),
                )
            })
            .collect(),
    )
}

fn extend_bulk_tile_qubits(qubits: &mut CoordSet, patch: &Patch) {
    for tile in patch.tiles() {
        if tile.data_slots().iter().flatten().count() != 4 {
            continue;
        }
        qubits.insert(tile.measure_qubit());
        for &q in tile.data_slots().iter().flatten() {
            qubits.insert(q);
        }
    }
}

fn z_measure_center(patch: &Patch) -> IVec2 {
    patch
        .z_tiles()
        .next()
        .map(Tile::measure_qubit)
        .expect("surface code patch has at least one Z tile")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegionKind {
    Expanding,
    Contracting,
}

/// Region qubit sets stay plaquette-sized (a measure qubit plus a handful of
/// data qubits), so a linear-scan small vector beats a hash set in both
/// lookup and rebuild cost on the per-step hot path.
type RegionQubits = SmallVec<[IVec2; 6]>;

fn region_insert(qubits: &mut RegionQubits, q: IVec2) {
    if !qubits.contains(&q) {
        qubits.push(q);
    }
}

struct RegionTrackingScratch {
    frame: StepFrame,
    epoch: u32,
    marks: Vec<RegionMarks>,
    qubits: RegionQubits,
}

#[derive(Clone, Copy, Default)]
struct RegionMarks {
    input: u32,
    output: u32,
}

impl RegionTrackingScratch {
    fn new(frame: StepFrame) -> Self {
        Self {
            frame,
            epoch: 0,
            marks: vec![RegionMarks::default(); frame.area()],
            qubits: RegionQubits::new(),
        }
    }

    fn begin_region(&mut self, input: &RegionQubits) {
        self.qubits.clear();
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.marks.fill(RegionMarks::default());
            self.epoch = 1;
        }
        for &q in input {
            self.marks[self.frame.index_in_frame(q)].input = self.epoch;
        }
    }

    #[inline]
    fn contains_input(&self, q: IVec2) -> bool {
        self.marks[self.frame.index_in_frame(q)].input == self.epoch
    }

    #[inline]
    fn insert_output(&mut self, q: IVec2) {
        let index = self.frame.index_in_frame(q);
        if self.marks[index].output == self.epoch {
            return;
        }
        self.marks[index].output = self.epoch;
        self.qubits.push(q);
    }
}

#[derive(Clone)]
struct Region {
    qubits: RegionQubits,
    basis: Basis,
    kind: RegionKind,
    reinclude: Option<IVec2>,
    input: PauliMap,
}

impl Region {
    fn translate(&mut self, offset: IVec2) {
        for q in &mut self.qubits {
            *q += offset;
        }
        if let Some(q) = &mut self.reinclude {
            *q += offset;
        }
        self.input = self.input.translated(offset);
    }
}

/// Dense per-step coordinate frame.
///
/// Covers the old patch bounding box plus a margin wide enough for the
/// translated patch and the `measure + forward (+ perpendicular)` expansion
/// probes a single walking step can introduce.
#[derive(Debug, Clone, Copy)]
struct StepFrame {
    min: IVec2,
    width: i32,
    height: i32,
}

impl StepFrame {
    fn covering(patch: &Patch) -> Self {
        // One diagonal step and the expansion probes reach at most two
        // half-cells beyond the current patch in every direction.
        const MARGIN: i32 = 2;
        let mut min = IVec2::MAX;
        let mut max = IVec2::MIN;
        for tile in patch.tiles() {
            min = min.min(tile.measure_qubit());
            max = max.max(tile.measure_qubit());
            for &slot in tile.data_slots() {
                if let Some(q) = slot {
                    min = min.min(q);
                    max = max.max(q);
                }
            }
        }
        debug_assert!(min.x <= max.x, "step frame requires a non-empty patch");
        min -= IVec2::splat(MARGIN);
        max += IVec2::splat(MARGIN);
        Self {
            min,
            width: max.x - min.x + 1,
            height: max.y - min.y + 1,
        }
    }

    #[inline]
    fn index(self, q: IVec2) -> Option<usize> {
        let d = q - self.min;
        if d.x < 0 || d.x >= self.width || d.y < 0 || d.y >= self.height {
            return None;
        }
        Some((d.y * self.width + d.x) as usize)
    }

    #[inline]
    fn index_in_frame(self, q: IVec2) -> usize {
        let d = q - self.min;
        debug_assert!(d.x >= 0 && d.x < self.width);
        debug_assert!(d.y >= 0 && d.y < self.height);
        (d.y * self.width + d.x) as usize
    }

    #[inline]
    fn coord(self, index: usize) -> IVec2 {
        let index = index as i32;
        self.min + IVec2::new(index % self.width, index / self.width)
    }

    fn area(self) -> usize {
        (self.width as usize) * (self.height as usize)
    }
}

/// Dense coordinate set over a step frame.
///
/// Row-major iteration yields coordinates sorted by `(y, x)`, which is exactly
/// the emission order the circuit builders sort into.
#[derive(Debug, Clone)]
struct CoordSet {
    frame: StepFrame,
    cells: Vec<bool>,
}

impl CoordSet {
    fn new(frame: StepFrame) -> Self {
        Self {
            frame,
            cells: vec![false; frame.area()],
        }
    }

    #[inline]
    fn contains(&self, q: IVec2) -> bool {
        self.frame.index(q).is_some_and(|index| self.cells[index])
    }

    #[inline]
    fn insert(&mut self, q: IVec2) {
        let index = self
            .frame
            .index(q)
            .expect("walking coordinates stay within the step frame");
        self.cells[index] = true;
    }

    fn iter_sorted(&self) -> impl Iterator<Item = IVec2> + '_ {
        self.cells
            .iter()
            .enumerate()
            .filter(|&(_, &set)| set)
            .map(|(index, _)| self.frame.coord(index))
    }

    fn sorted_coords(&self) -> Vec<IVec2> {
        self.iter_sorted().collect()
    }

    fn union_with(&mut self, other: &CoordSet) {
        debug_assert_eq!(self.cells.len(), other.cells.len());
        for (cell, &other_cell) in self.cells.iter_mut().zip(&other.cells) {
            *cell |= other_cell;
        }
    }
}

fn used_coord_set(patch: &Patch, frame: StepFrame) -> CoordSet {
    let mut set = CoordSet::new(frame);
    for tile in patch.tiles() {
        set.insert(tile.measure_qubit());
        for &slot in tile.data_slots() {
            if let Some(q) = slot {
                set.insert(q);
            }
        }
    }
    set
}

fn data_coord_set(patch: &Patch, frame: StepFrame) -> CoordSet {
    let mut set = CoordSet::new(frame);
    for tile in patch.tiles() {
        for &slot in tile.data_slots() {
            if let Some(q) = slot {
                set.insert(q);
            }
        }
    }
    set
}

/// Sentinel for "no partner" in [`CxCell`]; walking coordinates stay well
/// inside `i32` range, so this value can never collide with a real qubit.
const NO_QUBIT: IVec2 = IVec2::splat(i32::MIN);

/// Per-coordinate CX partners. A qubit can simultaneously be the control of
/// one pair and the target of another within the same layer, so both roles
/// are tracked.
#[derive(Debug, Clone, Copy)]
struct CxCell {
    /// Partner target when this coordinate is a control.
    target: IVec2,
    /// Partner control when this coordinate is a target.
    control: IVec2,
}

const EMPTY_CX_CELL: CxCell = CxCell {
    target: NO_QUBIT,
    control: NO_QUBIT,
};

/// One CX layer, with dense per-coordinate partner lookup for region tracking
/// and a `(control, target)` pair list for Stim emission.
struct CxLayer {
    frame: StepFrame,
    cells: Vec<CxCell>,
    /// Pairs sorted by `(target.y, target.x)`.
    pairs: Vec<(IVec2, IVec2)>,
    touched: Vec<usize>,
}

impl CxLayer {
    fn new(frame: StepFrame) -> Self {
        Self {
            frame,
            cells: vec![EMPTY_CX_CELL; frame.area()],
            pairs: Vec::with_capacity(frame.area() / 4),
            touched: Vec::with_capacity(frame.area() / 2),
        }
    }

    fn reset(&mut self, frame: StepFrame) {
        for index in self.touched.drain(..) {
            self.cells[index] = EMPTY_CX_CELL;
        }
        self.frame = frame;
        self.pairs.clear();
        if self.cells.len() != frame.area() {
            self.cells.resize(frame.area(), EMPTY_CX_CELL);
        }
    }

    fn add_pair(&mut self, control: IVec2, target: IVec2) {
        let control_index = self
            .frame
            .index(control)
            .expect("CX controls stay within the step frame");
        let target_index = self
            .frame
            .index(target)
            .expect("CX targets stay within the step frame");
        debug_assert_eq!(self.cells[control_index].target, NO_QUBIT);
        debug_assert_eq!(self.cells[target_index].control, NO_QUBIT);
        self.cells[control_index].target = target;
        self.cells[target_index].control = control;
        self.touched.push(control_index);
        self.touched.push(target_index);
    }

    #[inline]
    fn cell(&self, q: IVec2) -> CxCell {
        self.cells[self.frame.index_in_frame(q)]
    }

    fn stim_targets(&self) -> impl Iterator<Item = IVec2> + '_ {
        self.pairs
            .iter()
            .flat_map(|&(control, target)| [target, control])
    }
}

fn initial_contracting_regions(patch: &Patch) -> Vec<Region> {
    patch
        .tiles()
        .iter()
        .map(|tile| {
            let mut qubits = RegionQubits::new();
            for &q in tile.data_slots().iter().flatten() {
                region_insert(&mut qubits, q);
            }
            Region {
                qubits,
                basis: tile.basis(),
                kind: RegionKind::Contracting,
                reinclude: Some(tile.measure_qubit()),
                input: tile.pauli_map(),
            }
        })
        .collect()
}

fn reset_and_make_regions(
    circuit: &mut CoordCircuit,
    patch: &Patch,
    old_all: &CoordSet,
    geometry: &StepGeometry,
    input_regions: &[Region],
    mode: StepMode,
) -> Result<Vec<Region>, CompileError> {
    let frame = old_all.frame;
    let mut resets = BasisCoordSets::new(frame);
    let mut regions = Vec::with_capacity(patch.len().saturating_mul(2));
    let reincluded_regions = reincluded_region_indices(frame, input_regions)?;

    for tile in patch.tiles() {
        let basis = tile.basis();
        let measure = tile.measure_qubit();
        let continuing_region = match mode {
            StepMode::Init { basis } if basis != tile.basis() => None,
            StepMode::Bulk | StepMode::Init { .. } | StepMode::Meas { .. } => reincluded_regions
                .get(measure)
                .map(|index| &input_regions[index]),
        };
        let mut contracting_qubits = RegionQubits::new();
        if let Some(region) = continuing_region {
            contracting_qubits.extend(region.qubits.iter().copied());
        }
        let mut expanding_qubits = RegionQubits::new();
        expanding_qubits.push(measure);
        let mut reinclude = None;

        resets.get_mut(basis).insert(measure);

        let data_slots = tile.data_slots();
        let data_count = data_slots.iter().flatten().count();
        let touches_data = |q| data_slots.iter().flatten().any(|&data| data == q);
        let is_boundary = data_count == 2;
        let is_trailing_boundary = is_boundary && touches_data(measure + geometry.forward);

        if is_boundary && !is_trailing_boundary {
            let back = geometry
                .perpendiculars()
                .into_iter()
                .find(|&pd| touches_data(measure + pd))
                .expect("leading boundary tile touches one perpendicular old data qubit");
            let extra = measure + geometry.forward + back;
            reinclude = Some(measure + geometry.forward);
            region_insert(&mut contracting_qubits, measure);
            region_insert(&mut expanding_qubits, extra);
            resets.get_mut(basis).insert(extra);
        } else if !is_boundary {
            for pd in geometry.perpendiculars() {
                let q = measure + geometry.forward + pd;
                if !old_all.contains(q) {
                    region_insert(&mut expanding_qubits, q);
                    resets.get_mut(basis).insert(q);
                }
            }
            region_insert(&mut contracting_qubits, measure);
        } else {
            region_insert(&mut contracting_qubits, measure);
        }

        regions.push(Region {
            qubits: expanding_qubits,
            basis,
            kind: RegionKind::Expanding,
            reinclude,
            input: PauliMap::empty(),
        });
        if let Some(region) = continuing_region {
            let input = match mode {
                StepMode::Init { basis } if basis == region.basis => PauliMap::empty(),
                StepMode::Bulk | StepMode::Init { .. } | StepMode::Meas { .. } => {
                    region.input.clone()
                }
            };
            regions.push(Region {
                qubits: contracting_qubits,
                basis: region.basis,
                kind: RegionKind::Contracting,
                reinclude: None,
                input,
            });
        }
    }

    emit_resets(circuit, &resets)?;
    Ok(regions)
}

/// Dense map from reinclude qubit to region index, sentinel `u32::MAX`.
struct ReincludeIndices {
    frame: StepFrame,
    cells: Vec<u32>,
}

impl ReincludeIndices {
    fn get(&self, q: IVec2) -> Option<usize> {
        let index = self.frame.index(q)?;
        (self.cells[index] != u32::MAX).then_some(self.cells[index] as usize)
    }
}

fn reincluded_region_indices(
    frame: StepFrame,
    regions: &[Region],
) -> Result<ReincludeIndices, CompileError> {
    let mut cells = vec![u32::MAX; frame.area()];
    for (region_index, region) in regions.iter().enumerate() {
        let Some(qubit) = region.reinclude else {
            continue;
        };
        let cell = frame
            .index(qubit)
            .map(|index| &mut cells[index])
            .ok_or_else(|| walking_error(WalkError::ReincludeOutsideFrame { qubit }))?;
        if *cell != u32::MAX {
            return Err(walking_error(WalkError::DuplicateReinclude { qubit }));
        }
        *cell = region_index as u32;
    }
    Ok(ReincludeIndices { frame, cells })
}

/// Rewrite each region's qubit set in place; `scratch` holds the new set
/// while the old one is still being read, then the two are swapped. Updating
/// in place keeps the regions' other fields (basis, input map, ...) untouched
/// instead of moving every `Region` through an iterator pipeline, which is
/// what used to dominate walking-step construction.
fn track_regions_through_cx(
    regions: &mut [Region],
    layer: &CxLayer,
    scratch: &mut RegionTrackingScratch,
) {
    for region in regions {
        scratch.begin_region(&region.qubits);
        for &q in &region.qubits {
            track_region_qubit(q, region.basis, layer, scratch);
        }
        std::mem::swap(&mut region.qubits, &mut scratch.qubits);
    }
}

fn track_region_qubit(
    q: IVec2,
    basis: Basis,
    layer: &CxLayer,
    scratch: &mut RegionTrackingScratch,
) {
    let cell = layer.cell(q);
    // Z regions propagate along control -> target, X regions the reverse.
    let (sink, source) = match basis {
        Basis::Z => (cell.target, cell.control),
        Basis::X => (cell.control, cell.target),
    };

    if sink != NO_QUBIT && !scratch.contains_input(sink) {
        scratch.insert_output(q);
        scratch.insert_output(sink);
        return;
    }
    if source != NO_QUBIT && scratch.contains_input(source) {
        scratch.insert_output(source);
        return;
    }
    scratch.insert_output(q);
}

fn measure_and_make_flows(
    circuit: &mut CoordCircuit,
    output_patch: &Patch,
    regions: &[Region],
    frame: StepFrame,
    mode: StepMode,
) -> Result<StepFlows, CompileError> {
    let keep_qubits = used_coord_set(output_patch, frame);
    let mut output_measure_qubits = CoordSet::new(frame);
    for tile in output_patch.tiles() {
        output_measure_qubits.insert(tile.measure_qubit());
    }
    let final_data_qubits = match mode {
        StepMode::Meas { .. } => data_coord_set(output_patch, frame),
        StepMode::Bulk | StepMode::Init { .. } => CoordSet::new(frame),
    };
    let mut measurements = BasisCoordSets::new(frame);
    if let StepMode::Meas { basis } = mode {
        measurements.get_mut(basis).union_with(&final_data_qubits);
    }
    // Each input region survives as at most one contracting remnant; reserving
    // avoids growth copies of these large structs (visible in heap profiles).
    let mut remaining_regions = Vec::with_capacity(regions.len());

    for region in regions {
        match region.kind {
            RegionKind::Contracting => {
                for &q in &region.qubits {
                    if keep_qubits.contains(q) && !output_measure_qubits.contains(q) {
                        return Err(walking_error(WalkError::ContractingKeptUnmeasuredQubit {
                            basis: region.basis,
                            qubit: q,
                        }));
                    }
                    measurements.get_mut(region.basis).insert(q);
                }
            }
            RegionKind::Expanding => {
                let mut reinclude = region.reinclude;
                let mut remaining_qubits = RegionQubits::new();
                for &q in &region.qubits {
                    let is_output_measure = output_measure_qubits.contains(q);
                    let measure_q = is_output_measure || !keep_qubits.contains(q);
                    if measure_q {
                        measurements.get_mut(region.basis).insert(q);
                    } else {
                        region_insert(&mut remaining_qubits, q);
                    }

                    if is_output_measure {
                        if let Some(previous) = reinclude
                            && previous != q
                        {
                            return Err(walking_error(
                                WalkError::ExpandingMultipleOutputMeasures {
                                    basis: region.basis,
                                    first: previous,
                                    second: q,
                                },
                            ));
                        }
                        reinclude = Some(q);
                    }
                }
                let output = region_pauli_map(remaining_qubits.iter().copied(), region.basis);

                match mode {
                    StepMode::Meas { basis } if region.basis == basis => {
                        let (continuing_output, measured_data) =
                            collapse_matching_output(&output, region.basis, &final_data_qubits)?;
                        if !continuing_output.is_empty() {
                            return Err(walking_error(WalkError::FinalMeasurementLeftOutput {
                                basis,
                                output: continuing_output,
                            }));
                        }
                        for &q in &measured_data {
                            measurements.get_mut(basis).insert(q);
                        }
                    }
                    StepMode::Meas { .. } => {}
                    StepMode::Bulk | StepMode::Init { .. } => {
                        remaining_regions.push(Region {
                            qubits: remaining_qubits,
                            basis: region.basis,
                            kind: RegionKind::Contracting,
                            reinclude,
                            input: output,
                        });
                    }
                }
            }
        }
    }

    let measurement_ids = emit_measurements(circuit, &measurements);
    let flows = flows_from_regions(
        regions,
        &remaining_regions,
        &keep_qubits,
        &output_measure_qubits,
        &final_data_qubits,
        mode,
        &measurement_ids,
    )?;
    Ok(StepFlows {
        flows,
        remaining_regions,
    })
}

/// One step's measurement pass: the flows it emits and the regions continuing
/// into the next step.
struct StepFlows {
    flows: Vec<Flow>,
    remaining_regions: Vec<Region>,
}

fn flows_from_regions(
    regions: &[Region],
    remaining_regions: &[Region],
    keep_qubits: &CoordSet,
    output_measure_qubits: &CoordSet,
    final_data_qubits: &CoordSet,
    mode: StepMode,
    measurement_ids: &MeasurementIds,
) -> Result<Vec<Flow>, CompileError> {
    let mut remaining_regions = remaining_regions.iter();
    let mut flows = Vec::with_capacity(regions.len());
    for region in regions {
        let center = region_center(&region.qubits);
        match region.kind {
            RegionKind::Contracting => {
                flows.push(flow_from_qubits(
                    region.input.clone(),
                    PauliMap::empty(),
                    region.qubits.iter().copied(),
                    region.qubits.len(),
                    center,
                    measurement_ids,
                )?);
            }
            RegionKind::Expanding => match mode {
                StepMode::Meas { basis } if region.basis == basis => {
                    let remaining =
                        region.qubits.iter().copied().filter(|&q| {
                            keep_qubits.contains(q) && !output_measure_qubits.contains(q)
                        });
                    let output = region_pauli_map(remaining, region.basis);
                    let (continuing_output, measured_data) =
                        collapse_matching_output(&output, region.basis, final_data_qubits)?;
                    debug_assert!(continuing_output.is_empty());
                    let measured_qubits = region
                        .qubits
                        .iter()
                        .copied()
                        .filter(|&q| output_measure_qubits.contains(q) || !keep_qubits.contains(q))
                        .chain(measured_data);
                    flows.push(flow_from_qubits(
                        region.input.clone(),
                        PauliMap::empty(),
                        measured_qubits,
                        region.qubits.len(),
                        center,
                        measurement_ids,
                    )?);
                }
                StepMode::Meas { .. } => {}
                StepMode::Bulk | StepMode::Init { .. } => {
                    let remaining = remaining_regions
                        .next()
                        .expect("remaining region exists for continuing expanding region");
                    let measured_qubits =
                        region.qubits.iter().copied().filter(|&q| {
                            output_measure_qubits.contains(q) || !keep_qubits.contains(q)
                        });
                    flows.push(flow_from_qubits(
                        region.input.clone(),
                        remaining.input.clone(),
                        measured_qubits,
                        region.qubits.len(),
                        center,
                        measurement_ids,
                    )?);
                }
            },
        }
    }
    debug_assert!(remaining_regions.next().is_none());
    Ok(flows)
}

fn flow_from_qubits(
    start: PauliMap,
    end: PauliMap,
    qubits: impl IntoIterator<Item = IVec2>,
    capacity: usize,
    center: Option<IVec2>,
    measurement_ids: &MeasurementIds,
) -> Result<Flow, CompileError> {
    let mut measured_qubits = Vec::with_capacity(capacity);
    measured_qubits.extend(qubits);
    measured_qubits.sort_unstable_by_key(|q| (q.y, q.x));
    let mut measurements = FlowMeasurements::with_capacity(measured_qubits.len());
    for q in measured_qubits {
        let id = measurement_ids
            .get(q)
            .ok_or_else(|| walking_error(WalkError::FlowReferencesUnmeasuredQubit { qubit: q }))?;
        measurements.push(id);
    }
    Ok(Flow {
        measurements,
        ..Flow::new(start, end)
    }
    .with_center(center))
}

fn collapse_matching_output(
    output: &PauliMap,
    basis: Basis,
    measured_data: &CoordSet,
) -> Result<(PauliMap, Vec<IVec2>), CompileError> {
    let pauli = Pauli::from(basis);
    let mut continuing = PauliMap::empty();
    let mut measured = Vec::new();

    for (&coord, &found_pauli) in output {
        if found_pauli != pauli {
            return Err(walking_error(WalkError::FinalFlowMismatchedPauli {
                basis,
                pauli: found_pauli,
                coord,
            }));
        }
        if measured_data.contains(coord) {
            measured.push(coord);
        } else {
            continuing.insert(coord, found_pauli);
        }
    }

    Ok((continuing, measured))
}

fn region_pauli_map(qubits: impl IntoIterator<Item = IVec2>, basis: Basis) -> PauliMap {
    let pauli = Pauli::from(basis);
    PauliMap::from_unique_entries(qubits.into_iter().map(|q| (q, pauli)))
}

/// A region's mean coordinate, truncated — the flow's detector-coordinate
/// annotation. Truncation makes it only approximately translation-covariant;
/// translated steps shift it as-is (see [`make_step_chunks`]).
fn region_center(qubits: &RegionQubits) -> Option<IVec2> {
    (!qubits.is_empty()).then(|| {
        let sum = qubits.iter().fold(IVec2::ZERO, |acc, &q| acc + q);
        sum / qubits.len() as i32
    })
}

fn walking_error(error: WalkError) -> CompileError {
    CompileError::WalkingConstruction(error)
}

/// X/Z reset and measurement coordinate sets for one step.
struct BasisCoordSets {
    x: CoordSet,
    z: CoordSet,
}

impl BasisCoordSets {
    fn new(frame: StepFrame) -> Self {
        Self {
            x: CoordSet::new(frame),
            z: CoordSet::new(frame),
        }
    }

    fn get(&self, basis: Basis) -> &CoordSet {
        match basis {
            Basis::X => &self.x,
            Basis::Z => &self.z,
        }
    }

    fn get_mut(&mut self, basis: Basis) -> &mut CoordSet {
        match basis {
            Basis::X => &mut self.x,
            Basis::Z => &mut self.z,
        }
    }
}

/// Dense map from measured qubit to its measurement id, sentinel `u32::MAX`.
struct MeasurementIds {
    frame: StepFrame,
    ids: Vec<u32>,
}

impl MeasurementIds {
    fn get(&self, q: IVec2) -> Option<u32> {
        let index = self.frame.index(q)?;
        (self.ids[index] != u32::MAX).then_some(self.ids[index])
    }
}

fn emit_resets(circuit: &mut CoordCircuit, resets: &BasisCoordSets) -> Result<(), CompileError> {
    for basis in [Basis::Z, Basis::X] {
        circuit.do_gate(reset_gate(basis), resets.get(basis).sorted_coords())?;
    }
    Ok(())
}

fn emit_measurements(circuit: &mut CoordCircuit, measurements: &BasisCoordSets) -> MeasurementIds {
    let frame = measurements.x.frame;
    let mut measurement_ids = MeasurementIds {
        frame,
        ids: vec![u32::MAX; frame.area()],
    };
    for basis in [Basis::Z, Basis::X] {
        let qubits = measurements.get(basis).sorted_coords();
        let ids = circuit.measure(PauliBasis::from(basis), qubits.iter().copied());
        for (qubit, id) in qubits.into_iter().zip(ids) {
            let index = frame.index(qubit).expect("measured qubits are in frame");
            measurement_ids.ids[index] = id;
        }
    }
    measurement_ids
}

fn make_cx_layer(
    layer: &mut CxLayer,
    allowed: &CoordSet,
    center: IVec2,
    direction: IVec2,
    aligned: bool,
) {
    layer.reset(allowed.frame);
    for q in allowed.iter_sorted() {
        let delta = q - center;
        if delta.x % 2 != 0 || delta.y % 2 != 0 {
            continue;
        }

        let odd_subgrid = delta.x.rem_euclid(4) != delta.y.rem_euclid(4);
        let layer_direction = if odd_subgrid && !aligned {
            -direction
        } else {
            direction
        };
        let other = q + layer_direction;
        if !allowed.contains(other) {
            continue;
        }

        if odd_subgrid {
            layer.add_pair(other, q);
        } else {
            layer.add_pair(q, other);
        }
    }

    for (index, cell) in layer.cells.iter().enumerate() {
        if cell.control != NO_QUBIT {
            layer.pairs.push((cell.control, layer.frame.coord(index)));
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn movement_decomposition_covers_cardinal_and_diagonal_moves() {
        assert_eq!(
            decompose_walking_movement(IVec2::new(1, 1)),
            vec![IVec2::new(1, 1), IVec2::new(1, 1)]
        );
        assert_eq!(
            decompose_walking_movement(IVec2::X),
            vec![IVec2::new(1, 1), IVec2::new(1, -1)]
        );
        assert_eq!(
            decompose_walking_movement(IVec2::Y),
            vec![IVec2::new(1, 1), IVec2::new(-1, 1)]
        );
    }

    /// Number of chunks the midline observable spans, recovered from its
    /// gateway entry (every walking chunk has a midline measurement, so the
    /// entry's chunk indices cover the whole schedule).
    fn entry_chunk_span(entry: &GatewayEntry) -> usize {
        entry
            .measurements
            .iter()
            .map(|chunk_measurements| chunk_measurements.chunk_index)
            .max()
            .map_or(0, |index| index + 1)
    }

    /// Assert the gateway entry spans contiguous chunks and every measurement is
    /// a valid template measurement id (the gateway is now in template space).
    fn assert_gateway_measurements_resolve(template: &LoweringTemplate, entry: &GatewayEntry) {
        assert_eq!(entry.measurements.len(), entry_chunk_span(entry));
        let num_measurements = template.program_template.circuit.num_measurements();
        for chunk_measurements in &entry.measurements {
            assert!(!chunk_measurements.measurements.is_empty());
            for &measurement_id in &chunk_measurements.measurements {
                assert!(measurement_id < num_measurements);
            }
        }
    }

    #[rstest]
    fn production_walking_connectivity_controls_boundary_chunks(
        #[values(
            Connectivity::ISOLATED,
            Connectivity::ISOLATED.with_pipe(Direction::ZMINUS),
            Connectivity::ISOLATED.with_pipe(Direction::ZPLUS),
            Connectivity::ISOLATED.with_pipe(Direction::ZMINUS).with_pipe(Direction::ZPLUS)
        )]
        connectivity: Connectivity,
    ) {
        let kind = WalkingKind::new(bloq_graph::WalkingBoundaryKind::ZXZ, IVec2::X)
            .expect("valid walking kind");
        let distance = 5;
        let template = compile_walking(kind, connectivity, distance).expect("compile walking");
        let expected_chunks = 2 * (distance as usize + 1);

        let expected_key =
            LocalStabilizer::new(Pauli::Z, walking_gateway_connectivity(connectivity));
        assert!(template.observable_gateway.contains_key(&expected_key));
        assert_eq!(
            entry_chunk_span(&template.observable_gateway[&expected_key]),
            expected_chunks
        );
    }

    #[test]
    fn compile_walking_gateway_uses_measurement_only_midline_observable() {
        let kind = WalkingKind::new(bloq_graph::WalkingBoundaryKind::ZXZ, IVec2::X)
            .expect("valid walking kind");
        let template = compile_walking(kind, Connectivity::ISOLATED, 5).expect("compile walking");
        let key = LocalStabilizer::new(Pauli::Z, Connectivity::ISOLATED);
        let entry = &template.observable_gateway[&key];
        let last_chunk = entry_chunk_span(entry) - 1;

        assert_eq!(entry_chunk_span(entry), 12);
        assert_eq!(entry.measurements[0].chunk_index, 0);
        assert_eq!(entry.measurements[0].measurements.len(), 2);
        assert_eq!(entry.measurements[last_chunk].chunk_index, last_chunk);
        assert_gateway_measurements_resolve(&template, entry);
    }

    #[rstest]
    fn compile_walking_gateway_resolves_for_all_boundaries_and_movements(
        #[values(
            bloq_graph::WalkingBoundaryKind::ZXZ,
            bloq_graph::WalkingBoundaryKind::ZXX,
            bloq_graph::WalkingBoundaryKind::XZZ,
            bloq_graph::WalkingBoundaryKind::XZX
        )]
        boundary: bloq_graph::WalkingBoundaryKind,
        #[values(
            IVec2::X,
            IVec2::NEG_X,
            IVec2::Y,
            IVec2::NEG_Y,
            IVec2::new(1, 1),
            IVec2::new(1, -1),
            IVec2::new(-1, 1),
            IVec2::new(-1, -1)
        )]
        movement: IVec2,
    ) {
        let kind = WalkingKind::new(boundary, movement).expect("valid walking kind");
        let template = compile_walking(kind, Connectivity::ISOLATED, 5).expect("compile walking");
        let key = LocalStabilizer::new(Pauli::from(boundary.z()), Connectivity::ISOLATED);
        let entry = template
            .observable_gateway
            .lookup(key)
            .expect("walking observable gateway entry");

        assert_eq!(entry_chunk_span(&entry), 12);
        assert_gateway_measurements_resolve(&template, &entry);
    }
}
