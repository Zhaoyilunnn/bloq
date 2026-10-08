//! Escape template constructor: the lattice-surgery half of a T block.
//!
//! Ports the msc-ls ZXZ escape stage (`lattice_surgery_generator_zxz`,
//! STE:1325–1692, composed by LSCG:349–476): three merge rounds interleaving
//! the Steane superdense cycles with the surgery-seam stabilizers and the
//! intermediate patch's surface rounds; destructive color-code readout; the
//! top-row stabilizer recovery with the `x_ab` history splice; downward code
//! expansion to `d`; and `d_color = 3` stabilization rounds.
//!
//! ## Chunk decomposition
//!
//! [`escape_chunks`] exposes five `Single` chunks for flow/distance tests:
//! `[merge, E1, S1, S2, S3]`. The persisted template compacts the identical
//! `S1..S3` tail into `REPEAT 3`. Decoder latency is represented by a separate
//! memory-padding node after the escape; it never rewrites this template.
//!
//! The whole 18-tick merge stage is **one chunk**: every Steane superdense
//! cycle crosses the msc-ls round boundaries (a cycle spans ~10 ticks), so
//! per-round chunks would need artificial ancilla-entangled interface
//! stabilizers at each cut. One chunk keeps every post-selected msc-ls parity
//! a plain chunk-local flow, with only the six cultivation faces entering and
//! the intermediate-patch stabilizers (plus `X_AB`) leaving.
//!
//! `E1` is the recovery/expansion round and `S1..S3` the stabilization
//! rounds, all plain `make_surface_code_chunk` products over the final patch
//! (flat `Single`s, not a `Loop`: only 3 rounds, and the distance harness
//! needs top-level detectors).
//!
//! ## Restart parities
//!
//! msc-ls emits each post-selected parity as one single-measurement detector,
//! deterministic given everything upstream is pinned. Chunk-local flows must
//! be deterministic *within the chunk*, so consecutive same-cycle
//! measurements pair up (`[m_r1 ⊕ m_r2]` instead of msc's `[m_r2]`); the
//! restart condition ("any parity odd → restart") depends only on the span of
//! the parity set, which matches msc's — the equivalence test must
//! compare spans, not listings.
//!
//! ## Fixed-bulk convention
//!
//! Surface code stabilizer bases are the base-frame checkerboard colors
//! carried through the variant transform (equal to
//! `checkerboard_basis(mapped position)` by the D₄ parity;
//! pinned by test). The msc-ls construction *logic* (which plaquettes are
//! weight-2, slot order, seam interleaving) is what gets ported; gate bases
//! thread the dual helpers, so the X-type-seam family is the H-conjugate of
//! the base circuit.

use bloq_circuit::{
    Chunk, ChunkOrLoop, CoordCircuit, Flow, FlowMarker, GateType, PauliBasis, PauliMap,
};
use bloq_graph::{Basis, Direction, Pauli};
use glam::IVec2;

// The Steane geometry tables live in `t/mod.rs`; the escape consumes the
// qubits at their final (post-syndrome-round) positions.
use super::{FACES, MscQubit, STEANE_FINAL as STEANE, SurgeryLayout};
use crate::CompileError;
use crate::block::LoweringTemplate;
use crate::block::fixed_bulk::utils::{make_normal_surface_code_patch, make_surface_code_chunk};
use crate::block::gateway::{ChunkMeasurements, GatewayEntry, LocalStabilizer, ObservableGateway};
use crate::block::patch::Patch;
use crate::signature::Connectivity;

// ==========================================================================
// msc-ls coordinates (the zxz generator's local constants)
// ==========================================================================

// Chunk positions in the `[merge, E1, S1, S2, S3]` schedule built by
// [`escape_chunks`].
const MERGE_CHUNK: usize = 0;
const E1_CHUNK: usize = 1;

const STEANE_2_: MscQubit = (5, 11);
const SURFACE_A: MscQubit = (1, 17);
const SURFACE_B: MscQubit = (3, 17);
const A_1A_L: MscQubit = (0, 16);
const A_1A_R: MscQubit = (2, 16);
/// `A_AB` reuses the right seam ancilla (STE:1343).
const A_AB: MscQubit = A_1A_R;
const A_0145_4: MscQubit = (1, 13);
const A_0145_015: MscQubit = (2, 14);
const A_0235_035: MscQubit = (4, 14);
const A_0235_2: MscQubit = (5, 13);
const A_0246_46: MscQubit = (3, 11);
const A_0246_02: MscQubit = (4, 12);

/// msc-ls surface-patch origin (LSCG:78-79).
const SURFACE_OFFSET: MscQubit = (1, 17);

// ==========================================================================
// Public constructors
// ==========================================================================

/// Build the escape template's chunks and observable gateway: `[merge, E1,
/// S1, S2, S3]`, all `Single`.
fn escape_chunks(
    layout: &SurgeryLayout,
) -> Result<(Vec<ChunkOrLoop>, ObservableGateway), CompileError> {
    let (merge, handles) = merge_chunk(layout)?;
    let final_patch = make_normal_surface_code_patch(layout.distance, layout.top_basis());
    let init = expansion_init(layout);
    let mut e1 = make_surface_code_chunk(&final_patch, (!init.is_empty()).then_some(&init), None)?;
    discard_orphan_resplit_consumers(layout, &mut e1);
    let gateway = build_gateway(layout, &handles, &e1, &final_patch);

    // `S1..S3` are the same plain round over the same patch, so it is built
    // once and cloned (`escape_template` folds the tail into a `REPEAT 3`).
    let stabilization = make_surface_code_chunk(&final_patch, None, None)?;
    let mut chunks = vec![
        ChunkOrLoop::Single(Box::new(merge)),
        ChunkOrLoop::Single(Box::new(e1)),
    ];
    chunks.extend(
        std::iter::repeat_n(stabilization, 3).map(|round| ChunkOrLoop::Single(Box::new(round))),
    );
    Ok((chunks, gateway))
}

/// The escape template: standalone composition of [`escape_chunks`]. Its
/// boundary flows are the six start-open (restart-flagged) Steane chains
/// toward the cultivation template and the end-open standard-face creators.
pub(crate) fn escape_template(layout: &SurgeryLayout) -> Result<LoweringTemplate, CompileError> {
    let (mut chunks, gateway) = escape_chunks(layout)?;
    let mut stabilization = chunks.split_off(2).into_iter();
    let ChunkOrLoop::Single(round) = stabilization
        .next()
        .expect("escape has a stabilization tail")
    else {
        unreachable!("escape stabilization rounds are flat chunks")
    };
    debug_assert_eq!(stabilization.count(), 2);
    chunks.push(ChunkOrLoop::Loop {
        body: vec![*round],
        repetitions: 3,
    });
    LoweringTemplate::from_chunks(chunks, gateway)
}

// ==========================================================================
// Geometry
// ==========================================================================

/// Whether the msc-frame surface cell `(i, j)` (row, column) belongs to the
/// intermediate patch (msc anchors it at its top-left corner, LSCG:314).
fn in_intermediate(i: i32, j: i32, d_int: i32) -> bool {
    i < d_int && j < d_int
}

/// Downward-expansion data init (LSCG:159–183): new data reset X on and below
/// the `i >= j` diagonal, Z above it; empty when `d_int == d`. Keys are final
/// block-frame coordinates, values dual-aware bases.
fn expansion_init(layout: &SurgeryLayout) -> crate::FxMap<IVec2, Basis> {
    let d = layout.distance as i32;
    let d_int = layout.intermediate_distance() as i32;
    let mut init = crate::FxMap::default();
    if d_int == d {
        return init;
    }
    for i in 0..d {
        for j in 0..d {
            if in_intermediate(i, j, d_int) {
                continue;
            }
            let base = if i >= j { Basis::X } else { Basis::Z };
            let msc = (SURFACE_OFFSET.0 + 2 * j, SURFACE_OFFSET.1 + 2 * i);
            init.insert(layout.map_msc(msc), layout.basis(base));
        }
    }
    init
}

/// Seam-side X two-weights deleted during the merge (LSCG:402–407), as final
/// block-frame ancilla positions. The first entry is the `x_ab`-splice tile
/// (msc `(2, 16)` = `A_AB`); the rest are re-split with no history.
fn deleted_seam_two_weights(layout: &SurgeryLayout) -> Vec<IVec2> {
    let d_int = layout.intermediate_distance() as i32;
    (0..d_int)
        .filter(|j| j % 2 == 0 && *j < d_int - 1)
        .map(|j| layout.map_msc((SURFACE_OFFSET.0 + 2 * j + 1, SURFACE_OFFSET.1 - 1)))
        .collect()
}

/// Drop E1's consumer flows for the recovered two-weights that re-split with
/// no measurement history (all deleted seam-side tiles except the
/// `x_ab`-splice one): their first post-merge outcome is nondeterministic
/// (msc recovers them with `already_satisfied = False`), and an unmatched
/// consumer would otherwise pollute the template's boundary residual.
fn discard_orphan_resplit_consumers(layout: &SurgeryLayout, e1: &mut Chunk) {
    let orphans = &deleted_seam_two_weights(layout)[1..];
    e1.flows.retain(|flow| {
        !(flow.end.is_empty() && flow.center.is_some_and(|center| orphans.contains(&center)))
    });
}

// ==========================================================================
// Surface-round driver
// ==========================================================================

/// One repeatedly-measured surface stabilizer during the merge: the msc-ls
/// `Surface{X,Z}SyndromeMeasurement` action tables (`surface_code.py:23–249`)
/// normalized to reset at slot 0 and measure at slot 5 (CX slots keep their
/// msc positions, so the neighbor-conflict structure is untouched; the
/// ancilla idles across the shifts).
struct DriverTile {
    ancilla: MscQubit,
    /// Base-frame stabilizer basis (== the checkerboard color after mapping).
    basis: Basis,
    /// Data coupling per CX slot 1..=4, msc frame.
    slots: [Option<MscQubit>; 4],
    /// Surgery-seam stabilizer: round comparisons post-selected, no creator.
    seam: bool,
    /// Measurement key per round.
    rounds: Vec<MeasKey>,
}

impl DriverTile {
    fn new(ancilla: MscQubit, basis: Basis, slots: [Option<MscQubit>; 4]) -> Self {
        Self {
            ancilla,
            basis,
            slots,
            seam: false,
            rounds: Vec::new(),
        }
    }

    fn four_weight(ancilla: MscQubit, basis: Basis) -> Self {
        let (x, y) = ancilla;
        let (lt, lb, rt, rb) = (
            (x - 1, y - 1),
            (x - 1, y + 1),
            (x + 1, y - 1),
            (x + 1, y + 1),
        );
        let slots = match basis {
            Basis::Z => [Some(lt), Some(lb), Some(rt), Some(rb)],
            Basis::X => [Some(lt), Some(rt), Some(lb), Some(rb)],
        };
        Self::new(ancilla, basis, slots)
    }

    /// The two-weight patterns the merge actually runs (the seam-side X
    /// `TWO_WEIGHT_DOWN`s are deleted; E1's `make_surface_code_chunk`
    /// re-creates them natively).
    fn two_weight(ancilla: MscQubit, basis: Basis, pattern: TwoWeight) -> Self {
        let (x, y) = ancilla;
        let (lt, lb, rt, rb) = (
            (x - 1, y - 1),
            (x - 1, y + 1),
            (x + 1, y - 1),
            (x + 1, y + 1),
        );
        let slots = match (basis, pattern) {
            (Basis::Z, TwoWeight::Down) => [None, Some(lb), None, Some(rb)],
            (Basis::X, TwoWeight::Up) => [Some(lt), Some(rt), None, None],
            (Basis::Z, TwoWeight::Left) => [Some(lt), Some(lb), None, None],
            (Basis::Z, TwoWeight::Right) => [None, None, Some(rt), Some(rb)],
            _ => unreachable!("two-weight pattern unused by the merge stage"),
        };
        Self::new(ancilla, basis, slots)
    }

    /// Emit this tile's action for the given round slot.
    fn run(&mut self, slot: usize, b: &mut EscapeBuilder) -> Result<(), CompileError> {
        match slot {
            0 => b.reset(self.basis, self.ancilla),
            5 => {
                let key = b.measure(self.basis, self.ancilla)?;
                self.rounds.push(key);
            }
            _ => {
                if let Some(data) = self.slots[slot - 1] {
                    match self.basis {
                        Basis::Z => b.cx(data, self.ancilla)?,
                        Basis::X => b.cx(self.ancilla, data)?,
                    }
                }
            }
        }
        Ok(())
    }

    /// The stabilizer's support in final block-frame coordinates.
    fn support(&self, layout: &SurgeryLayout) -> PauliMap {
        let pauli = layout.pauli(Pauli::from(self.basis));
        self.slots
            .iter()
            .flatten()
            .map(|&q| (layout.map_msc(q), pauli))
            .collect()
    }

    fn center(&self, layout: &SurgeryLayout) -> IVec2 {
        layout.map_msc(self.ancilla)
    }
}

#[derive(Clone, Copy)]
enum TwoWeight {
    Up,
    Down,
    Left,
    Right,
}

/// The stabilizers measured every merge round: the surgery seam (STE:1348–58)
/// plus the intermediate patch minus its deleted seam-side two-weights
/// (LSCG:98–127, 402–407).
fn merge_driver_tiles(d_int: i32) -> Vec<DriverTile> {
    let (ox, oy) = SURFACE_OFFSET;
    let mut tiles = Vec::new();

    // Seam: one four-weight (bridging STEANE_5/STEANE_3 to the patch) plus
    // (d_int − 1)/2 − 1 pure-surface two-weights.
    let mut seam = vec![DriverTile::four_weight((4, 16), Basis::Z)];
    for i in 0..((d_int - 1) / 2 - 1) {
        seam.push(DriverTile::two_weight(
            (8 + 4 * i, 16),
            Basis::Z,
            TwoWeight::Down,
        ));
    }
    for mut tile in seam {
        tile.seam = true;
        tiles.push(tile);
    }

    for i in 0..d_int {
        for j in 0..d_int {
            let (x, y) = (ox + 2 * j, oy + 2 * i);
            // The i == 0 seam-side X two-weights are deleted during the merge.
            if i == d_int - 1 && j % 2 == 1 {
                tiles.push(DriverTile::two_weight(
                    (x + 1, y + 1),
                    Basis::X,
                    TwoWeight::Up,
                ));
            }
            if j == 0 && i % 2 == 1 {
                tiles.push(DriverTile::two_weight(
                    (x - 1, y + 1),
                    Basis::Z,
                    TwoWeight::Right,
                ));
            }
            if j == d_int - 1 && i % 2 == 0 && i < d_int - 1 {
                tiles.push(DriverTile::two_weight(
                    (x + 1, y + 1),
                    Basis::Z,
                    TwoWeight::Left,
                ));
            }
            if i < d_int - 1 && j < d_int - 1 {
                let basis = if (i + j) % 2 == 0 { Basis::Z } else { Basis::X };
                tiles.push(DriverTile::four_weight((x + 1, y + 1), basis));
            }
        }
    }
    tiles
}

// ==========================================================================
// Circuit builder
// ==========================================================================

/// Handle for a pooled measurement's record id, resolved via the table
/// [`EscapeBuilder::finish`] returns (ids are only allocated when the
/// measurement's moment is emitted).
#[derive(Clone, Copy, Default)]
struct MeasKey(usize);

/// Transliterates the msc-ls tick stream, coalescing resets and measurements
/// into shared moments by construction: CX batches emit per msc tick (the
/// moment skeleton), while resets and measurements pool across ticks and only
/// flush — each pool as one kind-homogeneous moment — when a later op touches
/// a pooled qubit. Flushing the whole pool at the first forced point is the
/// greedy interval-stabbing optimum, so the merge stage keeps a handful of
/// reset/measurement moments instead of one per msc reset/readout tick.
///
/// All ops within one msc tick act on disjoint qubits, so any serialization
/// is circuit-equivalent; bloq tick indices and measurement record order are
/// therefore not 1:1 with msc-ls's (flows resolve ids through [`MeasKey`]).
struct EscapeBuilder<'a> {
    layout: &'a SurgeryLayout,
    circuit: CoordCircuit,
    /// Current msc tick's CX operands.
    cx: Vec<IVec2>,
    /// Resets awaiting their forced moment, in call order.
    resets: Vec<(GateType, IVec2)>,
    /// Measurements awaiting their forced moment, in call order.
    measures: Vec<(PauliBasis, IVec2, MeasKey)>,
    /// Record id per [`MeasKey`], filled at flush time.
    ids: Vec<u32>,
    /// Whether any moment was emitted yet (tick separators are lazy).
    dirty: bool,
}

impl<'a> EscapeBuilder<'a> {
    fn new(layout: &'a SurgeryLayout) -> Self {
        Self {
            layout,
            circuit: CoordCircuit::new(),
            cx: Vec::new(),
            resets: Vec::new(),
            measures: Vec::new(),
            ids: Vec::new(),
            dirty: false,
        }
    }

    fn has_pending_reset(&self, qubit: IVec2) -> bool {
        self.resets.iter().any(|&(_, q)| q == qubit)
    }

    fn has_pending_measure(&self, qubit: IVec2) -> bool {
        self.measures.iter().any(|&(_, q, _)| q == qubit)
    }

    /// Pool a dual-aware reset of the given base-frame basis. A pending
    /// measurement of the qubit flushes first (measure-before-reset order);
    /// these flush rules keep the two pools qubit-disjoint, so flushing a
    /// whole pool never reorders same-qubit ops.
    fn reset(&mut self, base: Basis, qubit: MscQubit) {
        let qubit = self.layout.map_msc(qubit);
        if self.has_pending_measure(qubit) {
            self.flush_measures();
        }
        self.resets.push((self.layout.reset_gate(base), qubit));
    }

    /// Queue a dual-aware CX (base-frame control/target order), first
    /// flushing any pool holding one of its operands — those moments must
    /// precede this tick's CX moment.
    fn cx(&mut self, control: MscQubit, target: MscQubit) -> Result<(), CompileError> {
        let pair = self
            .layout
            .cx_operands(self.layout.map_msc(control), self.layout.map_msc(target));
        for qubit in pair {
            if self.has_pending_measure(qubit) {
                self.flush_measures();
            }
            if self.has_pending_reset(qubit) {
                self.flush_resets()?;
            }
        }
        self.cx.extend(pair);
        Ok(())
    }

    /// Pool a dual-aware measurement of the given base-frame basis; the
    /// returned key resolves after [`Self::finish`]. A pending reset of the
    /// qubit flushes first (reset-before-measure order).
    fn measure(&mut self, base: Basis, qubit: MscQubit) -> Result<MeasKey, CompileError> {
        let qubit = self.layout.map_msc(qubit);
        if self.has_pending_reset(qubit) {
            self.flush_resets()?;
        }
        if self.has_pending_measure(qubit) {
            self.flush_measures();
        }
        let key = MeasKey(self.ids.len());
        self.ids.push(u32::MAX);
        self.measures
            .push((self.layout.basis(base).into(), qubit, key));
        Ok(key)
    }

    /// Start a fresh moment.
    fn next_moment(&mut self) {
        if self.dirty {
            self.circuit.tick();
        }
        self.dirty = true;
    }

    /// Emit every pooled reset as one moment (grouped per gate type — all
    /// reset flavors are one moment kind).
    fn flush_resets(&mut self) -> Result<(), CompileError> {
        if self.resets.is_empty() {
            return Ok(());
        }
        self.next_moment();
        for (gate, group) in group_by_key(std::mem::take(&mut self.resets)) {
            self.circuit.do_gate(gate, group)?;
        }
        Ok(())
    }

    /// Emit every pooled measurement as one moment, resolving their keys.
    fn flush_measures(&mut self) {
        if self.measures.is_empty() {
            return;
        }
        self.next_moment();
        let pooled = std::mem::take(&mut self.measures)
            .into_iter()
            .map(|(basis, qubit, key)| (basis, (qubit, key)))
            .collect();
        for (basis, batch) in group_by_key(pooled) {
            let (qubits, keys): (Vec<_>, Vec<_>) = batch.into_iter().unzip();
            for (key, id) in keys.into_iter().zip(self.circuit.measure(basis, qubits)) {
                self.ids[key.0] = id;
            }
        }
    }

    /// End the current msc tick: emit its CX moment. Pools stay pooled — they
    /// flush when a later op forces them, coalescing across msc ticks.
    fn tick(&mut self) -> Result<(), CompileError> {
        if self.cx.is_empty() {
            return Ok(());
        }
        self.next_moment();
        Ok(self
            .circuit
            .do_gate(GateType::CX, std::mem::take(&mut self.cx))?)
    }

    /// Flush the remaining pools and return the circuit plus the
    /// key → record id table.
    fn finish(mut self) -> Result<(CoordCircuit, Vec<u32>), CompileError> {
        self.flush_measures();
        self.flush_resets()?;
        Ok((self.circuit, self.ids))
    }
}

/// Bucket a pooled batch by key, preserving first-seen key order.
///
/// A pool flushes as one moment, but the emitters take one gate type (or one
/// basis) per call, so the moment is issued as a handful of same-key batches.
fn group_by_key<K: PartialEq, V>(items: Vec<(K, V)>) -> Vec<(K, Vec<V>)> {
    let mut groups: Vec<(K, Vec<V>)> = Vec::new();
    for (key, value) in items {
        match groups.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, values)) => values.push(value),
            None => groups.push((key, vec![value])),
        }
    }
    groups
}

// ==========================================================================
// Merge chunk
// ==========================================================================

/// Named measurement handles the gateway and downstream tests need.
struct MergeHandles {
    /// First-round seam measurements (the hand-rolled `A_1A_R` plus each seam
    /// driver tile's round-1 outcome): the `Z_L ⊗ Z_L` surgery record.
    pub(crate) seam_first_round: Vec<u32>,
    /// Destructive readout of the Steane west-edge logical (`m1, m4, m6`).
    pub(crate) logical_readout: [u32; 3],
}

/// Build the single merge-stage chunk: 18 msc ticks interleaving the ZXZ
/// Steane segments (STE:1370–1692), the seam drivers, and the intermediate
/// patch's surface rounds (LSCG:395–421, `SURFACE_SYNDROME_EXTRACTION_OFFSET
/// = 0`).
fn merge_chunk(layout: &SurgeryLayout) -> Result<(Chunk, MergeHandles), CompileError> {
    let d_int = layout.intermediate_distance() as i32;
    let mut tiles = merge_driver_tiles(d_int);
    let mut b = EscapeBuilder::new(layout);

    // Steane-side measurement keys, named after the msc-ls sources.
    let mut a1a = [MeasKey::default(); 3];
    let mut ax0145 = [MeasKey::default(); 2];
    let mut az0145 = [MeasKey::default(); 2];
    let mut ax0235 = [MeasKey::default(); 2];
    let mut az0235 = [MeasKey::default(); 2];
    let mut ax0246 = [MeasKey::default(); 2];
    let mut az0246 = [MeasKey::default(); 2];

    let run_tiles = |tiles: &mut Vec<DriverTile>,
                     slot: usize,
                     b: &mut EscapeBuilder|
     -> Result<(), CompileError> {
        for tile in tiles.iter_mut() {
            tile.run(slot, b)?;
        }
        Ok(())
    };

    // The merge runs as three uniform 7-moment rounds — [R][CX×5][M] — the
    // hand-optimized repacking of the msc-ls 18-tick stream (each call keeps
    // its "msc t = n" provenance). Driver tiles occupy CX ticks 1–4 of each
    // round; the 5th CX tick is pure Steane. Resets and measurements pool in
    // the builder and flush as the round's single reset/measurement moment.

    // ---- Round 1: superdense cycle 1 (0145 completes), qubit 2 out, first
    // seam readout. -------------------------------------------------------
    b.reset(Basis::X, A_0145_4);
    b.reset(Basis::Z, A_0145_015);
    b.reset(Basis::X, A_0235_035);
    b.reset(Basis::Z, A_0235_2);
    b.reset(Basis::X, A_0246_46);
    b.reset(Basis::Z, A_0246_02);
    b.reset(Basis::Z, STEANE_2_);
    b.reset(Basis::Z, A_1A_R); // msc t = 1
    for i in 0..d_int {
        for j in 0..d_int {
            b.reset(
                Basis::X,
                (SURFACE_OFFSET.0 + 2 * j, SURFACE_OFFSET.1 + 2 * i),
            );
        }
    }
    run_tiles(&mut tiles, 0, &mut b)?;
    b.tick()?;

    // CX 1 — entangle ancilla pairs; move qubit 2 out (msc t = 1).
    b.cx(A_0145_4, A_0145_015)?;
    b.cx(A_0235_035, A_0235_2)?;
    b.cx(A_0246_46, A_0246_02)?;
    b.cx(STEANE[2], STEANE_2_)?;
    run_tiles(&mut tiles, 1, &mut b)?;
    b.tick()?;

    // CX 2 — CX(1) (msc t = 2).
    b.cx(STEANE[5], A_0145_015)?;
    b.cx(STEANE[0], A_0235_035)?;
    b.cx(STEANE[2], A_0235_2)?;
    b.cx(STEANE[6], A_0246_46)?;
    b.cx(STEANE[1], A_1A_R)?;
    run_tiles(&mut tiles, 2, &mut b)?;
    b.tick()?;

    // CX 3 — CX(2); qubit 2 lands at STEANE_2_ (msc t = 3).
    b.cx(STEANE[1], A_0145_015)?;
    b.cx(STEANE[5], A_0235_035)?;
    b.cx(STEANE[4], A_0246_46)?;
    b.cx(STEANE[0], A_0246_02)?;
    b.cx(STEANE_2_, STEANE[2])?;
    b.cx(SURFACE_A, A_1A_R)?;
    run_tiles(&mut tiles, 3, &mut b)?;
    b.tick()?;

    // CX 4 — CX(3) (msc t = 4). Qubit 2's vacated home is deliberately *not*
    // re-initialized, the same shuttle deviation the cultivation syndrome round
    // takes (`steane::syndrome_round_chunk`): the round-2 move-back is then not
    // a clean move and its `MX(5,11)` byproduct is no longer an independent
    // post-selection — it folds into the chains that carry X on qubit 2.
    b.cx(STEANE[4], A_0145_4)?;
    b.cx(STEANE[0], A_0145_015)?;
    b.cx(STEANE[3], A_0235_035)?;
    b.cx(STEANE_2_, A_0246_02)?;
    run_tiles(&mut tiles, 4, &mut b)?;
    b.tick()?;

    // CX 5 — disentangle 0145; CX(4) (msc t = 5).
    b.cx(A_0145_4, A_0145_015)?;
    b.cx(A_0235_035, STEANE[5])?;
    b.cx(A_0246_02, STEANE_2_)?;
    b.tick()?;

    // Round-1 measurements — first seam readout (msc t = 4), Z0145 cycle-1
    // readout (msc t = 6), surface round 1.
    a1a[0] = b.measure(Basis::Z, A_1A_R)?;
    ax0145[0] = b.measure(Basis::X, A_0145_4)?;
    az0145[0] = b.measure(Basis::Z, A_0145_015)?;
    run_tiles(&mut tiles, 5, &mut b)?;

    // ---- Round 2: whole 0145 cycle 2, CX(5)/CX(6) of 0235/0246, qubit 2
    // back home, remaining seam readouts, destructive m1. ------------------
    b.reset(Basis::X, A_0145_4); // msc t = 7
    b.reset(Basis::Z, A_0145_015); // msc t = 7
    b.reset(Basis::Z, A_1A_L); // msc t = 7
    b.reset(Basis::Z, A_1A_R); // msc t = 9
    run_tiles(&mut tiles, 0, &mut b)?;
    b.tick()?;

    // CX 1 — CX(5) heads (msc t = 6); qubit 2 move-back starts (msc t = 6);
    // re-entangle 0145 (msc t = 8); left seam entangles (msc t = 8).
    b.cx(A_0235_035, STEANE[0])?;
    b.cx(A_0246_46, STEANE[6])?;
    b.cx(STEANE[2], STEANE_2_)?;
    b.cx(A_0145_4, A_0145_015)?;
    b.cx(STEANE[1], A_1A_L)?;
    run_tiles(&mut tiles, 1, &mut b)?;
    b.tick()?;

    // CX 2 — CX(6) (msc t = 7); left seam couples the patch (msc t = 9);
    // 0145 cycle-2 CX(1) head (msc t = 9).
    b.cx(A_0235_035, STEANE[3])?;
    b.cx(A_0235_2, STEANE[2])?;
    b.cx(A_0246_46, STEANE[4])?;
    b.cx(A_0246_02, STEANE[0])?;
    b.cx(SURFACE_A, A_1A_L)?;
    b.cx(STEANE[1], A_0145_015)?;
    run_tiles(&mut tiles, 2, &mut b)?;
    b.tick()?;

    // CX 3 — disentangle 0235/0246, qubit 2 home (msc t = 8); 0145 cycle-2
    // CX(1)/CX(2) tails (msc t = 9/10); right seam re-entangles (msc t = 10).
    b.cx(A_0235_035, A_0235_2)?;
    b.cx(A_0246_46, A_0246_02)?;
    b.cx(STEANE_2_, STEANE[2])?;
    b.cx(STEANE[4], A_0145_4)?;
    b.cx(STEANE[0], A_0145_015)?;
    b.cx(STEANE[1], A_1A_R)?;
    run_tiles(&mut tiles, 3, &mut b)?;
    b.tick()?;

    // CX 4 — 0145 cycle-2 CX(3) (msc t = 11); right seam couples the patch
    // (msc t = 11).
    b.cx(STEANE[5], A_0145_015)?;
    b.cx(SURFACE_A, A_1A_R)?;
    run_tiles(&mut tiles, 4, &mut b)?;
    b.tick()?;

    // CX 5 — disentangle 0145 cycle 2 (msc t = 12).
    b.cx(A_0145_4, A_0145_015)?;
    b.tick()?;

    // Round-2 measurements — 0235/0246 cycle-1 readouts + the move byproduct
    // (msc t = 9), both remaining seam readouts (msc t = 10/12), 0145
    // cycle-2 readouts (msc t = 13), destructive m1 (msc t = 11), surface
    // round 2.
    ax0235[0] = b.measure(Basis::X, A_0235_035)?;
    az0235[0] = b.measure(Basis::Z, A_0235_2)?;
    ax0246[0] = b.measure(Basis::X, A_0246_46)?;
    az0246[0] = b.measure(Basis::Z, A_0246_02)?;
    let steane2_move = b.measure(Basis::X, STEANE_2_)?;
    a1a[1] = b.measure(Basis::Z, A_1A_L)?;
    a1a[2] = b.measure(Basis::Z, A_1A_R)?;
    ax0145[1] = b.measure(Basis::X, A_0145_4)?;
    az0145[1] = b.measure(Basis::Z, A_0145_015)?;
    let m1 = b.measure(Basis::X, STEANE[1])?;
    run_tiles(&mut tiles, 5, &mut b)?;

    // ---- Round 3: 0235/0246 cycle 2 (qubit 2 shuttles out again), the
    // destructive readout, the x_ab splice, surface round 3. ---------------
    b.reset(Basis::X, A_0235_035); // msc t = 10
    b.reset(Basis::Z, A_0235_2); // msc t = 10
    b.reset(Basis::X, A_0246_46); // msc t = 10
    b.reset(Basis::Z, A_0246_02); // msc t = 10
    b.reset(Basis::Z, STEANE_2_); // msc t = 10
    b.reset(Basis::X, A_AB); // msc t = 14
    run_tiles(&mut tiles, 0, &mut b)?;
    b.tick()?;

    // CX 1 — entangle 0235/0246 (msc t = 11); qubit 2 out (msc t = 11).
    b.cx(A_0235_035, A_0235_2)?;
    b.cx(A_0246_46, A_0246_02)?;
    b.cx(STEANE[2], STEANE_2_)?;
    run_tiles(&mut tiles, 1, &mut b)?;
    b.tick()?;

    // CX 2 — cycle-2 CX(1) (msc t = 12).
    b.cx(STEANE[6], A_0246_46)?;
    b.cx(STEANE[0], A_0246_02)?;
    b.cx(STEANE[5], A_0235_035)?;
    run_tiles(&mut tiles, 2, &mut b)?;
    b.tick()?;

    // CX 3 — cycle-2 CX(2) (msc t = 13); A_AB couples SURFACE_A (msc t = 15).
    b.cx(A_AB, SURFACE_A)?;
    b.cx(STEANE[4], A_0246_46)?;
    b.cx(STEANE_2_, A_0246_02)?;
    b.cx(STEANE[0], A_0235_035)?;
    b.cx(STEANE[2], A_0235_2)?;
    run_tiles(&mut tiles, 3, &mut b)?;
    b.tick()?;

    // CX 4 — cycle-2 CX(3) (msc t = 14); disentangle 0246 (msc t = 15); A_AB
    // couples SURFACE_B (msc t = 16).
    b.cx(A_AB, SURFACE_B)?;
    b.cx(STEANE[3], A_0235_035)?;
    b.cx(A_0246_46, A_0246_02)?;
    run_tiles(&mut tiles, 4, &mut b)?;
    b.tick()?;

    // CX 5 — disentangle 0235 (msc t = 15).
    b.cx(A_0235_035, A_0235_2)?;
    b.tick()?;

    // Round-3 measurements — cycle-2 readouts (msc t = 16), the destructive
    // readout (msc t = 13–16), x_ab (msc t = 17), surface round 3.
    az0246[1] = b.measure(Basis::Z, A_0246_02)?;
    az0235[1] = b.measure(Basis::Z, A_0235_2)?;
    let m0 = b.measure(Basis::X, STEANE[0])?;
    let m2 = b.measure(Basis::X, STEANE[2])?;
    let m2_ = b.measure(Basis::X, STEANE_2_)?;
    let m4 = b.measure(Basis::X, STEANE[4])?;
    let m5 = b.measure(Basis::X, STEANE[5])?;
    let m6 = b.measure(Basis::X, STEANE[6])?;
    let m3 = b.measure(Basis::X, STEANE[3])?;
    ax0246[1] = b.measure(Basis::X, A_0246_46)?;
    ax0235[1] = b.measure(Basis::X, A_0235_035)?;
    let m_ab = b.measure(Basis::X, A_AB)?;
    run_tiles(&mut tiles, 5, &mut b)?;

    // Pooled measurements only receive record ids when their moment flushes:
    // resolve every key now that the circuit is complete.
    let (circuit, ids) = b.finish()?;
    let rec = |key: MeasKey| ids[key.0];
    let a1a = a1a.map(rec);
    let ax0145 = ax0145.map(rec);
    let az0145 = az0145.map(rec);
    let ax0235 = ax0235.map(rec);
    let az0235 = az0235.map(rec);
    let ax0246 = ax0246.map(rec);
    let az0246 = az0246.map(rec);
    let [steane2_move, m0, m1, m2, m2_, m3, m4, m5, m6, m_ab] =
        [steane2_move, m0, m1, m2, m2_, m3, m4, m5, m6, m_ab].map(rec);

    // ---- Flows ----------------------------------------------------------
    let face = |face: usize, pauli: Pauli| -> PauliMap {
        FACES[face]
            .iter()
            .map(|&q| (layout.map_msc(STEANE[q]), layout.pauli(pauli)))
            .collect()
    };
    let restart = |start: PauliMap, recs: &[u32]| {
        Flow::new(start, PauliMap::empty())
            .with_measurements(recs.iter().copied())
            .with_marker(FlowMarker::Restart)
    };
    let local_restart = |recs: &[u32]| restart(PauliMap::empty(), recs);

    // Steane-side chains and parities. Every closure of a cultivation chain
    // and every superdense-cycle comparison is post-selected (design §3.2).
    // Rec sets were derived against the ported circuit with stim
    // (`flow_generators`, unsigned) and are pinned by `verify_flows`. The
    // non-obvious sets:
    // - the round-2 Z0145 comparison picks up the Z the 0246 ancilla dumps
    //   onto STEANE_0 at t=7 (`CX(A_0246_02, STEANE_0)`), absorbed by its
    //   readout;
    // - cycle-2 of 0235/0246 re-extracts Z only; its Z readout is
    //   deterministic through the pair entanglement (0235 alone; 0246 with
    //   the Z the 0235 ancilla dumps onto qubit 2 at t=7, absorbed by
    //   `az0235[0]`);
    // - the terminal reconstructions (STE:1682–87) splice against the
    //   cycle-1 X readouts so each parity closes within the chunk;
    // - the seam-anticommuting X0145 face survives as X0145·X_A·X_B (the
    //   seam dumps cancel pairwise) until the destructive readout closes it
    //   as `[m0, m1, m4, m5, m_ab]` (STE:1686) — the §4 cross-template
    //   restart carrying the cultivation flag;
    // - both hand-rolled `Z_1A` round comparisons are post-selected
    //   (STE:1557, 1601);
    // - with qubit 2's old home never re-initialized (the deliberate deviation
    //   above), the move byproduct `steane2_move` is not deterministic alone:
    //   it folds into the X0235 consumer and the 0246 destructive
    //   reconstruction — msc-ls's standalone `MX (5,11)` post-selection is one
    //   fewer independent constraint here.
    let mut flows = vec![
        restart(face(0, Pauli::Z), &[az0145[0]]),
        local_restart(&[ax0145[0]]),
        local_restart(&[az0145[0], az0145[1], az0246[0]]),
        local_restart(&[ax0145[1]]),
        restart(face(1, Pauli::X), &[ax0235[0], steane2_move]),
        restart(face(1, Pauli::Z), &[az0235[0]]),
        restart(face(2, Pauli::X), &[ax0246[0]]),
        restart(face(2, Pauli::Z), &[az0246[0]]),
        local_restart(&[ax0235[1]]),
        local_restart(&[az0235[1]]),
        local_restart(&[ax0246[1]]),
        local_restart(&[az0246[0], az0246[1], az0235[0]]),
        local_restart(&[ax0235[0], m0, m2, m2_, m3, m5]),
        local_restart(&[ax0246[0], steane2_move, m0, m2, m2_, m4, m6]),
        restart(face(0, Pauli::X), &[m0, m1, m4, m5, m_ab]),
        local_restart(&[a1a[0], a1a[1]]),
        local_restart(&[a1a[1], a1a[2]]),
    ];

    // Surface stabilizers: first-round determinism for X tiles (|+⟩ init),
    // round comparisons, and creators toward E1 are decoder detectors. Seam
    // tiles compare post-selected and never re-create (the seam splits at
    // readout).
    for tile in &tiles {
        let rounds: Vec<u32> = tile.rounds.iter().map(|&key| rec(key)).collect();
        debug_assert_eq!(rounds.len(), 3);
        let marker = |post_selected: bool| {
            if post_selected {
                FlowMarker::Restart
            } else {
                FlowMarker::Detector
            }
        };
        let center = tile.center(layout);
        let comparison = |a: u32, b: u32, post_selected: bool| {
            Flow::new(PauliMap::empty(), PauliMap::empty())
                .with_measurements([a, b])
                .with_center(center)
                .with_marker(marker(post_selected))
        };
        if tile.seam {
            flows.push(comparison(rounds[0], rounds[1], true));
            flows.push(comparison(rounds[1], rounds[2], true));
            continue;
        }
        if tile.basis == Basis::X {
            flows.push(
                Flow::new(PauliMap::empty(), PauliMap::empty())
                    .with_measurements([rounds[0]])
                    .with_center(center)
                    .with_marker(FlowMarker::Detector),
            );
        }
        flows.push(comparison(rounds[0], rounds[1], false));
        flows.push(comparison(rounds[1], rounds[2], false));
        flows.push(
            Flow::new(PauliMap::empty(), tile.support(layout))
                .with_measurements([rounds[2]])
                .with_center(center),
        );
    }

    // The recovered `x_ab`-splice stabilizer: `MX(A_AB)` measures X_A·X_B
    // (A_AB leaves reset |+⟩ and coupled both), so the tile E1 re-creates at
    // the same ancilla starts with history (LSCG:438–439).
    let x_ab_support: PauliMap = [SURFACE_A, SURFACE_B]
        .into_iter()
        .map(|q| (layout.map_msc(q), layout.pauli(Pauli::X)))
        .collect();
    flows.push(
        Flow::new(PauliMap::empty(), x_ab_support)
            .with_measurements([m_ab])
            .with_center(layout.map_msc(A_AB)),
    );

    let seam_first_round = std::iter::once(a1a[0])
        .chain(tiles.iter().filter(|t| t.seam).map(|t| rec(t.rounds[0])))
        .collect();

    Ok((
        Chunk { circuit, flows },
        MergeHandles {
            seam_first_round,
            logical_readout: [m1, m4, m6],
        },
    ))
}

// ==========================================================================
// Observable gateway
// ==========================================================================

/// Build the observable gateway. A T block is non-Clifford: it
/// *terminates* the stabilizer flow, so its logical correlation surface cannot
/// pass through — it must be **anchored** by an endpoint operator to be defined
/// deterministically. Each entry encodes one such surface:
///
/// - `operator_in` — where the surface **starts**: the Steane logical edge
///   string the cultivation produces (`Z_L` on the seam-line qubits
///   `{1, 3, 5}`, `X_L` on the west edge `{1, 4, 6}` ⊥ the seam). Not a −Z
///   *boundary* operator (nothing enters a T block from below); it is the
///   magic-state source endpoint, closed internally by the cultivation seam
///   when the templates compose.
/// - `measurements` — how the surface is **transported**: the first-round seam
///   records (`Z_L`) / destructive readout `m1, m4, m6` (`X_L`), then the
///   E1-round stabilizers of the strip that shifts it from the seam-adjacent
///   edge to the block midline.
/// - `operator_out` — the surface's **final form** at the +Z face midline: the
///   contract a downstream block connects to.
///
/// Movers come uniformly from E1 (deviating from the design's "merge round 1
/// when d_int == d" note): the mover strip includes the seam-side two-weights,
/// which are deleted during the whole merge, so round 1 cannot supply them —
/// E1, which recovers every final-patch tile, always can.
fn build_gateway(
    layout: &SurgeryLayout,
    handles: &MergeHandles,
    e1: &Chunk,
    final_patch: &Patch,
) -> ObservableGateway {
    let d = layout.distance as i32;
    // E1 tile measurement ids by ancilla (every tile's creator carries its
    // own single measurement).
    let e1_ids: crate::FxMap<IVec2, u32> = e1
        .flows
        .iter()
        .filter(|flow| flow.start.is_empty() && !flow.end.is_empty())
        .filter_map(|flow| Some((flow.center?, flow.measurements[0])))
        .collect();

    // Base-frame final patch (top basis X) picks the mover strips; the map
    // transports them per variant. It *is* the escape's final patch unless the
    // variant flipped the top basis.
    let x_patch = (layout.top_basis() != Basis::X)
        .then(|| make_normal_surface_code_patch(layout.distance, Basis::X));
    let base_patch = x_patch.as_ref().unwrap_or(final_patch);
    let movers = |strip: &dyn Fn(IVec2) -> bool, basis: Basis| -> Vec<u32> {
        base_patch
            .tiles()
            .iter()
            .filter(|tile| tile.basis() == basis && strip(tile.measure_qubit()))
            .map(|tile| {
                *e1_ids
                    .get(&layout.map(tile.measure_qubit()))
                    .expect("every base-patch tile exists in the E1 round")
            })
            .collect()
    };
    // `Z_L` is a data row, `X_L` a data column; both are logical for any odd
    // index in `1..=2d − 1`. Landing them on row/column `line` costs the
    // plaquettes between `line` and where the merge left the surface: the
    // seam-adjacent row `y = 1` for `Z_L`, the column `x = 2d − 1` opposite it
    // for `X_L` (the mirrored embedding puts msc's west column at +x).
    let z_rep = |line: i32| {
        (
            (0..d)
                .map(|i| {
                    (
                        layout.map(IVec2::new(2 * i + 1, line)),
                        layout.pauli(Pauli::Z),
                    )
                })
                .collect::<PauliMap>(),
            movers(&|m: IVec2| (2..line).contains(&m.y), Basis::Z),
        )
    };
    let x_rep = |line: i32| {
        (
            (0..d)
                .map(|i| {
                    (
                        layout.map(IVec2::new(line, 2 * i + 1)),
                        layout.pauli(Pauli::X),
                    )
                })
                .collect::<PauliMap>(),
            movers(&|m: IVec2| (line + 1..2 * d - 1).contains(&m.x), Basis::X),
        )
    };
    let (midline_row, z_movers) = z_rep(d);
    let (midline_col, x_movers) = x_rep(d);

    // The correlation-surface start endpoints: the Steane logical edge strings
    // (`Z_L` on the seam line {1,3,5}, `X_L` on the west edge {1,4,6}).
    let steane_edge = |qubits: &[usize], pauli: Pauli| -> PauliMap {
        qubits
            .iter()
            .map(|&q| (layout.map_msc(STEANE[q]), layout.pauli(pauli)))
            .collect()
    };

    let connectivity = Connectivity::ISOLATED.with_pipe(Direction::ZPLUS);
    let entry = |merge_recs: &[u32],
                 mover_recs: Vec<u32>,
                 operator_in: PauliMap,
                 operator_out: PauliMap| {
        GatewayEntry {
            measurements: vec![
                ChunkMeasurements {
                    chunk_index: MERGE_CHUNK,
                    measurements: merge_recs.to_vec(),
                },
                ChunkMeasurements {
                    chunk_index: E1_CHUNK,
                    measurements: mover_recs,
                },
            ],
            operator_in,
            operator_out,
        }
    };

    let mut gateway = ObservableGateway::new();
    gateway.insert(
        LocalStabilizer::new(layout.pauli(Pauli::Z), connectivity),
        entry(
            &handles.seam_first_round,
            z_movers.clone(),
            steane_edge(&[1, 3, 5], Pauli::Z),
            midline_row,
        ),
    );
    gateway.insert(
        LocalStabilizer::new(layout.pauli(Pauli::X), connectivity),
        entry(
            &handles.logical_readout,
            x_movers.clone(),
            steane_edge(&[1, 4, 6], Pauli::X),
            midline_col,
        ),
    );

    // Armless (ISOLATED) crossings: an escaped-patch logical absorbed onto a
    // memory cube above the T block, anchored below by the T source but with no
    // arm continuing out any face (design §3.3). Observable resolution routes
    // such a crossing down its worldline to this gateway
    // (`lower/observable.rs::source_t_block`), so the byproduct record branch is
    // folded exactly once. The record set is the arm entry's byproduct
    // (`Z` picks up the `X` byproduct `a = seam ⊕ z_movers`; `X` picks up
    // `b = m₁m₄m₆ ⊕ x_movers`) — the same physical parity, since a straight
    // memory transport folds no records into the logical. Boundary operators are
    // empty: the absorbed segment is a self-contained horizontal logical, not a
    // temporal seam, so it never cancels against a neighbour.
    gateway.insert(
        LocalStabilizer::isolated(layout.pauli(Pauli::Z)),
        entry(
            &handles.seam_first_round,
            z_movers,
            PauliMap::empty(),
            PauliMap::empty(),
        ),
    );
    gateway.insert(
        LocalStabilizer::isolated(layout.pauli(Pauli::X)),
        entry(
            &handles.logical_readout,
            x_movers,
            PauliMap::empty(),
            PauliMap::empty(),
        ),
    );

    gateway
}

#[cfg(test)]
mod tests {
    use bloq_graph::Direction;
    use bloq_stim::StimFlowVerifier;
    use rstest::rstest;

    use super::*;
    use crate::block::fixed_bulk::t::{assert_ticks_are_kind_homogeneous, family_layout};

    fn base_layout(distance: u32) -> SurgeryLayout {
        family_layout(Direction::YMINUS, false, distance)
    }

    /// Run the full cultivation+escape composite with the TRUE `T` seed
    /// (`bloq_vm`), and per shot compare the gateway record folds (`r_A` =
    /// seam first round ⊕ Z movers, `r_B` = m₁m₄m₆ ⊕ X movers) against the
    /// frame-free byproduct read directly off the escaped patch state. The
    /// output is `X^a Z^b |T̄⟩_mid` (`⟨X̄_mid⟩ = (−1)^b/√2`,
    /// `⟨Ȳ_mid⟩ = (−1)^{a⊕b}/√2`), and the folds match **exactly**:
    /// `a = r_A`, `b = r_B` on every shot — the circuit-level bookkeeping
    /// (measurement conventions, reset fillers, representative closure,
    /// movers) carries no sign constant anywhere, and neither does the state
    /// orientation (positive-frame cultivation, see
    /// `steane::tests::cultivation_delivers_exact_t_state`). Together they
    /// pin the frame-constant-free output frames
    /// lowered by `registry/readouts.rs`.
    #[test]
    fn gateway_record_folds_equal_physical_byproduct() {
        use bloq_vm::{CircuitExecutor, EnginePauli, EnginePauliString as PauliString, Simulator};
        use glam::IVec2;

        use crate::block::fixed_bulk::t::steane;

        let layout = base_layout(3);
        let mut chunks = vec![
            ChunkOrLoop::Single(Box::new(steane::injection_chunk(&layout).unwrap())),
            ChunkOrLoop::Single(Box::new(steane::syndrome_round_chunk(&layout).unwrap())),
            ChunkOrLoop::Single(Box::new(steane::check_chunk(&layout).unwrap())),
        ];
        let (escape, mut gateway) = escape_chunks(&layout).unwrap();
        chunks.extend(escape);
        gateway.shift_chunk_indices(3);
        let template = LoweringTemplate::from_chunks(chunks, gateway).unwrap();
        let circuit = &template.program_template.circuit;

        let key = |pauli: Pauli| {
            LocalStabilizer::new(
                layout.pauli(pauli),
                Connectivity::ISOLATED.with_pipe(Direction::ZPLUS),
            )
        };
        let record_set = |pauli: Pauli| -> Vec<u32> {
            template
                .observable_gateway
                .lookup(key(pauli))
                .unwrap()
                .measurements
                .iter()
                .flat_map(|chunk| chunk.measurements.iter().copied())
                .collect()
        };
        let z_records = record_set(Pauli::Z);
        let x_records = record_set(Pauli::X);

        let executor = CircuitExecutor::new(circuit).expect("valid escape circuit");
        let n = executor.qubit_count();
        let map = circuit.build_coord_to_index();
        let d = layout.distance as i32;

        // The callers below deduplicate the row/column crossing first, so the
        // coordinates are distinct and this is a set of sites, not a product.
        let engine_pauli = |pauli| match pauli {
            Pauli::X => EnginePauli::X,
            Pauli::Y => EnginePauli::Y,
            Pauli::Z => EnginePauli::Z,
            Pauli::I => unreachable!("Pauli strings omit identity terms"),
        };
        let string = |terms: &[(IVec2, Pauli)]| -> PauliString {
            PauliString::from_terms(
                n,
                terms
                    .iter()
                    .map(|&(coord, p)| (map[&coord] as usize, engine_pauli(p))),
            )
        };
        // Midline logicals of the escaped patch (build_gateway's operator_out
        // strings) and Ȳ := iX̄Z̄ (Y at the (d, d) crossing).
        let row = |i: i32| (layout.map(IVec2::new(2 * i + 1, d)), layout.pauli(Pauli::Z));
        let col = |i: i32| (layout.map(IVec2::new(d, 2 * i + 1)), layout.pauli(Pauli::X));
        let x_mid = string(&(0..d).map(col).collect::<Vec<_>>());
        let y_terms: Vec<(IVec2, Pauli)> = (0..d)
            .flat_map(|i| {
                let (rc, rp) = row(i);
                let (cc, cp) = col(i);
                if rc == cc {
                    vec![(rc, Pauli::Y)]
                } else {
                    vec![(rc, rp), (cc, cp)]
                }
            })
            .collect();
        // Deduplicate the crossing (row/col meet once, at (d, d)).
        let mut seen = std::collections::HashSet::new();
        let y_terms: Vec<(IVec2, Pauli)> = y_terms
            .into_iter()
            .filter(|(c, _)| seen.insert(*c))
            .collect();
        let y_mid = string(&y_terms);

        const TOL: f64 = 1e-9;
        let inv_sqrt2 = std::f64::consts::FRAC_1_SQRT_2;
        let mut classes = std::collections::HashSet::new();
        for seed in 0..24u64 {
            let mut sim = Simulator::with_seed(n, seed);
            let rec = executor.run_shot(&mut sim).unwrap();
            let fold = |ids: &[u32]| {
                ids.iter().fold(false, |acc, &id| {
                    acc ^ rec.get(id).expect("gateway readout must be available")
                })
            };
            let r_a = fold(&z_records);
            let r_b = fold(&x_records);
            let ex = sim.peek_observable_expectation(&x_mid).unwrap();
            let ey = sim.peek_observable_expectation(&y_mid).unwrap();
            assert!(
                (ex.abs() - inv_sqrt2).abs() < TOL && (ey.abs() - inv_sqrt2).abs() < TOL,
                "seed {seed}: patch state off the T orbit (X={ex:+.4}, Y={ey:+.4})"
            );
            let b = ex < 0.0;
            let a = (ey < 0.0) ^ b;
            assert_eq!(
                (a, b),
                (r_a, r_b),
                "seed {seed}: gateway record folds must equal the physical byproduct"
            );
            classes.insert((a, b));
        }
        assert_eq!(
            classes.len(),
            4,
            "expected all four byproduct classes across the batch, saw {classes:?}"
        );
    }

    /// The ISOLATED gateway entries (an absorbed escaped-patch logical on a
    /// memory cube above the T block, routed here by
    /// `lower/observable.rs::source_t_block`) carry exactly the arm entries'
    /// byproduct record branch and no boundary operators. Since
    /// `gateway_record_folds_equal_physical_byproduct` pins the arm folds to the
    /// physically-simulated byproduct, this transitively pins the ISOLATED
    /// entries: a bare-`Z` crossing folds the `X`-byproduct branch `a`, a
    /// bare-`X` crossing folds the `Z`-byproduct branch `b`. Checked across both
    /// circuit families (base and X↔Z dual).
    #[rstest]
    fn isolated_entries_carry_arm_byproduct_without_operators(#[values(false, true)] dual: bool) {
        let layout = family_layout(Direction::YMINUS, dual, 3);
        let (_, gateway) = escape_chunks(&layout).unwrap();
        for pauli in [Pauli::Z, Pauli::X] {
            let basis = layout.pauli(pauli);
            let arm = gateway
                .lookup(LocalStabilizer::new(
                    basis,
                    Connectivity::ISOLATED.with_pipe(Direction::ZPLUS),
                ))
                .expect("arm entry exists");
            let isolated = gateway
                .lookup(LocalStabilizer::isolated(basis))
                .expect("isolated entry exists");
            assert_eq!(
                isolated.measurements, arm.measurements,
                "{pauli} isolated records must equal the arm byproduct branch",
            );
            assert!(
                isolated.operator_in.is_empty() && isolated.operator_out.is_empty(),
                "{pauli} isolated entry is a self-contained horizontal logical, no seam operators",
            );
        }
    }

    /// [`EscapeBuilder`]'s pooling coalesces the reset/measurement moments by
    /// construction: the merge chunk keeps a handful, not one per msc
    /// reset/readout tick. Regression guard for the coalescing (semantics are
    /// covered by the flow and distance tests).
    #[test]
    fn merge_chunk_coalesces_reset_and_measure_moments() {
        use bloq_circuit::Op;
        let (merge, _) = merge_chunk(&base_layout(3)).unwrap();
        let body = merge.circuit.body(merge.circuit.entry_body()).unwrap();
        let (mut reset_moments, mut measure_moments, mut kind) = (0, 0, None);
        for op in body.ops() {
            match op {
                Op::Tick => kind = None,
                Op::Measure { .. } => {
                    if kind.replace('m') != Some('m') {
                        measure_moments += 1;
                    }
                }
                Op::Gate { gate, .. } if gate.is_reset() && kind.replace('r') != Some('r') => {
                    reset_moments += 1;
                }
                _ => {}
            }
        }
        assert!(
            reset_moments <= 4 && measure_moments <= 4,
            "merge chunk should coalesce to few reset/measure moments, \
             got {reset_moments} reset and {measure_moments} measure moments"
        );
    }

    #[test]
    fn merge_surface_band_checks_feed_decoder() {
        let layout = base_layout(7);
        let (merge, _) = merge_chunk(&layout).unwrap();
        let centers = [layout.map_msc((4, 18)), layout.map_msc((2, 20))];
        let checks: Vec<_> = merge
            .flows
            .iter()
            .filter(|flow| {
                flow.start.is_empty()
                    && flow.end.is_empty()
                    && flow.center.is_some_and(|center| centers.contains(&center))
            })
            .collect();

        assert_eq!(checks.len(), 6);
        assert!(
            checks
                .iter()
                .all(|flow| flow.marker == FlowMarker::Detector)
        );
    }

    #[rstest]
    fn escape_chunks_verify_flows_per_variant(
        #[values(
            Direction::YMINUS,
            Direction::XPLUS,
            Direction::YPLUS,
            Direction::XMINUS
        )]
        side: Direction,
        #[values(false, true)] dual: bool,
        #[values(3, 5, 7)] distance: u32,
    ) {
        let layout = family_layout(side, dual, distance);
        let (chunks, ..) = escape_chunks(&layout).unwrap();
        assert_eq!(chunks.len(), 5);
        for (index, chunk) in chunks.iter().enumerate() {
            let ChunkOrLoop::Single(chunk) = chunk else {
                panic!("escape chunks are all Single");
            };
            assert!(
                !chunk.circuit.has_moments_conflict(),
                "chunk {index}: qubit reused within a moment"
            );
            chunk
                .verify_flows(None, None)
                .unwrap_or_else(|error| panic!("chunk {index} flows fail stim: {error}"));
        }
    }

    /// Each escape chunk keeps one op kind per tick (editor-clean display).
    #[test]
    fn escape_ticks_are_kind_homogeneous() {
        let (chunks, ..) = escape_chunks(&base_layout(3)).unwrap();
        for (index, chunk) in chunks.iter().enumerate() {
            let ChunkOrLoop::Single(chunk) = chunk else {
                unreachable!()
            };
            assert_ticks_are_kind_homogeneous(&format!("escape chunk {index}"), &chunk.circuit);
        }
    }

    /// Restart-set equivalence: composing cultivation + escape into one
    /// template, the restart parities span the GF(2) space of msc-ls's
    /// post-selected detectors for the same geometry (base variant, ZXZ, S+
    /// imperfect init — `tools/dump_msc_ls_post_selection.py` regenerates the
    /// fixtures from the msc-ls sources; the checked-in `data/*.stim` files
    /// predate the current coordinate layout and are not used), modulo the two
    /// `(5,11)` shuttle checks bloq deliberately gives up (see below). bloq
    /// adds no parity outside the reference span.
    ///
    /// Span comparison, not listing comparison (module docs): bloq pairs
    /// consecutive same-cycle measurements where msc-ls emits singles, and
    /// "restart when any parity is odd" depends only on the span. Both sides
    /// are keyed by (block-local qubit, per-qubit measurement ordinal), which
    /// is invariant under bloq's tick re-serialization.
    ///
    /// The fixture disables the complementary-gap pipeline's heuristic
    /// surface code post-selection, so patch-tile checks are decoder detectors
    /// and absent from both restart sets.
    #[rstest]
    #[case::d3(3)]
    #[case::d5(5)]
    #[case::d7(7)]
    fn restart_set_spans_msc_ls_post_selection(#[case] distance: u32) {
        use crate::block::fixed_bulk::t::steane::{
            check_chunk, injection_chunk, syndrome_round_chunk,
        };

        let layout = base_layout(distance);
        let d_int = layout.intermediate_distance() as i32;

        // Merged single template: the cross-stage chains (escape merge rounds
        // comparing against cultivation's last syndrome round) close here as
        // plain template restarts, mirroring msc-ls's single-circuit listing.
        let mut chunks = vec![
            ChunkOrLoop::Single(Box::new(injection_chunk(&layout).unwrap())),
            ChunkOrLoop::Single(Box::new(syndrome_round_chunk(&layout).unwrap())),
            ChunkOrLoop::Single(Box::new(check_chunk(&layout).unwrap())),
        ];
        // Empty gateway: its `chunk_index` entries are authored for the
        // escape-only chunk list and the restart comparison never reads it.
        let (escape, ..) = escape_chunks(&layout).unwrap();
        chunks.extend(escape);
        let template = LoweringTemplate::from_chunks(chunks, ObservableGateway::new()).unwrap();
        let program = &template.program_template;

        // Measurement id -> (qubit, per-qubit ordinal). Registry records are
        // sorted by id = emission order, so counting occurrences per qubit
        // reproduces msc-ls's per-qubit measurement ordinals.
        let mut per_qubit: crate::FxMap<IVec2, u32> = crate::FxMap::default();
        let mut key_of: crate::FxMap<u32, (IVec2, u32)> = crate::FxMap::default();
        for record in program.circuit.meas_registry().records() {
            let ordinal = per_qubit.entry(record.qubit).or_insert(0);
            key_of.insert(record.id, (record.qubit, *ordinal));
            *ordinal += 1;
        }

        let compared: Vec<_> = program
            .restarts
            .iter()
            .map(|restart| {
                restart
                    .parity
                    .measurements()
                    .map(|id| key_of[&id])
                    .collect::<Vec<_>>()
            })
            .collect();

        // Bloq's span sits two generators BELOW msc-ls's, one per dropped
        // re-init of qubit 2's vacated home. Adding them back recovers the
        // reference span, and the rank bump pins that bloq really leaves those
        // two directions open (the flag, by contrast, IS spanned — it rides the
        // logical anchor chain, `steane::transversal_y`):
        //
        // - the cultivation (5,11) shuttle check: msc-ls's standalone MX(5,11)
        //   (ordinal 0) folds into the 0235/0246 X faces it shuttled through
        //   (`steane::syndrome_round_chunk`);
        // - the escape (5,11) shuttle check: the merge drops the same re-init
        //   after its qubit-2 move-out, folding the standalone move byproduct
        //   `steane2_move` (ordinal 1) into the X0235 consumer and the 0246
        //   destructive reconstruction (`merge_chunk` flow notes).
        let cultivation_shuttle = vec![(layout.map_msc((5, 11)), 0)];
        let escape_shuttle = vec![(layout.map_msc((5, 11)), 1)];
        let bloq_rank = gf2_row_basis(&compared).len();
        let mut augmented = compared;
        augmented.push(cultivation_shuttle);
        augmented.push(escape_shuttle);
        assert_eq!(
            gf2_row_basis(&augmented).len(),
            bloq_rank + 2,
            "both folded (5,11) shuttle checks are independent of bloq's span"
        );

        let fixture = match d_int {
            3 => include_str!("testdata/msc_ls_post_selection_dint3.txt"),
            5 => include_str!("testdata/msc_ls_post_selection_dint5.txt"),
            _ => unreachable!("d_int is 3 or 5 by the §2.1 rule"),
        };
        let expected: Vec<Vec<(IVec2, u32)>> = fixture
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                line.split_whitespace()
                    .map(|token| {
                        let (coord, ordinal) = token.split_once(':').unwrap();
                        let (x, y) = coord.split_once(',').unwrap();
                        (
                            layout.map_msc((x.parse().unwrap(), y.parse().unwrap())),
                            ordinal.parse().unwrap(),
                        )
                    })
                    .collect()
            })
            .collect();

        // Span equality once the two shuttle checks are added back. Rank
        // equality alone would not catch a bloq parity outside msc-ls's span,
        // so compare the bases.
        assert_eq!(
            gf2_row_basis(&augmented),
            gf2_row_basis(&expected),
            "restart span differs from msc-ls post-selection span (d = {distance})"
        );
    }

    /// Row-reduced GF(2) basis of a set of parities over (qubit, ordinal)
    /// columns — two parity sets define the same restart predicate iff their
    /// bases agree.
    fn gf2_row_basis(parities: &[Vec<(IVec2, u32)>]) -> Vec<Vec<(IVec2, u32)>> {
        let mut columns: Vec<(IVec2, u32)> = parities.iter().flatten().copied().collect();
        columns.sort_unstable_by_key(|&(qubit, ordinal)| (qubit.x, qubit.y, ordinal));
        columns.dedup();
        let index_of: crate::FxMap<(IVec2, u32), usize> = columns
            .iter()
            .enumerate()
            .map(|(index, &key)| (key, index))
            .collect();

        let mut rows: Vec<Vec<u64>> = parities
            .iter()
            .map(|parity| {
                let mut row = vec![0u64; columns.len().div_ceil(64)];
                for key in parity {
                    // XOR, not OR: a parity listing a measurement twice
                    // cancels it.
                    row[index_of[key] / 64] ^= 1 << (index_of[key] % 64);
                }
                row
            })
            .collect();

        // Gaussian elimination to reduced row echelon form.
        let mut pivot_rows: Vec<Vec<u64>> = Vec::new();
        for col in 0..columns.len() {
            let (word, bit) = (col / 64, 1u64 << (col % 64));
            let Some(pivot) = rows.iter().position(|row| row[word] & bit != 0) else {
                continue;
            };
            let pivot_row = rows.swap_remove(pivot);
            for row in &mut rows {
                if row[word] & bit != 0 {
                    for (lhs, rhs) in row.iter_mut().zip(&pivot_row) {
                        *lhs ^= rhs;
                    }
                }
            }
            for row in &mut pivot_rows {
                if row[word] & bit != 0 {
                    for (lhs, rhs) in row.iter_mut().zip(&pivot_row) {
                        *lhs ^= rhs;
                    }
                }
            }
            pivot_rows.push(pivot_row);
        }

        pivot_rows
            .iter()
            .map(|row| {
                columns
                    .iter()
                    .enumerate()
                    .filter(|(col, _)| row[col / 64] & (1 << (col % 64)) != 0)
                    .map(|(_, &key)| key)
                    .collect()
            })
            .collect()
    }

    /// Standalone escape template: boundary flows are exactly the six
    /// start-open restart-flagged Steane chains (toward cultivation) plus one
    /// end-open detector creator per final-patch tile (the standard memory
    /// face).
    #[rstest]
    fn escape_template_builds_standalone(
        #[values(false, true)] dual: bool,
        #[values(3, 7)] distance: u32,
    ) {
        let layout = family_layout(Direction::YMINUS, dual, distance);
        let template = escape_template(&layout).unwrap();
        let program = &template.program_template;

        let final_patch = make_normal_surface_code_patch(distance, layout.top_basis());
        let expected_ends: crate::FxSet<PauliMap> = final_patch
            .tiles()
            .iter()
            .map(crate::block::patch::Tile::pauli_map)
            .collect();

        let (start_open, end_open): (Vec<_>, Vec<_>) = program
            .boundary_flows
            .iter()
            .partition(|flow| !flow.start.is_empty());

        assert_eq!(start_open.len(), 6, "six Steane chains enter the template");
        for flow in &start_open {
            assert!(flow.end.is_empty());
            assert_eq!(flow.marker, FlowMarker::Restart);
        }
        let starts: crate::FxSet<PauliMap> =
            start_open.iter().map(|flow| flow.start.clone()).collect();
        let expected_starts: crate::FxSet<PauliMap> = FACES
            .iter()
            .flat_map(|face| {
                [Pauli::X, Pauli::Z].map(|pauli| {
                    face.iter()
                        .map(|&q| (layout.map_msc(STEANE[q]), layout.pauli(pauli)))
                        .collect()
                })
            })
            .collect();
        assert_eq!(starts, expected_starts);

        assert_eq!(end_open.len(), expected_ends.len());
        let ends: crate::FxSet<PauliMap> = end_open.iter().map(|flow| flow.end.clone()).collect();
        assert_eq!(ends, expected_ends, "end face is a standard memory face");
        for flow in &end_open {
            assert_eq!(flow.marker, FlowMarker::Detector);
        }
    }

    /// Composing cultivation and escape closes every Steane chain: only the
    /// end-open detector chains remain, and the fused X0145 closure carries
    /// the cultivation flag and syndrome MX(1,13) (the cross-template
    /// restart).
    #[test]
    fn cultivation_and_escape_compose_and_close() {
        use crate::block::fixed_bulk::t::steane;

        let layout = base_layout(3);
        let mut chunks = vec![
            ChunkOrLoop::Single(Box::new(steane::injection_chunk(&layout).unwrap())),
            ChunkOrLoop::Single(Box::new(steane::syndrome_round_chunk(&layout).unwrap())),
            ChunkOrLoop::Single(Box::new(steane::check_chunk(&layout).unwrap())),
        ];
        let (escape, ..) = escape_chunks(&layout).unwrap();
        chunks.extend(escape);
        let template = LoweringTemplate::from_chunks(chunks, ObservableGateway::new()).unwrap();
        let program = &template.program_template;

        assert!(
            program
                .boundary_flows
                .iter()
                .all(|flow| flow.start.is_empty() && !flow.end.is_empty()),
            "only the end face stays open"
        );

        // Template measurement ids: syndrome 0..=7 (MX(1,13) is 0), check
        // 8..=14 (flag is 8), merge 15.. — the fused X0145 chain closes with
        // the flag and MX(1,13) plus the five-term destructive readout.
        let x0145 = program.restarts.iter().any(|restart| {
            let terms: Vec<u32> = restart.parity.measurements().collect();
            terms.contains(&0)
                && terms.contains(&8)
                && terms.iter().filter(|&&m| m >= 15).count() == 5
        });
        assert!(x0145, "fused X0145 restart carries flag + MX(1,13)");
    }

    /// The native-basis rule: every pure-surface stabilizer the merge places
    /// sits on its checkerboard color for all 8 variants. The seam four-weight
    /// (Steane support) and the hand-rolled Z_1A ride off-lattice qubits and
    /// are exempt by construction.
    #[rstest]
    fn escape_places_only_checkerboard_conformant_surface_ancillas(
        #[values(
            Direction::YMINUS,
            Direction::XPLUS,
            Direction::YPLUS,
            Direction::XMINUS
        )]
        side: Direction,
        #[values(false, true)] dual: bool,
    ) {
        use crate::block::fixed_bulk::utils::checkerboard_basis;
        for distance in [3, 5, 7] {
            let layout = family_layout(side, dual, distance);
            let d = distance as i32;
            let d_int = layout.intermediate_distance() as i32;
            for tile in merge_driver_tiles(d_int) {
                let support = tile.support(&layout);
                let pure_surface = support.iter().all(|(coord, _)| {
                    (1..2 * d).contains(&coord.x) && (1..2 * d).contains(&coord.y)
                });
                if !pure_surface {
                    continue;
                }
                assert_eq!(
                    checkerboard_basis(tile.center(&layout)),
                    layout.basis(tile.basis),
                    "{side} dual={dual} d={distance} tile at {}",
                    tile.center(&layout)
                );
            }
        }
    }

    /// Structural counts are variant-invariant and match the msc-ls geometry.
    #[rstest]
    fn escape_structural_counts_per_variant(
        #[values(
            Direction::YMINUS,
            Direction::XPLUS,
            Direction::YPLUS,
            Direction::XMINUS
        )]
        side: Direction,
        #[values(false, true)] dual: bool,
        #[values(3, 5, 7)] distance: u32,
    ) {
        let layout = family_layout(side, dual, distance);
        let d_int = layout.intermediate_distance();
        let (chunks, ..) = escape_chunks(&layout).unwrap();
        assert_eq!(chunks.len(), 5);
        let ChunkOrLoop::Single(merge) = &chunks[0] else {
            unreachable!()
        };
        // 3 rounds over (d_int² − 1) driver stabilizers plus the 25 named
        // Steane-side records (3 seam + 12 superdense + 1 move + 8 destructive
        // + x_ab).
        assert_eq!(
            merge.circuit.num_measurements(),
            3 * (d_int * d_int - 1) + 25
        );

        // E1's first-round deterministic detectors follow the expansion
        // diagonal: fully-initialized same-basis tiles only exist at d = 7.
        let ChunkOrLoop::Single(e1) = &chunks[1] else {
            unreachable!()
        };
        let deterministic = e1
            .flows
            .iter()
            .filter(|flow| flow.start.is_empty() && flow.end.is_empty())
            .count();
        // Pinned empirically for the 5 → 7 expansion (stim verifies each as a
        // valid deterministic flow; this guards the init-diagonal geometry).
        let expected = if distance == 7 { 11 } else { 0 };
        assert_eq!(deterministic, expected, "E1 diagonal-rule detectors");
    }

    /// The gateway compiles msc-ls's Zero/Plus observable definitions: the
    /// seam-teleported logical reads the first-round seam measurements, the
    /// destructive logical reads m1/m4/m6, and both shift edge → midline via
    /// E1-round mover stabilizers.
    #[rstest]
    fn escape_gateway_matches_msc_observable_definitions(
        #[values(
            Direction::YMINUS,
            Direction::XPLUS,
            Direction::YPLUS,
            Direction::XMINUS
        )]
        side: Direction,
        #[values(false, true)] dual: bool,
    ) {
        let distance = 5;
        let layout = family_layout(side, dual, distance);
        let d = distance as i32;
        let (_, gateway) = escape_chunks(&layout).unwrap();
        let connectivity = Connectivity::ISOLATED.with_pipe(Direction::ZPLUS);

        // operator_in = the Steane logical edge string (the surface's start
        // endpoint at this non-Clifford block).
        let steane_edge = |qubits: &[usize], pauli: Pauli| -> PauliMap {
            qubits
                .iter()
                .map(|&q| (layout.map_msc(STEANE[q]), layout.pauli(pauli)))
                .collect()
        };

        let seam_key = LocalStabilizer::new(layout.pauli(Pauli::Z), connectivity);
        let seam_entry = &gateway[&seam_key];
        assert_eq!(seam_entry.operator_in, steane_edge(&[1, 3, 5], Pauli::Z));
        assert_eq!(seam_entry.measurements[0].chunk_index, 0);
        assert_eq!(
            seam_entry.measurements[0].measurements.len(),
            layout.intermediate_distance().div_ceil(2) as usize,
            "one first-round record per seam stabilizer"
        );
        assert_eq!(seam_entry.measurements[1].chunk_index, 1);
        let expected_row: PauliMap = (0..d)
            .map(|i| (layout.map(IVec2::new(2 * i + 1, d)), layout.pauli(Pauli::Z)))
            .collect();
        assert_eq!(seam_entry.operator_out, expected_row);

        let readout_key = LocalStabilizer::new(layout.pauli(Pauli::X), connectivity);
        let readout_entry = &gateway[&readout_key];
        assert_eq!(readout_entry.operator_in, steane_edge(&[1, 4, 6], Pauli::X));
        assert_eq!(readout_entry.measurements[0].chunk_index, 0);
        assert_eq!(readout_entry.measurements[0].measurements.len(), 3);
        let expected_col: PauliMap = (0..d)
            .map(|i| (layout.map(IVec2::new(d, 2 * i + 1)), layout.pauli(Pauli::X)))
            .collect();
        assert_eq!(readout_entry.operator_out, expected_col);
    }

    // ======================================================================
    // Distance harness (step 7)
    // ======================================================================

    use stim::noise::{NoiseModel, UniformDepolarizing};

    /// Perfect Steane initialization (msc's `perform_perfect_steane_*`): one
    /// MPP moment measuring the six face stabilizers plus the gateway's
    /// `operator_in` — the surface's start endpoint, materialized as an MPP
    /// boundary so the observable is deterministic. Stabilizer records anchor
    /// creators that fuse into the merge chunk's restart closures; the
    /// `operator_in` record feeds the observable directly.
    fn steane_port_chunk(layout: &SurgeryLayout, operator_in: &PauliMap) -> (Chunk, u32) {
        let steane_map = |qubits: &[usize], p: Pauli| -> PauliMap {
            qubits
                .iter()
                .map(|&q| (layout.map_msc(STEANE[q]), layout.pauli(p)))
                .collect()
        };
        let mut products: Vec<PauliMap> = FACES
            .iter()
            .flat_map(|face| [Pauli::X, Pauli::Z].map(|p| steane_map(face, p)))
            .collect();
        products.push(operator_in.clone());

        let mut circuit = CoordCircuit::new();
        let recs = circuit
            .measure_pauli_products(products.iter().cloned())
            .unwrap();
        let flows = products[..6]
            .iter()
            .zip(&recs)
            .map(|(stabilizer, &m)| {
                Flow::new(PauliMap::empty(), stabilizer.clone()).with_measurements([m])
            })
            .collect();
        (Chunk { circuit, flows }, recs[6])
    }

    /// Perfect destructive readout: MPP every final-patch stabilizer (closing
    /// the last stabilization round's creators) plus the logical line.
    fn cap_chunk(patch: &Patch, logical: &PauliMap) -> (Chunk, u32) {
        let mut products: Vec<PauliMap> = patch
            .tiles()
            .iter()
            .map(crate::block::patch::Tile::pauli_map)
            .collect();
        products.push(logical.clone());
        let mut circuit = CoordCircuit::new();
        let recs = circuit
            .measure_pauli_products(products.iter().cloned())
            .unwrap();
        let flows = patch
            .tiles()
            .iter()
            .zip(&recs)
            .map(|(tile, &m)| {
                Flow::new(tile.pauli_map(), PauliMap::empty())
                    .with_measurements([m])
                    .with_center(tile.measure_qubit())
            })
            .collect();
        (Chunk { circuit, flows }, *recs.last().unwrap())
    }

    /// Emit the noiseless port → escape → cap circuit as standalone stim text
    /// with every detector *and restart* parity as a plain `DETECTOR` line
    /// (msc-faithful: its `.stim` carries the post-selected parities as
    /// ordinary detectors and keeps the post-selection index list on the
    /// Python side) and observable 0 = perfect initialization ⊕ gateway
    /// records ⊕ perfect readout.
    fn merged_distance_stim(layout: &SurgeryLayout, pauli: Pauli) -> String {
        let (escape, mut gateway) = escape_chunks(layout).unwrap();
        let key = LocalStabilizer::new(
            layout.pauli(pauli),
            Connectivity::ISOLATED.with_pipe(Direction::ZPLUS),
        );
        let gateway_entry = gateway.lookup(key).unwrap();

        // Materialize the surface's endpoints: operator_in as the perfect
        // Steane port, operator_out as the perfect capped readout.
        let (port, port_logical) = steane_port_chunk(layout, &gateway_entry.operator_in);
        let final_patch = make_normal_surface_code_patch(layout.distance, layout.top_basis());
        let (cap, _) = cap_chunk(&final_patch, &gateway_entry.operator_out);

        let mut chunks = vec![ChunkOrLoop::Single(Box::new(port))];
        chunks.extend(escape);
        chunks.push(ChunkOrLoop::Single(Box::new(cap)));
        gateway.shift_chunk_indices(1);
        let template = LoweringTemplate::from_chunks(chunks, gateway).unwrap();
        let program = &template.program_template;
        assert!(
            program.boundary_flows.is_empty(),
            "port and cap close every chain"
        );

        let detectors: Vec<Vec<u32>> = program
            .detectors
            .iter()
            .map(|detector| detector.parity.measurements().collect())
            .chain(
                program
                    .restarts
                    .iter()
                    .map(|restart| restart.parity.measurements().collect()),
            )
            .collect();
        // The port is chunk 0, so its chunk-local logical id is already a
        // template id; the cap logical MPP is the template's last measurement.
        let mut observable = vec![port_logical, program.circuit.num_measurements() - 1];
        let resolved = template.observable_gateway.lookup(key).unwrap();
        observable.extend(
            resolved
                .measurements
                .iter()
                .flat_map(|chunk| chunk.measurements.iter().copied()),
        );
        bloq_stim::emit_annotated_stim(&program.circuit, &detectors, &[(0, observable)]).unwrap()
    }

    fn undetectable_error_weight(stim_text: &str) -> usize {
        let circuit: stim::Circuit = stim_text.parse().unwrap();
        let noisy = UniformDepolarizing::new(1e-3)
            .unwrap()
            .noisy_circuit_skipping_mpp_boundaries(&circuit)
            .unwrap();
        noisy
            .search_for_undetectable_logical_errors(3, 3, false, true)
            .unwrap()
            .len()
    }

    /// The escape's fault distance through the merge, pinned empirically
    /// at **3 for every d** — not d_int. The weight-3 witness (recovered by
    /// printing `search_for_undetectable_logical_errors` on the circuit
    /// `merged_distance_stim` builds): a data error on the seam-adjacent qubit
    /// (msc `SURFACE_B`) in the gap between the seam four-weight's slot-2
    /// coupling and the neighboring patch tile's slot-3 coupling flips one
    /// E1 mover record (the logical), and its two comparison flips are each
    /// cancelled by a measurement flip. These checks are decoder detectors,
    /// so complementary-GAP confidence can reject ambiguous corrections but
    /// cannot raise the raw combinatorial weight. The pin still guards the
    /// harness: a dropped detector or restart
    /// parity shows up as weight 1–2. Full d sweep on the base variant; one
    /// dual and one reflected variant at d = 3 (distance is
    /// D₄/dual-invariant, structural tests pin the rest).
    #[rstest]
    #[case(Direction::YMINUS, false, 3)]
    #[case(Direction::YMINUS, false, 5)]
    #[case(Direction::YMINUS, false, 7)]
    #[case(Direction::YMINUS, true, 3)]
    #[case(Direction::XPLUS, false, 3)]
    fn escape_distance_through_merge(
        #[case] side: Direction,
        #[case] dual: bool,
        #[case] distance: u32,
        #[values(Pauli::Z, Pauli::X)] pauli: Pauli,
    ) {
        let layout = family_layout(side, dual, distance);
        let text = merged_distance_stim(&layout, pauli);
        assert_eq!(
            undetectable_error_weight(&text),
            3,
            "{side} dual={dual} d={distance} {pauli:?} observable"
        );
    }

    /// Standalone port → E1 → S1..S3 → cap circuit over the pure-surface
    /// tail: perfect MPPs of exactly the stabilizers E1 consumes, an
    /// intermediate-patch logical whose extension onto the full patch is
    /// init-deterministic, and a perfect capped readout.
    fn expansion_distance_stim(pauli: Pauli) -> String {
        let layout = base_layout(7);
        let d = layout.distance as i32;
        let d_int = layout.intermediate_distance() as i32;
        let final_patch = make_normal_surface_code_patch(layout.distance, layout.top_basis());
        let init = expansion_init(&layout);
        // NOTE: the real compiler runs `discard_orphan_resplit_consumers`
        // here (the recovered seam-side two-weights genuinely have no merge
        // history). This isolated harness keeps them so the perfect port can
        // constrain the seam boundary — otherwise that edge is
        // under-initialized and X_L collapses to a spurious weight-3 (a
        // harness artifact, not a code property; the full-merge distance is
        // pinned separately by `escape_distance_through_merge`).
        let e1 = make_surface_code_chunk(&final_patch, Some(&init), None).unwrap();

        // Port: perfect MPPs of every stabilizer E1 consumes (including the
        // recovered seam-side two-weights), plus the intermediate-patch
        // logical representative whose extension onto the full patch is
        // init-deterministic (Z: the seam-side row y = 1; X: the corner
        // column x = 2d − 1 — both extend across qubits the expansion
        // initializes in the matching basis, so the observable needs no
        // mover records).
        let mut products: Vec<PauliMap> = e1
            .flows
            .iter()
            .filter(|flow| !flow.start.is_empty())
            .map(|flow| flow.start.clone())
            .collect();
        let line = |range: std::ops::Range<i32>, position: &dyn Fn(i32) -> IVec2| -> PauliMap {
            range
                .map(|i| (layout.map(position(i)), layout.pauli(pauli)))
                .collect()
        };
        let (port_line, cap_line) = match pauli {
            Pauli::Z => (
                line(d - d_int..d, &|i| IVec2::new(2 * i + 1, 1)),
                line(0..d, &|i| IVec2::new(2 * i + 1, 1)),
            ),
            _ => (
                line(0..d_int, &|i| IVec2::new(2 * d - 1, 2 * i + 1)),
                line(0..d, &|i| IVec2::new(2 * d - 1, 2 * i + 1)),
            ),
        };
        products.push(port_line);
        let mut circuit = CoordCircuit::new();
        let recs = circuit
            .measure_pauli_products(products.iter().cloned())
            .unwrap();
        let port_logical = *recs.last().unwrap();
        let flows = products[..recs.len() - 1]
            .iter()
            .zip(&recs)
            .map(|(stabilizer, &m)| {
                Flow::new(PauliMap::empty(), stabilizer.clone()).with_measurements([m])
            })
            .collect();
        let port = Chunk { circuit, flows };

        let (cap, _) = cap_chunk(&final_patch, &cap_line);
        let mut chunks = vec![
            ChunkOrLoop::Single(Box::new(port)),
            ChunkOrLoop::Single(Box::new(e1)),
        ];
        for _ in 0..3 {
            chunks.push(ChunkOrLoop::Single(Box::new(
                make_surface_code_chunk(&final_patch, None, None).unwrap(),
            )));
        }
        chunks.push(ChunkOrLoop::Single(Box::new(cap)));
        let template = LoweringTemplate::from_chunks(chunks, ObservableGateway::new()).unwrap();
        let program = &template.program_template;
        assert!(program.boundary_flows.is_empty());
        assert!(
            program.restarts.is_empty(),
            "no post-selection after the merge"
        );

        let detectors: Vec<Vec<u32>> = program
            .detectors
            .iter()
            .map(|detector| detector.parity.measurements().collect())
            .collect();
        let observable = vec![port_logical, program.circuit.num_measurements() - 1];
        bloq_stim::emit_annotated_stim(&program.circuit, &detectors, &[(0, observable)]).unwrap()
    }

    /// Graphlike distance of the pure-surface tail, **d_int = 5 for both
    /// observables** — matching msc-ls's own downward-expansion patch
    /// (`surface_code_expansion.SurfaceCodePatch`, verified independently by
    /// building its circuit and running `shortest_graphlike_error`; it
    /// reports 5 for Z_L, and 5 for X_L once initialized in X so the logical
    /// is deterministic).
    ///
    /// The expanded code has distance `d` (7), but the *growth step* is only
    /// protected to `d_int`: a logical string crosses the freshly-initialized
    /// region at zero cost (same-basis errors on just-initialized qubits are
    /// init-time stabilizers), so only its `d_int`-long segment through the
    /// intermediate patch costs faults. Expansion raises the distance of
    /// *future* rounds, not of the growth moment. These pins guard the
    /// harness wiring, the init-diagonal geometry, and the seam-boundary
    /// initialization (see `expansion_distance_stim`).
    #[rstest]
    fn escape_distance_after_expansion(#[values(Pauli::Z, Pauli::X)] pauli: Pauli) {
        let text = expansion_distance_stim(pauli);
        let circuit: stim::Circuit = text.parse().unwrap();
        let noisy = UniformDepolarizing::new(1e-3)
            .unwrap()
            .noisy_circuit_skipping_mpp_boundaries(&circuit)
            .unwrap();
        assert_eq!(noisy.shortest_graphlike_error().unwrap().len(), 5);
    }
}
