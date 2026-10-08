//! Steane-code stage library: the cultivation half of a T block.
//!
//! Ports the three msc-ls cultivation stages as data-driven chunks (Port-block
//! precedent — hand-written circuit + flows, no round machinery):
//!
//! 1. [`injection_chunk`] — 13-tick unitary (non-FT) encoding of the magic
//!    state into the d=3 Steane code (`injection_generator`, STE:495). The
//!    tick-8 seed is the **true non-Clifford `T`**, reversing msc-ls's
//!    `S_DAG` stand-in orientation to match bloq's positive logical-Z
//!    convention. Clifford-only consumers substitute it via
//!    [`GateType::clifford_proxy`] (`T -> S`); the editor program view and
//!    simulator see the real gate.
//! 2. [`syndrome_round_chunk`] — one superdense ZX syndrome round
//!    (`zx_syndrome_extraction_after_injection_generator`, STE:708; the
//!    variant the msc-ls surgery pipeline composes, LSCG:373). All 8
//!    measurements are post-selected → restart parities.
//! 3. [`check_chunk`] — the transversal Hadamard-test check certifying the T
//!    state (`check_generator`, STE:828): the true non-Clifford `T … T_DAG`
//!    conjugation sandwich, whose `S … S_DAG` proxy is the complex conjugate
//!    of msc-ls's `S_DAG … S`; flag MX
//!    on Steane qubit 0, reverse cascade, 6 post-selected ancilla MXs. The
//!    single msc-ls `perform_check` implicitly performs the papers' *pair* of
//!    "Check T" boxes.
//!
//! Coordinates are authored in msc-ls space and transported per variant
//! through [`SurgeryLayout::map_msc`] — the rotation only. The merge-basis
//! dual deliberately leaves cultivation untouched: the stage
//! prepares the same magic state regardless of how the escape measures it;
//! only the surgery and readout dualize.
//!
//! ## Flow decomposition
//!
//! Every msc-ls cultivation detector is a post-selected single measurement,
//! deterministic only given the codespace prepared upstream. In chunk-flow
//! terms that determinism is a *chain closure*, so the stages decompose like
//! a standard syndrome round:
//!
//! - injection emits one creator flow `∅ → S` per Steane stabilizer;
//!   every flow in these chunks is restart-flagged, because every closure of
//!   a cultivation stabilizer chain — here or in the escape template — is
//!   post-selected;
//! - the syndrome round consumes each stabilizer (closing a restart chain
//!   whose parity is exactly the msc-ls post-selected single) and re-creates
//!   it toward the check;
//! - the check has no stabilizer-closing measurement (its ancilla web only
//!   reconstructs syndromes jointly with the flag), so its stabilizer flows
//!   are pass-throughs; the three X-face pass-throughs splice in the flag.
//!
//! One msc-ls post-selection is not a stabilizer chain at all: the check's
//! flag reads the *logical* operator the cultivated state is an eigenstate of.
//! It travels its own chain — injection creates, the syndrome round transports,
//! the check reads back — described in [`transversal_y`].
//!
//! The measurement sets on each flow were derived from the msc-ls reference
//! circuits with stim (`has_flow`, minimal subsets) and are pinned semantically
//! by the `verify_flows` tests below. This module holds the canonical
//! restart-parity accounting: 14 template-local entries against msc-ls's 15,
//! the one deviation being the qubit-2 shuttle readout described in
//! [`syndrome_round_chunk`].

use bloq_circuit::{
    Chunk, ChunkOrLoop, CoordCircuit, Flow, FlowMarker, GateType, PauliBasis, PauliMap,
};
use bloq_graph::Pauli;
use glam::IVec2;

use super::{FACES, MscQubit, STEANE_FINAL, STEANE_INJECTED, SurgeryLayout};
use crate::CompileError;
use crate::block::LoweringTemplate;
use crate::block::gateway::ObservableGateway;

/// The magic-state injection stage (STE:495): 13 ticks, no measurements.
/// Flows: one restart-flagged creator per Steane stabilizer at the
/// post-injection positions.
pub(crate) fn injection_chunk(layout: &SurgeryLayout) -> Result<Chunk, CompileError> {
    let mut b = StageBuilder::new(layout);
    // msc-ls resets each qubit mid-sweep, right before its first CX — mixing
    // reset and two-qubit ticks. Every such qubit idles from circuit start to
    // its reset, so hoisting all resets into the opening tick is
    // circuit-equivalent and keeps each tick to a single op kind.
    b.gate(
        GateType::RX,
        &[
            (5, 13),
            (3, 13),
            (3, 15),
            (1, 13),
            (6, 12),
            (3, 11),
            (5, 15),
            (2, 12),
            (2, 10),
            (0, 14),
        ],
    )?;
    b.gate(GateType::RZ, &[(4, 12), (4, 14), (2, 14)])?;
    b.tick();
    b.cx(&[((5, 13), (4, 12)), ((3, 13), (4, 14)), ((3, 15), (2, 14))])?;
    b.tick();
    b.cx(&[((5, 13), (4, 14)), ((3, 13), (2, 14))])?;
    b.tick();
    b.cx(&[
        ((3, 13), (4, 12)),
        ((3, 15), (4, 14)),
        ((1, 13), (2, 14)),
        ((6, 12), (5, 13)),
    ])?;
    b.tick();
    b.cx(&[
        ((3, 11), (4, 12)),
        ((5, 15), (4, 14)),
        ((2, 12), (1, 13)),
        ((5, 13), (6, 12)),
    ])?;
    b.tick();
    b.cx(&[((2, 12), (3, 11)), ((4, 14), (5, 15)), ((2, 14), (1, 13))])?;
    b.tick();
    b.cx(&[((4, 12), (3, 11)), ((1, 13), (2, 12))])?;
    b.tick();
    b.cx(&[((3, 11), (2, 12))])?;
    b.tick();
    // The seed of the magic state: the true non-Clifford `T` (bloq keeps the
    // real gate and lets the Clifford proxy substitute it for Clifford-only
    // consumers — see module docs). DELIBERATE DEVIATION from msc-ls (whose
    // stand-in is `S_DAG`, i.e. a `T_DAG` seed): msc-ls's implicit logical
    // frame is `Z_L = −Z₁Z₃Z₅` / `Y_L = Y₁Y₄Y₆`, in which its `T_DAG` seed IS
    // logical T; bloq's frame is the all-positive `Z̄ = +Z₁Z₃Z₅` /
    // `Ȳ = iX̄Z̄ = −Y₁Y₄Y₆` (the escape gateway's strings), where the seed
    // transports as `C₂ Z_v C₂† = +Z₁Z₄Z₆ ≡ +Z̄` (CX-only, sign-free), so a
    // `T` seed is the positive-frame logical T̄ and the delivered state is
    // exactly `|T̄⟩ = T̄|+̄⟩` — eliminating the output-frame conversion
    // constant. The check sandwich below flips with it, keeping seed and
    // certification consistent.
    b.gate(GateType::T, &[(2, 12)])?;
    b.tick();
    b.cx(&[((3, 11), (2, 12))])?;
    b.tick();
    b.cx(&[((1, 13), (2, 12))])?;
    b.tick();
    b.cx(&[((2, 10), (3, 11)), ((0, 14), (1, 13))])?;
    b.tick();
    b.cx(&[((3, 11), (2, 10)), ((1, 13), (0, 14))])?;

    let mut flows: Vec<Flow> = stabilizer_supports(layout, &STEANE_INJECTED)
        .map(|support| restart_flow(PauliMap::empty(), support, &[], &[]))
        .collect();
    // The logical anchor (see [`transversal_y`]): injection seeds the magic
    // state, so the anchor is a creator here.
    flows.push(
        restart_flow(
            PauliMap::empty(),
            transversal_y(layout, &STEANE_INJECTED),
            &[],
            &[],
        )
        .with_sign(true),
    );
    Ok(Chunk {
        circuit: b.circuit,
        flows,
    })
}

/// One superdense ZX syndrome round after injection (STE:708). Moves qubit 1
/// to its final home and qubit 2 out and back.
///
/// Deviates from msc-ls: qubit 2's vacated home is *not* re-initialized
/// mid-round. The move-back `CX((6,12),(5,11)); CX((5,11),(6,12))` is then not
/// a clean move — the pair ends stabilized by `Z(6,12)Z(5,11)`, so `MX(5,11)`
/// comes out random and its byproduct rides every chain carrying X on qubit 2.
/// Each such chain absorbs it (see the rec tables below), so nothing is lost
/// except msc-ls's standalone `MX(5,11)` post-selection: 14 template-local
/// parities where the reference declares 15.
pub(crate) fn syndrome_round_chunk(layout: &SurgeryLayout) -> Result<Chunk, CompileError> {
    // Measurements, in emission order:
    // `[MX(1,13), MZ(2,14), MZ(0,14), MX(4,14), MZ(5,13), MX(3,11), MZ(4,12), MX(5,11)]`.
    // Consumers close the injection creators into restart parities. Measurement
    // 2 is the qubit-1 move byproduct, deterministic on its own; measurement 7
    // is the qubit-2 one and is not (module docs above), so it folds into the
    // 0235 X face it shuttled through.
    const CONSUMER_RECS: [&[u32]; 6] = [&[0], &[1], &[3, 7], &[4], &[5], &[6]];
    // Correlation of each re-created stabilizer with the round's outcomes (the
    // mid-round qubit moves entangle faces, hence the cross-face Z sets). The
    // un-re-initialized (5,11) rides the 0246 X face on re-creation.
    const CREATOR_RECS: [&[u32]; 6] = [&[0], &[6], &[3], &[], &[5, 7], &[1, 4, 6]];
    const LOCAL_RESTARTS: [&[u32]; 1] = [&[2]];

    let mut b = StageBuilder::new(layout);
    b.gate(GateType::RX, &[(1, 13)])?;
    b.gate(GateType::RZ, &[(2, 14), (1, 15)])?;
    b.gate(GateType::RX, &[(4, 14)])?;
    b.gate(GateType::RZ, &[(5, 13)])?;
    b.gate(GateType::RX, &[(3, 11)])?;
    b.gate(GateType::RZ, &[(4, 12), (5, 11)])?;
    b.tick();
    b.cx(&[
        ((0, 14), (1, 15)),
        ((1, 13), (2, 14)),
        ((4, 14), (5, 13)),
        ((3, 11), (4, 12)),
        ((6, 12), (5, 11)),
    ])?;
    b.tick();
    b.cx(&[
        ((3, 15), (2, 14)),
        ((0, 14), (1, 13)),
        ((5, 15), (4, 14)),
        ((6, 12), (5, 13)),
        ((2, 10), (3, 11)),
        ((3, 13), (4, 12)),
    ])?;
    b.tick();
    b.cx(&[
        ((1, 15), (0, 14)),
        ((3, 13), (4, 14)),
        ((2, 12), (3, 11)),
        ((5, 11), (6, 12)),
    ])?;
    b.tick();
    b.cx(&[
        ((2, 12), (1, 13)),
        ((3, 13), (2, 14)),
        ((3, 15), (4, 14)),
        ((5, 11), (4, 12)),
    ])?;
    b.tick();
    b.cx(&[
        ((1, 13), (2, 12)),
        ((2, 14), (3, 13)),
        ((4, 14), (3, 15)),
        ((4, 12), (5, 11)),
    ])?;
    b.tick();
    b.cx(&[
        ((2, 14), (3, 15)),
        ((4, 14), (3, 13)),
        ((3, 11), (2, 10)),
        ((6, 12), (5, 11)),
    ])?;
    b.tick();
    b.cx(&[
        ((2, 14), (1, 15)),
        ((4, 14), (5, 15)),
        ((5, 13), (6, 12)),
        ((3, 11), (2, 12)),
        ((4, 12), (3, 13)),
    ])?;
    b.tick();
    b.cx(&[
        ((1, 13), (2, 14)),
        ((4, 14), (5, 13)),
        ((3, 11), (4, 12)),
        ((5, 11), (6, 12)),
    ])?;
    b.tick();
    let measurements = [
        b.measure(PauliBasis::X, (1, 13)),
        b.measure(PauliBasis::Z, (2, 14)),
        b.measure(PauliBasis::Z, (0, 14)),
        b.measure(PauliBasis::X, (4, 14)),
        b.measure(PauliBasis::Z, (5, 13)),
        b.measure(PauliBasis::X, (3, 11)),
        b.measure(PauliBasis::Z, (4, 12)),
        b.measure(PauliBasis::X, (5, 11)),
    ];

    let mut flows = Vec::with_capacity(14);
    let supports_in = stabilizer_supports(layout, &STEANE_INJECTED);
    let supports_out = stabilizer_supports(layout, &STEANE_FINAL);
    for (index, (support_in, support_out)) in supports_in.zip(supports_out).enumerate() {
        flows.push(restart_flow(
            support_in,
            PauliMap::empty(),
            CONSUMER_RECS[index],
            &measurements,
        ));
        flows.push(restart_flow(
            PauliMap::empty(),
            support_out,
            CREATOR_RECS[index],
            &measurements,
        ));
    }
    flows.extend(
        LOCAL_RESTARTS.iter().map(|parity| {
            restart_flow(PauliMap::empty(), PauliMap::empty(), parity, &measurements)
        }),
    );
    // The logical anchor rides along untouched — it commutes with everything
    // the round measures — but it does ride the two Z-face ancillas it shares
    // support with (`[MZ(2,14), MZ(5,13)]`) and, because `Y(6,12)` carries X on
    // qubit 2, the dirty move-back's byproduct `MX(5,11)` as well. Folding that
    // third outcome in is what keeps the anchor a deterministic chain without
    // the re-init: the shuttle readout is individually random, but its
    // randomness *is* the anchor's byproduct.
    flows.push(restart_flow(
        transversal_y(layout, &STEANE_INJECTED),
        transversal_y(layout, &STEANE_FINAL),
        &[1, 4, 7],
        &measurements,
    ));
    Ok(Chunk {
        circuit: b.circuit,
        flows,
    })
}

/// The transversal Hadamard-test check (STE:828). Qubit positions are
/// unchanged; the flag rides the open X-face chains toward the escape
/// template (module docs).
pub(crate) fn check_chunk(layout: &SurgeryLayout) -> Result<Chunk, CompileError> {
    // Measurements, in emission order: flag `MX(3,13)` then the 6 ancilla MXs
    // `[(3,11), (4,12), (5,13), (4,14), (1,13), (2,14)]`. X-face pass-throughs
    // splice in the flag (index 0); Z faces pass through untouched.
    const PASSTHROUGH_RECS: [&[u32]; 6] = [&[0], &[], &[0], &[], &[0], &[]];
    // Template-local restart parities: the 5 standalone-deterministic ancilla
    // MXs plus the flag⊗`MX(4,12)` pair. Neither half of that pair is a Pauli
    // flow on its own — both measure the *logical* `(X̄+Ȳ)/√2` — so the flag's
    // own msc-ls post-selection rides the logical anchor chain instead
    // ([`transversal_y`]), and `MX(4,12)` follows from the two together.
    const LOCAL_RESTARTS: [&[u32]; 6] = [&[1], &[3], &[4], &[5], &[6], &[0, 2]];
    // The ancilla-web CX cascade, one slice per tick; the check replays it in
    // reverse after the flag (the circuit is a palindrome around the flag).
    const CASCADE_TICKS: [&[(MscQubit, MscQubit)]; 4] = [
        &[
            ((3, 11), (2, 10)),
            ((4, 12), (3, 13)),
            ((5, 13), (6, 12)),
            ((4, 14), (5, 15)),
            ((1, 13), (2, 12)),
            ((2, 14), (1, 15)),
        ],
        &[((2, 12), (3, 11)), ((4, 14), (5, 13)), ((3, 13), (2, 14))],
        &[((3, 13), (2, 12)), ((4, 14), (3, 15))],
        &[((3, 13), (4, 14))],
    ];

    let mut b = StageBuilder::new(layout);
    // msc-ls resets the ancilla web and applies the transversal T† in one
    // tick; two ticks keep the reset and rotation kinds unmixed.
    b.gate(
        GateType::RX,
        &[(3, 11), (4, 12), (5, 13), (4, 14), (1, 13), (2, 14)],
    )?;
    b.tick();
    // The transversal non-Clifford conjugation `T … flag-MX … T_DAG` certifying
    // the magic state. DELIBERATE DEVIATION from msc-ls (`S_DAG … S` stand-in,
    // i.e. `T_DAG … T`), mirroring the seed flip above: the measured operator
    // is `⊗(T† X T) = ⊗(X−Y)/√2`, transversally the positive-frame logical
    // `(X̄+Ȳ)/√2` (X-faces → (−Y)⁴-faces = X·Z-face products, Z-faces fixed —
    // the stabilizer group is unchanged), whose +1 eigenstate is the
    // positive-frame `|T̄⟩` the flipped seed injects.
    b.gate(GateType::T, &STEANE_FINAL)?;
    for tick in CASCADE_TICKS {
        b.tick();
        b.cx(tick)?;
    }
    b.tick();
    let flag = b.measure(PauliBasis::X, (3, 13));
    b.tick();
    b.gate(GateType::RX, &[(3, 13)])?;
    for tick in CASCADE_TICKS.iter().rev() {
        b.tick();
        b.cx(tick)?;
    }
    b.tick();
    b.gate(GateType::T_DAG, &STEANE_FINAL)?;
    b.tick();
    let measurements = [
        flag,
        b.measure(PauliBasis::X, (3, 11)),
        b.measure(PauliBasis::X, (4, 12)),
        b.measure(PauliBasis::X, (5, 13)),
        b.measure(PauliBasis::X, (4, 14)),
        b.measure(PauliBasis::X, (1, 13)),
        b.measure(PauliBasis::X, (2, 14)),
    ];

    let mut flows: Vec<Flow> = stabilizer_supports(layout, &STEANE_FINAL)
        .enumerate()
        .map(|(index, support)| {
            restart_flow(
                support.clone(),
                support,
                PASSTHROUGH_RECS[index],
                &measurements,
            )
        })
        .collect();
    flows.extend(
        LOCAL_RESTARTS.iter().map(|parity| {
            restart_flow(PauliMap::empty(), PauliMap::empty(), parity, &measurements)
        }),
    );
    // The check reads the logical anchor back and does not re-create it: the
    // flag is exactly that readout, which is what makes msc-ls's standalone
    // `MX(3,13)` post-selection expressible here (see [`transversal_y`]).
    flows.push(
        restart_flow(
            transversal_y(layout, &STEANE_FINAL),
            PauliMap::empty(),
            &[0],
            &measurements,
        )
        .with_sign(true),
    );
    Ok(Chunk {
        circuit: b.circuit,
        flows,
    })
}

/// The transversal `Y⊗7` at the given positions: the *logical* anchor of the
/// cultivated magic state, and the one post-selection that is not a stabilizer
/// chain.
///
/// Conjugating the check's flag `X(3,13)` back through the ancilla cascade
/// (whose `RX`s pin every ancilla factor to `+1`) and then through the
/// transversal seed gate turns it into `⊗(T†XT) = ⊗(X−Y)/√2`, i.e. the logical
/// `(X̄+Ȳ)/√2` — deterministic precisely because cultivation delivers exactly
/// `|T̄⟩` (`tests::cultivation_delivers_exact_t_state`). The final `MX(4,12)`
/// measures the same operator, which is why the two are a Pauli flow together
/// while neither is alone.
///
/// Under the Clifford proxy that carries flow verification (`T -> S`) the same
/// operator is `⊗(S†XS) = ⊗(−Y) = −Y⊗7`, an honest Pauli, so the anchor is a
/// stabilizer of the injected state and the chain
/// *injection creates → syndrome round transports → check reads back* verifies
/// with stim and composes to the single-measurement restart `{flag}` msc-ls
/// declares. The proxy is what makes the chain expressible; the parity it
/// yields is deterministic under the true non-Clifford `T` as well.
///
/// `Y` is dual-invariant, so — like the rest of cultivation — this needs no
/// dual-aware basis.
fn transversal_y(layout: &SurgeryLayout, positions: &[MscQubit; 7]) -> PauliMap {
    positions
        .iter()
        .map(|&q| (layout.map_msc(q), Pauli::Y))
        .collect()
}

/// The 6 Steane stabilizer supports in table order (`2 * face + {X: 0, Z: 1}`),
/// at the given qubit positions, transported per variant.
fn stabilizer_supports<'a>(
    layout: &'a SurgeryLayout,
    positions: &'a [MscQubit; 7],
) -> impl Iterator<Item = PauliMap> + 'a {
    FACES.iter().flat_map(move |face| {
        [Pauli::X, Pauli::Z].map(move |pauli| {
            face.iter()
                .map(|&qubit| (layout.map_msc(positions[qubit]), pauli))
                .collect()
        })
    })
}

/// A restart-flagged flow between the given interfaces, carrying the
/// `table`-indexed measurements. With both interfaces empty this is a
/// template-local restart parity (a deterministic, post-selected product).
fn restart_flow(start: PauliMap, end: PauliMap, table: &[u32], measurements: &[u32]) -> Flow {
    Flow::new(start, end)
        .with_measurements(recs(table, measurements))
        .with_marker(FlowMarker::Restart)
}

fn recs<'a>(table: &'a [u32], measurements: &'a [u32]) -> impl Iterator<Item = u32> + 'a {
    table.iter().map(move |&index| measurements[index as usize])
}

/// The cultivation template (stages 1–2): injection + one
/// superdense syndrome round + the Hadamard-test check, composed standalone.
/// No observable gateway — the escape template owns the in/out story; the six
/// boundary flows are the restart-flagged open Steane chains the escape
/// template's first merge round closes.
pub(crate) fn cultivation_template(
    layout: &SurgeryLayout,
) -> Result<LoweringTemplate, CompileError> {
    let chunks = vec![
        ChunkOrLoop::Single(Box::new(injection_chunk(layout)?)),
        ChunkOrLoop::Single(Box::new(syndrome_round_chunk(layout)?)),
        ChunkOrLoop::Single(Box::new(check_chunk(layout)?)),
    ];
    LoweringTemplate::from_chunks(chunks, ObservableGateway::new())
}

/// Transliterates one msc-ls tick sequence, mapping every msc-ls coordinate
/// through the variant layout.
struct StageBuilder<'a> {
    layout: &'a SurgeryLayout,
    circuit: CoordCircuit,
}

impl<'a> StageBuilder<'a> {
    fn new(layout: &'a SurgeryLayout) -> Self {
        Self {
            layout,
            circuit: CoordCircuit::new(),
        }
    }

    fn gate(&mut self, gate: GateType, qubits: &[MscQubit]) -> Result<(), CompileError> {
        let targets: Vec<IVec2> = qubits.iter().map(|&q| self.layout.map_msc(q)).collect();
        Ok(self.circuit.do_gate(gate, targets)?)
    }

    fn cx(&mut self, pairs: &[(MscQubit, MscQubit)]) -> Result<(), CompileError> {
        let targets: Vec<IVec2> = pairs
            .iter()
            .flat_map(|&(control, target)| {
                [self.layout.map_msc(control), self.layout.map_msc(target)]
            })
            .collect();
        Ok(self.circuit.do_gate(GateType::CX, targets)?)
    }

    fn measure(&mut self, basis: PauliBasis, qubit: MscQubit) -> u32 {
        self.circuit.measure(basis, [self.layout.map_msc(qubit)])[0]
    }

    fn tick(&mut self) {
        self.circuit.tick();
    }
}

#[cfg(test)]
mod tests {
    use bloq_circuit::DetectorParity;
    use bloq_graph::{Basis, Direction};
    use bloq_stim::StimFlowVerifier;
    use rstest::rstest;

    use super::*;
    use crate::block::fixed_bulk::t::family_layout;

    fn layout(side: Direction, distance: u32) -> SurgeryLayout {
        family_layout(side, false, distance)
    }

    /// Simulate the full cultivation stage alone (injection, syndrome round,
    /// check) with the TRUE `T` seed (`bloq_vm`), and pin the delivered
    /// logical Bloch vector against the exact canonical edge strings the
    /// escape gateway anchors on (`Z̄ = Z{1,3,5}` seam line, `X̄ = X{1,4,6}`
    /// west edge, `Ȳ := iX̄Z̄ = Y₁X₄X₆Z₃Z₅`).
    ///
    /// Cultivation delivers **exactly** `|T̄⟩ = T̄|+̄⟩` in that positive
    /// frame — ⟨X̄⟩ = +1/√2, ⟨Ȳ⟩ = +1/√2, ⟨Z̄⟩ = 0, all stabilizers +1 —
    /// with no Pauli dressing and no sign constant. (The dressed variants
    /// are distinguishable by the (⟨X̄⟩, ⟨Ȳ⟩) sign pattern: ideal (+,+),
    /// X̄-dressed (+,−), Z̄-dressed (−,−), Ȳ-dressed (−,+).) With
    /// `escape::tests::gateway_record_folds_equal_physical_byproduct` this
    /// pins the frame-constant-free output frames
    /// lowered by `registry/readouts.rs`.
    #[test]
    fn cultivation_delivers_exact_t_state() {
        use bloq_vm::{CircuitExecutor, EnginePauli, EnginePauliString as PauliString, Simulator};

        let layout = layout(Direction::YMINUS, 3);
        let template = cultivation_template(&layout).unwrap();
        let circuit = &template.program_template.circuit;
        let executor = CircuitExecutor::new(circuit).expect("valid cultivation circuit");
        let n = executor.qubit_count();
        let coord_to_index = circuit.build_coord_to_index();

        let at = |q: usize| coord_to_index[&layout.map_msc(STEANE_FINAL[q])] as usize;
        // Every string below names distinct qubits, so this is a set of sites
        // rather than a Pauli product — no phase to carry.
        let engine_pauli = |pauli| match pauli {
            Pauli::X => EnginePauli::X,
            Pauli::Y => EnginePauli::Y,
            Pauli::Z => EnginePauli::Z,
            Pauli::I => unreachable!("Pauli strings omit identity terms"),
        };
        let string = |terms: &[(usize, Pauli)]| -> PauliString {
            PauliString::from_terms(n, terms.iter().map(|&(q, p)| (at(q), engine_pauli(p))))
        };

        let x_bar = string(&[(1, Pauli::X), (4, Pauli::X), (6, Pauli::X)]);
        let z_bar = string(&[(1, Pauli::Z), (3, Pauli::Z), (5, Pauli::Z)]);
        let y_bar = string(&[
            (1, Pauli::Y),
            (4, Pauli::X),
            (6, Pauli::X),
            (3, Pauli::Z),
            (5, Pauli::Z),
        ]);
        let stabilizers: Vec<PauliString> = FACES
            .iter()
            .flat_map(|face| {
                [Pauli::X, Pauli::Z]
                    .map(|p| string(&face.iter().map(|&q| (q, p)).collect::<Vec<_>>()))
            })
            .collect();

        const TOL: f64 = 1e-9;
        let m = circuit.num_measurements();
        let mut accepted_shots = 0;
        for seed in 0..8u64 {
            let mut sim = Simulator::with_seed(n, seed);
            let rec = executor.run_shot(&mut sim).unwrap();
            let accepted = (0..m).all(|id| rec.get(id) == Some(false));
            if !accepted {
                continue;
            }
            accepted_shots += 1;
            for (i, stabilizer) in stabilizers.iter().enumerate() {
                let value = sim.peek_observable_expectation(stabilizer).unwrap();
                assert!(
                    (value - 1.0).abs() < TOL,
                    "seed {seed}: stabilizer {i} expectation {value}, want +1"
                );
            }
            let ex = sim.peek_observable_expectation(&x_bar).unwrap();
            let ey = sim.peek_observable_expectation(&y_bar).unwrap();
            let ez = sim.peek_observable_expectation(&z_bar).unwrap();
            let inv_sqrt2 = std::f64::consts::FRAC_1_SQRT_2;
            assert!(
                (ex - inv_sqrt2).abs() < TOL && (ey - inv_sqrt2).abs() < TOL && ez.abs() < TOL,
                "seed {seed}: delivered Bloch ({ex:+.4}, {ey:+.4}, {ez:+.4}), \
                 want exactly |T̄⟩ = (+1/√2, +1/√2, 0)"
            );
        }
        assert!(
            accepted_shots > 0,
            "no accepted cultivation shot across the seed batch"
        );
    }

    /// Every stage chunk's hand-ported flows hold semantically (stim
    /// `has_all_flows`) under all eight variants and across distances.
    #[rstest]
    fn stage_chunks_verify_flows_per_variant(
        #[values(
            Direction::YMINUS,
            Direction::XPLUS,
            Direction::YPLUS,
            Direction::XMINUS
        )]
        side: Direction,
        #[values(false, true)] dual: bool,
        #[values(3, 7)] distance: u32,
    ) {
        let layout = family_layout(side, dual, distance);
        for (stage, chunk) in [
            ("injection", injection_chunk(&layout).unwrap()),
            ("syndrome", syndrome_round_chunk(&layout).unwrap()),
            ("check", check_chunk(&layout).unwrap()),
        ] {
            assert!(
                !chunk.circuit.has_moments_conflict(),
                "{stage}: qubit reused within a moment"
            );
            chunk
                .verify_flows(None, None)
                .unwrap_or_else(|error| panic!("{stage} flows fail stim verification: {error}"));
        }
    }

    /// Each stage circuit keeps one op kind per tick (editor-clean display;
    /// the msc-ls originals mix resets and measurements into CX ticks).
    #[test]
    fn stage_ticks_are_kind_homogeneous() {
        let layout = layout(Direction::YMINUS, 3);
        for (stage, chunk) in [
            ("injection", injection_chunk(&layout).unwrap()),
            ("syndrome", syndrome_round_chunk(&layout).unwrap()),
            ("check", check_chunk(&layout).unwrap()),
        ] {
            crate::block::fixed_bulk::t::assert_ticks_are_kind_homogeneous(stage, &chunk.circuit);
        }
    }

    /// Structural counts are variant-invariant: the D₄ transform and the
    /// merge-basis dual never change the cultivation circuit's shape.
    #[rstest]
    fn stage_counts_are_variant_invariant(
        #[values(
            Direction::YMINUS,
            Direction::XPLUS,
            Direction::YPLUS,
            Direction::XMINUS
        )]
        side: Direction,
        #[values(Basis::X, Basis::Z)] x_basis: Basis,
    ) {
        let layout = SurgeryLayout::new(side, x_basis, x_basis.flip(), 3);

        let injection = injection_chunk(&layout).unwrap();
        assert_eq!(injection.circuit.num_measurements(), 0);
        // 6 stabilizer creators + the logical anchor.
        assert_eq!(injection.flows.len(), 7);

        let syndrome = syndrome_round_chunk(&layout).unwrap();
        assert_eq!(syndrome.circuit.num_measurements(), 8);
        // 6 consumer + 6 creator + the qubit-1 move byproduct + the anchor; the
        // qubit-2 byproduct is not standalone (no mid-round re-init).
        assert_eq!(syndrome.flows.len(), 14);

        let check = check_chunk(&layout).unwrap();
        assert_eq!(check.circuit.num_measurements(), 7);
        // 6 pass-throughs + 6 local restarts + the anchor readout.
        assert_eq!(check.flows.len(), 13);

        for chunk in [&injection, &syndrome, &check] {
            assert!(
                chunk
                    .flows
                    .iter()
                    .all(|flow| flow.marker == FlowMarker::Restart),
                "every cultivation flow is post-selected"
            );
        }
    }

    /// Composing the three stages into a template routes every post-selected
    /// parity into the `restarts` side table (no ordinary detectors) and
    /// leaves the six Steane stabilizers open — restart-flagged — for the
    /// escape template.
    #[test]
    fn cultivation_template_restarts_match_msc_ls_post_selection() {
        let layout = layout(Direction::YMINUS, 3);
        let template = cultivation_template(&layout).expect("cultivation stages compose");
        let program = &template.program_template;

        assert!(
            program.detectors.is_empty(),
            "cultivation emits no decoder-fed detectors"
        );

        // Template measurement ids: syndrome round 0..=7, check 8..=14.
        // Syndrome parities are msc-ls's 8 singles minus the standalone
        // `MX(5,11)` (id 7), which folds into the 0235 face closure (id 3)
        // now that the mid-round re-init is gone. Check parities are the 5
        // standalone ancilla singles, the flag⊗MX(4,12) pair, and the logical
        // anchor's closure — which carries ids 1, 4 (the Z-face ancillas its
        // transport rides) and 7 (the shuttle byproduct). Ids 1 and 4 are also
        // declared alone, so the closure still spans msc-ls's standalone flag
        // `MX(3,13)` up to id 7, the one deviation
        // (`escape::tests::restart_set_spans_msc_ls_post_selection`).
        let expected: crate::FxSet<DetectorParity> = [
            &[0][..],
            &[1],
            &[2],
            &[3, 7],
            &[4],
            &[5],
            &[6],
            &[9],
            &[11],
            &[12],
            &[13],
            &[14],
            &[8, 10],
            &[1, 4, 7, 8],
        ]
        .into_iter()
        .map(|parity| DetectorParity::from_measurements(parity.iter().copied()))
        .collect();
        let actual: crate::FxSet<DetectorParity> = program
            .restarts
            .iter()
            .map(|restart| restart.parity.clone())
            .collect();
        assert_eq!(actual, expected);

        // Boundary: exactly the six stabilizer chains, all restart-flagged,
        // each entering the escape template with its accumulated syndrome
        // history (the flag, template id 8, rides every X face).
        assert_eq!(program.boundary_flows.len(), 6);
        let ends: crate::FxSet<PauliMap> = program
            .boundary_flows
            .iter()
            .map(|flow| flow.end.clone())
            .collect();
        let expected_ends: crate::FxSet<PauliMap> =
            stabilizer_supports(&layout, &STEANE_FINAL).collect();
        assert_eq!(ends, expected_ends);
        for flow in &program.boundary_flows {
            assert!(flow.start.is_empty());
            assert_eq!(flow.marker, FlowMarker::Restart);
        }
        let x0145: PauliMap = FACES[0]
            .iter()
            .map(|&qubit| (layout.map_msc(STEANE_FINAL[qubit]), Pauli::X))
            .collect();
        let flag_chain = program
            .boundary_flows
            .iter()
            .find(|flow| flow.end == x0145)
            .expect("X0145 stays open");
        let mut measurements = flag_chain.measurements.to_vec();
        measurements.sort_unstable();
        assert_eq!(measurements, [0, 8], "syndrome MX(1,13) plus the flag");
    }

    /// The rotated variants place the Steane spill outside the patch on the
    /// chosen side (the cell the placement pass must reserve).
    #[rstest]
    #[case(Direction::YMINUS, IVec2::NEG_Y)]
    #[case(Direction::XPLUS, IVec2::X)]
    #[case(Direction::YPLUS, IVec2::Y)]
    #[case(Direction::XMINUS, IVec2::NEG_X)]
    fn steane_spill_lands_on_chosen_side(
        #[case] side: Direction,
        #[case] outward: IVec2,
        #[values(false, true)] dual: bool,
    ) {
        let distance = 3;
        let layout = family_layout(side, dual, distance);
        let patch_max = 2 * distance as i32 - 1;
        for position in STEANE_FINAL {
            let mapped = layout.map_msc(position);
            let out_of_patch =
                mapped.x < 1 || mapped.x > patch_max || mapped.y < 1 || mapped.y > patch_max;
            assert!(
                out_of_patch,
                "Steane qubit {position:?} maps into the patch"
            );
            let overshoot = mapped - IVec2::splat(distance as i32);
            assert!(
                overshoot.dot(outward) > 0,
                "Steane qubit {position:?} spills toward the wrong side"
            );
        }
    }
}
