//! Lowering one node's [`NodeEmissionPlan`] to standalone Stim text.
//!
//! [`emit_bloq_stim`](crate::emit_bloq_stim) linearizes a whole program into
//! one global qubit and measurement space. A caller assembling a circuit of
//! its own — splicing a compiled T source into a larger sampling circuit,
//! rendering one node against a *different* program's layout — needs the
//! opposite: a single plan, addressed through a qubit map it chose, with the
//! measurement columns reported back so its own indices line up.
//!
//! Two things separate this from the program emitter. `REPEAT` blocks are
//! **unrolled**, because the columns this returns are absolute positions in
//! the emitted record stream and a loop has no single position. And feedback
//! Paulis are written as their X/Z factors rather than as `CY` (see
//! [`emit_plan_stim`]).

use std::collections::BTreeMap;

use bloq_circuit::{
    BodyId, CircuitError, ConditionalCorrection, CoordCircuit, GateType, MeasurementFrameError, Op,
    Pauli, PauliBasis, PauliMap, measurement_gate_name,
};
use bloq_ir::lowering::{InstanceMeasurement, NodeEmissionPlan};
use glam::IVec2;
use rustc_hash::FxHashMap;

use crate::dialect::{HONEST_T_TAG, StimDialect};
use crate::emit::{StimEmissionError, conditional_pauli_gate_name, pauli_error_gate_name};
use crate::layout::QubitLayout;
use crate::measurement_frame::{MAX_STIM_RECORD_LOOKBACK, stim_record_lookback};
use crate::text_utils::{push_int, push_number_prefer_int, write_stim_tag};

/// How [`emit_plan_stim`] should render a plan.
#[derive(Debug, Clone, Copy)]
pub struct PlanStimOptions<'a> {
    qubit_map: &'a FxHashMap<IVec2, u32>,
    dialect: StimDialect,
    tag: Option<&'a str>,
}

impl<'a> PlanStimOptions<'a> {
    /// Render against `qubit_map`, in the default [`StimDialect`] and with no
    /// tag.
    ///
    /// `qubit_map` addresses the emitted circuit: every qubit coordinate the
    /// plan touches must appear in it, and the index it maps to is the qubit
    /// number written out. Passing the caller's own global layout is what lets
    /// several plans be emitted into one shared index space.
    /// Sparse indices in `0..=16_777_215` are supported without allocating entries
    /// for missing indices. Both dialects share Stim's 24-bit qubit targets.
    pub fn new(qubit_map: &'a FxHashMap<IVec2, u32>) -> Self {
        Self {
            qubit_map,
            dialect: StimDialect::default(),
            tag: None,
        }
    }

    /// Choose how non-Clifford `T` gates are spelled.
    #[must_use]
    pub fn with_dialect(mut self, dialect: StimDialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// Tag every emitted instruction with `tag`, so the caller can find this
    /// plan's slice of a larger circuit structurally instead of by position.
    ///
    /// The `HONEST_T` tag wins on the gates it marks: it is load-bearing for
    /// the [`StimDialect`] round trip, whereas this tag is the caller's own
    /// bookkeeping.
    #[must_use]
    pub fn with_tag(mut self, tag: &'a str) -> Self {
        self.tag = Some(tag);
        self
    }
}

/// One lowered plan: its Stim text and where its measurements landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStim {
    /// The plan's Stim text, with every `REPEAT` unrolled.
    pub text: String,
    /// The absolute column each of the plan's instance-space measurements
    /// occupies in [`Self::text`]'s record stream.
    ///
    /// Relative to the start of this text: a caller splicing it at offset `n`
    /// adds `n` to every column.
    pub measurement_columns: BTreeMap<InstanceMeasurement, u32>,
    /// How many measurement records [`Self::text`] emits — the offset a
    /// following fragment starts at.
    pub measurement_count: u32,
}

/// Lower one node's emission plan to standalone Stim text.
///
/// Every [`Op`] variant is handled, so a plan is never rejected for containing
/// an operation this does not know about. Two conventions are worth naming
/// because they are not the only defensible choices:
///
/// - **`REPEAT` is unrolled.** [`Self::measurement_columns`] are absolute
///   positions, and a measurement inside a loop occupies a different one every
///   iteration. Unrolling is what makes the map well defined; the cost is that
///   a plan with a large repetition count produces proportionally large text.
/// - **A feedback `Y` is written as `CX` then `CZ`, not `CY`.** The control is
///   a classical record, so the phase difference between `Y` and `XZ` is
///   constant across the whole branch and cannot be observed. This matches the
///   convention the artifacts already in the field were built with, and
///   deliberately differs from [`emit_bloq_stim`](crate::emit_bloq_stim),
///   whose text is pinned by its own consumers.
///
/// [`Self::measurement_columns`]: PlanStim::measurement_columns
///
/// # Errors
///
/// - [`StimEmissionError::Circuit`] if a plan qubit is missing from the qubit
///   map, a body reference dangles or recurses, or a record lookback leaves
///   Stim's range.
/// - [`StimEmissionError::QubitIndexOutOfRange`] if a caller-supplied qubit index
///   exceeds `16_777_215`.
/// - [`StimEmissionError::UnsupportedGate`] for a non-Clifford gate other than
///   `T`/`T_DAG` in the [`StimDialect::Stim`] dialect, which Stim could not
///   parse.
/// - [`StimEmissionError::MalformedGraph`] if a feedback control is read
///   before its measurement is emitted, or the plan names a measurement its
///   own circuit never emits.
///
/// # Panics
///
/// In debug builds, panics if plan emission disagrees with the circuit column walk.
pub fn emit_plan_stim(
    plan: &NodeEmissionPlan,
    options: &PlanStimOptions<'_>,
) -> Result<PlanStim, StimEmissionError> {
    let layout = QubitLayout::new(options.qubit_map.clone())?;
    let mut emitter = PlanStimEmitter {
        circuit: &plan.circuit,
        layout: &layout,
        dialect: options.dialect,
        tag: options.tag,
        text: String::new(),
        columns: FxHashMap::default(),
        count: 0,
        stack: Vec::new(),
    };
    emitter.emit_body(plan.circuit.entry_body())?;

    // The standalone column walk and this emitter unroll the same circuit by
    // different routes (closed-form there, literal repetition here). They can
    // only disagree if one of them is wrong, so cross-check them rather than
    // let a divergence surface as a misaligned artifact.
    debug_assert_eq!(
        plan.circuit
            .expanded_measurement_columns()
            .map(|expanded| expanded.count()),
        Ok(emitter.count),
        "plan lowering and the column walk disagree on the record count",
    );

    let measurement_columns =
        plan.measurements
            .iter()
            .map(|(&measurement, local)| {
                let column = emitter.columns.get(local).copied().ok_or(
                    StimEmissionError::MalformedGraph(
                        "emission plan names a measurement its circuit never emits",
                    ),
                )?;
                Ok((measurement, column))
            })
            .collect::<Result<BTreeMap<_, _>, StimEmissionError>>()?;

    Ok(PlanStim {
        text: emitter.text,
        measurement_columns,
        measurement_count: emitter.count,
    })
}

struct PlanStimEmitter<'a> {
    circuit: &'a CoordCircuit,
    layout: &'a QubitLayout,
    dialect: StimDialect,
    tag: Option<&'a str>,
    text: String,
    /// Emitted column of each measurement id's latest occurrence so far —
    /// what a feedback control resolves against, and what the finished map is
    /// built from.
    /// Unlike whole-program emission, standalone plans may retain sparse local
    /// ids near u32::MAX. A dense MeasurementFrame would allocate by that id.
    columns: FxHashMap<u32, u32>,
    count: u32,
    /// Bodies currently being unrolled, so a self-referential body is reported
    /// instead of recursing forever.
    stack: Vec<BodyId>,
}

impl PlanStimEmitter<'_> {
    fn emit_body(&mut self, body: BodyId) -> Result<(), StimEmissionError> {
        if self.stack.contains(&body) {
            return Err(CircuitError::InvalidCircuitBody(body).into());
        }
        self.stack.push(body);
        // Copied out so the op borrow is on the circuit, not on `self`.
        let circuit = self.circuit;
        let source = circuit
            .body(body)
            .ok_or(CircuitError::InvalidCircuitBody(body))?;
        for op in source.ops() {
            self.emit_op(op)?;
        }
        self.stack.pop();
        Ok(())
    }

    fn emit_op(&mut self, op: &Op) -> Result<(), StimEmissionError> {
        match op {
            Op::Gate { gate, qubits } => self.emit_gate(*gate, qubits),
            Op::Measure {
                basis,
                qubits,
                measurements,
                flip_probability,
            } => {
                self.record_measurements(measurements)?;
                let arg = (*flip_probability != 0.0).then_some(*flip_probability);
                self.write_qubit_line(measurement_gate_name(*basis), self.tag, arg, qubits)
            }
            Op::MPP {
                products,
                measurements,
            } => {
                self.record_measurements(measurements)?;
                self.emit_mpp(products)
            }
            Op::ConditionalPauli(corrections) => self.emit_conditional_paulis(corrections),
            Op::Depolarize1 {
                probability,
                qubits,
            } => self.write_qubit_line("DEPOLARIZE1", self.tag, Some(*probability), qubits),
            Op::Depolarize2 {
                probability,
                qubits,
            } => self.write_qubit_line("DEPOLARIZE2", self.tag, Some(*probability), qubits),
            Op::PauliError {
                probability,
                pauli,
                qubits,
            } => self.write_qubit_line(
                pauli_error_gate_name(*pauli),
                self.tag,
                Some(*probability),
                qubits,
            ),
            Op::Tick => self.write_qubit_line("TICK", self.tag, None, &[]),
            Op::Repeat { body, repetitions } => {
                for _ in 0..*repetitions {
                    self.emit_body(*body)?;
                }
                Ok(())
            }
        }
    }

    fn emit_gate(&mut self, gate: GateType, qubits: &[IVec2]) -> Result<(), StimEmissionError> {
        let (name, tag) = match (self.dialect, gate) {
            (StimDialect::Stim, GateType::T) => ("S", Some(HONEST_T_TAG)),
            (StimDialect::Stim, GateType::T_DAG) => ("S_DAG", Some(HONEST_T_TAG)),
            // The other non-Cliffords have no Stim spelling at all — not even
            // a tagged proxy — so emitting them would produce text Stim
            // rejects. The Clifft dialect writes them literally.
            (StimDialect::Stim, gate) if gate.is_non_clifford() => {
                return Err(StimEmissionError::UnsupportedGate(gate));
            }
            (_, gate) => (gate.into(), self.tag),
        };
        self.write_qubit_line(name, tag, None, qubits)
    }

    /// One `MPP X0*X1 Z2*Z3` line: each product's Paulis joined by `*`,
    /// products separated by a space, in the products' own coordinate order.
    /// A plan with no Pauli targets writes nothing rather than a bare `MPP`,
    /// which Stim rejects.
    fn emit_mpp(&mut self, products: &[PauliMap]) -> Result<(), StimEmissionError> {
        let mut targets = String::new();
        for product in products {
            let mut first = true;
            for (coord, pauli) in product.iter().filter(|&(_, &pauli)| pauli != Pauli::I) {
                let qubit = self.qubit(*coord)?;
                targets.push(if first { ' ' } else { '*' });
                first = false;
                targets.push(match pauli {
                    Pauli::X => 'X',
                    Pauli::Y => 'Y',
                    Pauli::Z => 'Z',
                    Pauli::I => unreachable!("identity Paulis are filtered out above"),
                });
                targets.push_str(self.layout.label(qubit));
            }
        }
        if targets.is_empty() {
            return Ok(());
        }
        self.write_instruction_prefix("MPP", self.tag, None);
        self.text.push_str(&targets);
        self.text.push('\n');
        Ok(())
    }

    fn emit_conditional_paulis(
        &mut self,
        corrections: &[ConditionalCorrection],
    ) -> Result<(), StimEmissionError> {
        for correction in corrections {
            let lookback = self.lookback_to(correction.control)?;
            let qubit = self.qubit(correction.target)?;
            match correction.pauli {
                // A record-controlled `Y` is written as its `X` and `Z`
                // factors. The control is classical, so the phase separating
                // `Y` from `XZ` is constant over the branch and unobservable —
                // and the artifacts this reproduces were built this way.
                PauliBasis::Y => {
                    self.write_feedback("CX", lookback, qubit);
                    self.write_feedback("CZ", lookback, qubit);
                }
                pauli => self.write_feedback(conditional_pauli_gate_name(pauli), lookback, qubit),
            }
        }
        Ok(())
    }

    /// Claim the next columns for `measurements`, in order. A repeated
    /// occurrence overwrites the earlier one, so a control or a plan lookup
    /// always resolves to the latest — the rule the emission frame and
    /// `CoordCircuit::flatten` both use.
    fn record_measurements(&mut self, measurements: &[u32]) -> Result<(), StimEmissionError> {
        for &measurement in measurements {
            let next = self.count.checked_add(1).ok_or_else(|| {
                CircuitError::from(MeasurementFrameError::EmittedMeasurementCountOverflow {
                    emitted_count: self.count,
                    additional: 1,
                })
            })?;
            self.columns.insert(measurement, self.count);
            self.count = next;
        }
        Ok(())
    }

    /// The `rec[-k]` offset from the current position back to `measurement`'s
    /// latest occurrence.
    fn lookback_to(&self, measurement: u32) -> Result<i32, StimEmissionError> {
        let column =
            self.columns
                .get(&measurement)
                .copied()
                .ok_or(StimEmissionError::MalformedGraph(
                    "conditional Pauli control is read before its measurement is emitted",
                ))?;
        let distance = self.count - column;
        stim_record_lookback(distance).ok_or_else(|| {
            CircuitError::from(MeasurementFrameError::RecordLookbackOutOfRange {
                lookback: distance,
                max: MAX_STIM_RECORD_LOOKBACK,
            })
            .into()
        })
    }

    fn qubit(&self, coord: IVec2) -> Result<u32, StimEmissionError> {
        self.layout
            .get(coord)
            .ok_or_else(|| CircuitError::QubitNotFoundInLayout(coord).into())
    }

    fn write_feedback(&mut self, gate: &str, lookback: i32, qubit: u32) {
        self.write_instruction_prefix(gate, self.tag, None);
        self.text.push_str(" rec[");
        push_int(&mut self.text, lookback);
        self.text.push_str("] ");
        self.text.push_str(self.layout.label(qubit));
        self.text.push('\n');
    }

    fn write_qubit_line(
        &mut self,
        gate: &str,
        tag: Option<&str>,
        arg: Option<f64>,
        qubits: &[IVec2],
    ) -> Result<(), StimEmissionError> {
        // Resolved before any text is written so a missing qubit fails without
        // leaving a half-finished line behind.
        let indices = qubits
            .iter()
            .map(|&coord| self.qubit(coord))
            .collect::<Result<Vec<_>, _>>()?;
        self.write_instruction_prefix(gate, tag, arg);
        for qubit in indices {
            self.text.push(' ');
            self.text.push_str(self.layout.label(qubit));
        }
        self.text.push('\n');
        Ok(())
    }

    fn write_instruction_prefix(&mut self, gate: &str, tag: Option<&str>, arg: Option<f64>) {
        self.text.push_str(gate);
        if let Some(tag) = tag {
            write_stim_tag(&mut self.text, tag);
        }
        if let Some(arg) = arg {
            self.text.push('(');
            push_number_prefer_int(&mut self.text, arg);
            self.text.push(')');
        }
    }
}

#[cfg(test)]
mod tests {
    use bloq_circuit::CircuitBody;
    use bloq_ir::lowering::TemplateInstanceId;
    use glam::ivec2;

    use super::*;

    fn instance_measurement(measurement: u32) -> InstanceMeasurement {
        InstanceMeasurement {
            instance: TemplateInstanceId(0),
            measurement,
        }
    }

    fn plan_of(circuit: CoordCircuit, measurements: &[u32]) -> NodeEmissionPlan {
        NodeEmissionPlan {
            normalized_templates: None,
            circuit,
            measurements: measurements
                .iter()
                .map(|&id| (instance_measurement(id), id))
                .collect(),
            bodies: FxHashMap::default(),
        }
    }

    fn layout_of(circuit: &CoordCircuit) -> FxHashMap<IVec2, u32> {
        circuit.build_coord_to_index()
    }

    /// One plan exercising every [`Op`] variant, so the lowering's
    /// exhaustiveness is a fact rather than a claim.
    fn every_op_circuit() -> (CoordCircuit, Vec<u32>) {
        let [a, b] = [ivec2(0, 0), ivec2(1, 0)];
        let mut circuit = CoordCircuit::new();
        circuit.do_gate(GateType::RZ, [a, b]).unwrap();
        circuit.tick();
        circuit.do_gate(GateType::CX, [a, b]).unwrap();
        circuit
            .body_mut(circuit.entry_body())
            .unwrap()
            .ops_mut()
            .extend([
                Op::Depolarize1 {
                    probability: 0.001,
                    qubits: vec![a],
                },
                Op::Depolarize2 {
                    probability: 0.002,
                    qubits: vec![a, b],
                },
                Op::PauliError {
                    probability: 0.003,
                    pauli: PauliBasis::Z,
                    qubits: vec![b],
                },
            ]);
        let mpp = circuit
            .measure_pauli_products([PauliMap::from_unique_entries([
                (a, Pauli::X),
                (b, Pauli::X),
            ])])
            .unwrap();
        let measured = circuit.measure(PauliBasis::Z, [a])[0];
        circuit
            .body_mut(circuit.entry_body())
            .unwrap()
            .ops_mut()
            .push(Op::ConditionalPauli(vec![
                ConditionalCorrection {
                    pauli: PauliBasis::X,
                    control: measured,
                    target: b,
                },
                ConditionalCorrection {
                    pauli: PauliBasis::Y,
                    control: mpp[0],
                    target: b,
                },
            ]));
        let looped = circuit.reserve_measurement_id(b);
        let body = circuit.add_body(CircuitBody::from_ops(vec![
            Op::Gate {
                gate: GateType::T,
                qubits: vec![a],
            },
            Op::Measure {
                basis: PauliBasis::X,
                qubits: vec![b],
                measurements: vec![looped],
                flip_probability: 0.004,
            },
        ]));
        circuit.push_repeat(body, 2);
        (circuit, vec![mpp[0], measured, looped])
    }

    #[test]
    fn every_op_lowers_and_unrolls_its_repeat() {
        let (circuit, measurements) = every_op_circuit();
        let qubit_map = layout_of(&circuit);
        let plan = plan_of(circuit, &measurements);

        let lowered = emit_plan_stim(&plan, &PlanStimOptions::new(&qubit_map)).unwrap();

        insta::assert_snapshot!(lowered.text, @r"
        R 0 1
        TICK
        CX 0 1
        DEPOLARIZE1(0.001) 0
        DEPOLARIZE2(0.002) 0 1
        Z_ERROR(0.003) 1
        MPP X0*X1
        M 0
        CX rec[-1] 1
        CX rec[-2] 1
        CZ rec[-2] 1
        S[HONEST_T] 0
        MX(0.004) 1
        S[HONEST_T] 0
        MX(0.004) 1
        ");
        // Two loop iterations of one measurement each, on top of the MPP and
        // the plain measurement.
        assert_eq!(lowered.measurement_count, 4);
        assert_eq!(
            lowered.measurement_columns[&instance_measurement(measurements[2])],
            3,
            "a looped measurement reports its final occurrence",
        );
    }

    /// The dialects differ only in how `T` is spelled, so converting one into
    /// the other must reproduce it exactly.
    #[test]
    fn the_two_dialects_are_related_by_the_text_codec() {
        let (circuit, measurements) = every_op_circuit();
        let qubit_map = layout_of(&circuit);
        let plan = plan_of(circuit, &measurements);
        let options = PlanStimOptions::new(&qubit_map);

        let as_stim = emit_plan_stim(&plan, &options).unwrap();
        let as_clifft = emit_plan_stim(&plan, &options.with_dialect(StimDialect::Clifft)).unwrap();

        assert!(as_clifft.text.contains("T 0"), "{}", as_clifft.text);
        assert_eq!(crate::stim_to_clifft_text(&as_stim.text), as_clifft.text);
        assert_eq!(crate::clifft_to_stim_text(&as_clifft.text), as_stim.text);
        assert_eq!(as_stim.measurement_columns, as_clifft.measurement_columns);
    }

    #[test]
    fn plan_columns_agree_with_the_standalone_column_walk() {
        let (circuit, measurements) = every_op_circuit();
        let qubit_map = layout_of(&circuit);
        let expanded = circuit.expanded_measurement_columns().unwrap();
        let plan = plan_of(circuit, &measurements);

        let lowered = emit_plan_stim(&plan, &PlanStimOptions::new(&qubit_map)).unwrap();

        assert_eq!(lowered.measurement_count, expanded.count());
        for (measurement, column) in &lowered.measurement_columns {
            assert_eq!(expanded.column(measurement.measurement), Some(*column));
        }
    }

    #[test]
    fn a_caller_tag_marks_every_instruction_but_yields_to_honest_t() {
        let (circuit, measurements) = every_op_circuit();
        let qubit_map = layout_of(&circuit);
        let plan = plan_of(circuit, &measurements);

        let lowered = emit_plan_stim(
            &plan,
            &PlanStimOptions::new(&qubit_map).with_tag("T_SOURCE"),
        )
        .unwrap();

        assert!(lowered.text.contains("TICK[T_SOURCE]"));
        assert!(lowered.text.contains("MPP[T_SOURCE] X0*X1"));
        assert!(lowered.text.contains("CX[T_SOURCE] rec[-1] 1"));
        assert!(
            lowered.text.contains("S[HONEST_T] 0"),
            "the dialect tag is load-bearing and wins: {}",
            lowered.text
        );
    }

    #[test]
    fn a_qubit_outside_the_map_is_reported_rather_than_emitted() {
        let mut circuit = CoordCircuit::new();
        circuit.do_gate(GateType::H, [ivec2(4, 4)]).unwrap();
        let plan = plan_of(circuit, &[]);
        let qubit_map = FxHashMap::from_iter([(ivec2(0, 0), 0)]);

        assert!(matches!(
            emit_plan_stim(&plan, &PlanStimOptions::new(&qubit_map)),
            Err(StimEmissionError::Circuit(CircuitError::QubitNotFoundInLayout(
                coord
            ))) if coord == ivec2(4, 4)
        ));
    }

    /// Stim has no spelling at all for the off-axis non-Cliffords, so the
    /// dialect that has to parse must refuse them.
    #[test]
    fn stim_dialect_rejects_a_non_clifford_without_an_s_proxy() {
        let qubit = ivec2(0, 0);
        let mut circuit = CoordCircuit::new();
        circuit.do_gate(GateType::T_XZ, [qubit]).unwrap();
        let qubit_map = layout_of(&circuit);
        let plan = plan_of(circuit, &[]);

        assert!(matches!(
            emit_plan_stim(&plan, &PlanStimOptions::new(&qubit_map)),
            Err(StimEmissionError::UnsupportedGate(GateType::T_XZ))
        ));
        let clifft = PlanStimOptions::new(&qubit_map).with_dialect(StimDialect::Clifft);
        assert_eq!(emit_plan_stim(&plan, &clifft).unwrap().text, "T_XZ 0\n");
    }

    #[test]
    fn a_feedback_control_read_before_its_measurement_is_reported() {
        let qubit = ivec2(0, 0);
        let mut circuit = CoordCircuit::new();
        let measurement = circuit.reserve_measurement_id(qubit);
        circuit
            .body_mut(circuit.entry_body())
            .unwrap()
            .ops_mut()
            .push(Op::ConditionalPauli(vec![ConditionalCorrection {
                pauli: PauliBasis::X,
                control: measurement,
                target: qubit,
            }]));
        let qubit_map = layout_of(&circuit);
        let plan = plan_of(circuit, &[]);

        assert!(matches!(
            emit_plan_stim(&plan, &PlanStimOptions::new(&qubit_map)),
            Err(StimEmissionError::MalformedGraph(_))
        ));
    }

    #[test]
    fn feedback_lookbacks_obey_stims_limit() {
        let circuit = CoordCircuit::new();
        let layout = QubitLayout::default();
        let emitter = PlanStimEmitter {
            circuit: &circuit,
            layout: &layout,
            dialect: StimDialect::Stim,
            tag: None,
            text: String::new(),
            columns: FxHashMap::from_iter([(0, 0)]),
            count: u32::MAX,
            stack: Vec::new(),
        };

        assert!(matches!(
            emitter.lookback_to(0),
            Err(StimEmissionError::Circuit(CircuitError::MeasurementFrame(
                MeasurementFrameError::RecordLookbackOutOfRange {
                    lookback,
                    max: MAX_STIM_RECORD_LOOKBACK,
                }
            ))) if lookback == u32::MAX
        ));
    }

    #[test]
    fn standalone_plan_rejects_qubit_indices_outside_stim_target_payload() {
        let qubit = ivec2(0, 0);
        let mut circuit = CoordCircuit::new();
        circuit.do_gate(GateType::H, [qubit]).unwrap();
        let plan = plan_of(circuit, &[]);
        for index in [1 << 24, u32::MAX] {
            let qubit_map = FxHashMap::from_iter([(qubit, index)]);
            for dialect in [StimDialect::Stim, StimDialect::Clifft] {
                let options = PlanStimOptions::new(&qubit_map).with_dialect(dialect);
                assert_eq!(
                    emit_plan_stim(&plan, &options),
                    Err(StimEmissionError::QubitIndexOutOfRange {
                        index: index as usize,
                        max: 16_777_215,
                    })
                );
            }
        }
    }

    #[test]
    fn standalone_plan_preserves_sparse_caller_qubit_indices() {
        let qubit = ivec2(0, 0);
        let mut circuit = CoordCircuit::new();
        circuit.do_gate(GateType::H, [qubit]).unwrap();
        let plan = plan_of(circuit, &[]);
        for index in [100_000, 16_777_215] {
            let qubit_map = FxHashMap::from_iter([(qubit, index)]);
            let output = emit_plan_stim(&plan, &PlanStimOptions::new(&qubit_map)).unwrap();
            assert_eq!(output.text, format!("H {index}\n"));
        }
    }

    #[test]
    fn standalone_plan_emits_sparse_measurement_ids_without_dense_allocation() {
        let qubit = ivec2(0, 0);
        let measurement = u32::MAX - 1;
        let mut circuit = CoordCircuit::new();
        circuit.register_measurement_id(measurement, qubit);
        circuit
            .body_mut(circuit.entry_body())
            .unwrap()
            .ops_mut()
            .extend([
                Op::Measure {
                    basis: PauliBasis::Z,
                    qubits: vec![qubit],
                    measurements: vec![measurement],
                    flip_probability: 0.0,
                },
                Op::ConditionalPauli(vec![ConditionalCorrection {
                    pauli: PauliBasis::X,
                    control: measurement,
                    target: qubit,
                }]),
            ]);
        bloq_ir::lowering::validate_template_circuit(&circuit).unwrap();
        let layout = layout_of(&circuit);
        let plan = plan_of(circuit, &[measurement]);

        let emitted = emit_plan_stim(&plan, &PlanStimOptions::new(&layout)).unwrap();

        assert_eq!(emitted.text, "M 0\nCX rec[-1] 0\n");
        assert_eq!(emitted.measurement_count, 1);
        assert_eq!(
            emitted.measurement_columns[&instance_measurement(measurement)],
            0
        );
    }

    #[cfg(feature = "verify")]
    #[test]
    fn arbitrary_caller_tags_round_trip_through_stim() {
        let mut circuit = CoordCircuit::new();
        circuit.do_gate(GateType::H, [ivec2(0, 0)]).unwrap();
        let qubit_map = layout_of(&circuit);
        let plan = plan_of(circuit, &[]);
        let tag = "a]b\\c\nd\rλ";
        let lowered =
            emit_plan_stim(&plan, &PlanStimOptions::new(&qubit_map).with_tag(tag)).unwrap();
        let parsed: stim::Circuit = lowered.text.parse().unwrap();
        let instructions = parsed.into_iter().collect::<Vec<_>>();
        let [stim::CircuitItem::Instruction(instruction)] = instructions.as_slice() else {
            panic!("one tagged gate expected");
        };
        assert_eq!(instruction.tag(), tag);
    }

    #[cfg(feature = "verify")]
    #[test]
    fn lowered_text_parses_as_stim_with_the_reported_record_count() {
        let (circuit, measurements) = every_op_circuit();
        let qubit_map = layout_of(&circuit);
        let plan = plan_of(circuit, &measurements);

        let lowered = emit_plan_stim(&plan, &PlanStimOptions::new(&qubit_map)).unwrap();

        let parsed: stim::Circuit = lowered.text.parse().expect("lowered text is valid Stim");
        assert_eq!(
            parsed.num_measurements(),
            u64::from(lowered.measurement_count)
        );
    }
}
