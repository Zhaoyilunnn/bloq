use glam::IVec2;
use itertools::Itertools;
use rustc_hash::FxHashMap;
use thiserror::Error;

use bloq_circuit::{Chunk, ChunkOrLoop, CircuitError, Flow, FlowError, PauliMap};

use crate::emit::{emit_detector_records, emit_observable_include_records, emit_standalone_stim};
use crate::layout::QubitLayout;
use crate::measurement_frame::MeasurementFrame;

/// Reason a chunk failed stim-based flow verification.
#[derive(Debug, Error)]
pub enum StimVerifyError {
    /// The static backend cannot represent the circuit's layout.
    #[error("{0}")]
    Emission(#[from] crate::StimEmissionError),
    /// Circuit construction or measurement resolution failed.
    #[error("{0}")]
    Circuit(#[from] CircuitError),
    /// Declared flow boundaries could not be matched consistently.
    #[error("{0}")]
    Flow(#[from] FlowError),
    /// A chunk's boundary does not match its expected interface.
    #[error("unmatched interface: {0}")]
    UnmatchedInterface(String),
    /// Stim confirmed the emitted circuit does not carry the chunk's declared
    /// flows; `flows` holds their string forms for diagnostics.
    #[error("the chunk does not have the following flows: {}", flows.join("; "))]
    ChunkDoesNotHaveFlows {
        /// Declared flows that Stim rejected.
        flows: Vec<String>,
    },
    /// A sub-chunk of a loop body failed verification; `index` is its position
    /// within the body.
    #[error("loop body sub-chunk {index} failed flow verification")]
    LoopChunk {
        /// Position of the failing chunk in the loop body.
        index: usize,
        /// Failure reported for that chunk.
        #[source]
        source: Box<StimVerifyError>,
    },
    /// Stim rejected emitted circuit or flow text.
    #[error("{0}")]
    Stim(#[from] stim::StimError),
}

/// Checks that a circuit fragment carries its declared stabilizer flows.
///
/// Implemented for [`Chunk`] and [`ChunkOrLoop`]: the fragment is emitted to Stim
/// text and each declared [`Flow`] is verified against it.
pub trait StimFlowVerifier {
    /// Verifies this fragment's flows, optionally pinning its boundary ports.
    ///
    /// `expected_in`/`expected_out` assert the start/end interface port sets
    /// (as multisets); `None` leaves that boundary unconstrained.
    ///
    /// # Errors
    ///
    /// Returns [`StimVerifyError`] if emission, parsing, or flow verification fails.
    fn verify_flows(
        &self,
        expected_in: Option<&[PauliMap]>,
        expected_out: Option<&[PauliMap]>,
    ) -> Result<(), StimVerifyError>;
}

impl StimFlowVerifier for ChunkOrLoop {
    fn verify_flows(
        &self,
        expected_in: Option<&[PauliMap]>,
        expected_out: Option<&[PauliMap]>,
    ) -> Result<(), StimVerifyError> {
        match self {
            ChunkOrLoop::Single(c) => c.verify_flows(expected_in, expected_out),
            ChunkOrLoop::Loop {
                body, repetitions, ..
            } => {
                if body.is_empty() {
                    return Ok(());
                }
                let n = body.len();

                let mut expected_ins: Vec<Option<Vec<PauliMap>>> =
                    body.iter().map(|c| Some(side_ports(c, FLOW_END))).collect();
                expected_ins.rotate_right(1);

                let mut expected_outs: Vec<Option<Vec<PauliMap>>> = body
                    .iter()
                    .map(|c| Some(side_ports(c, FLOW_START)))
                    .collect();
                expected_outs.rotate_left(1);

                if *repetitions <= 1 {
                    expected_ins[0] = None;
                    expected_outs[n - 1] = None;
                }
                if let Some(ext_in) = expected_in {
                    expected_ins[0] = Some(ext_in.to_vec());
                }
                if let Some(ext_out) = expected_out {
                    expected_outs[n - 1] = Some(ext_out.to_vec());
                }

                for (index, chunk) in body.iter().enumerate() {
                    chunk
                        .verify_flows(
                            expected_ins[index].as_deref(),
                            expected_outs[index].as_deref(),
                        )
                        .map_err(|error| StimVerifyError::LoopChunk {
                            index,
                            source: Box::new(error),
                        })?;
                }
                Ok(())
            }
        }
    }
}

impl StimFlowVerifier for Chunk {
    fn verify_flows(
        &self,
        expected_in: Option<&[PauliMap]>,
        expected_out: Option<&[PauliMap]>,
    ) -> Result<(), StimVerifyError> {
        let sides: [(&str, FlowSide, Option<&[PauliMap]>); 2] = [
            ("START", FLOW_START, expected_in),
            ("END", FLOW_END, expected_out),
        ];

        for (label, side, _) in sides {
            for (&interface, group) in self.flows.iter().into_group_map_by(|&f| side(f)).iter() {
                if !interface.is_empty() && group.len() > 1 {
                    return Err(FlowError::Composition(format!(
                        "multiple flows have the same {label} interface {interface}: {}",
                        group.iter().map(ToString::to_string).join("; ")
                    ))
                    .into());
                }
            }
        }
        for (label, side, expected) in sides {
            let Some(expected) = expected else { continue };
            let actual = side_ports(self, side);
            if ports_mismatch(expected, &actual) {
                return Err(StimVerifyError::UnmatchedInterface(format!(
                    "chunk {label} interface does not match; expected: {}; actual: {}",
                    expected.iter().map(ToString::to_string).join(", "),
                    actual.iter().map(ToString::to_string).join(", "),
                )));
            }
        }
        verify_stim_flows(self)
    }
}

/// Which end of a flow an interface check reads. START and END are checked
/// identically, so the side is a parameter rather than a duplicated code path.
type FlowSide = fn(&Flow) -> &PauliMap;
const FLOW_START: FlowSide = |flow| &flow.start;
const FLOW_END: FlowSide = |flow| &flow.end;

/// The non-empty interfaces on one side of every flow in `chunk`.
fn side_ports(chunk: &Chunk, side: FlowSide) -> Vec<PauliMap> {
    chunk
        .flows
        .iter()
        .filter_map(|flow| {
            let port = side(flow);
            (!port.is_empty()).then(|| port.clone())
        })
        .collect()
}

/// Whether the two port lists differ as multisets, via hash counting rather
/// than a quadratic scan (which would also wrongly accept a duplicated
/// expected port matched by a single actual one).
fn ports_mismatch(expected: &[PauliMap], actual: &[PauliMap]) -> bool {
    if expected.len() != actual.len() {
        return true;
    }
    let mut counts: FxHashMap<&PauliMap, usize> =
        FxHashMap::with_capacity_and_hasher(expected.len(), Default::default());
    for port in expected {
        *counts.entry(port).or_default() += 1;
    }
    actual.iter().any(|port| match counts.get_mut(port) {
        Some(count) if *count > 0 => {
            *count -= 1;
            false
        }
        _ => true,
    })
}

/// Emits a flat coordinate circuit as standalone Stim text.
///
/// Trailing `DETECTOR` and `OBSERVABLE_INCLUDE` lines resolve circuit
/// measurement ids to record lookbacks in emission order.
///
/// This is the escape-template distance-test entry point: restart parities are
/// emitted as ordinary `DETECTOR` lines — msc-ls-faithful, since its `.stim`
/// output carries every post-selected parity as a plain detector and keeps the
/// post-selection index list on the Python side. Loop bodies are unsupported
/// (`resolve_measurement` fails on unemitted ids); the T-block templates are
/// all flat [`Single`](ChunkOrLoop::Single) chunks.
///
/// # Errors
///
/// Returns [`StimVerifyError`] if circuit emission or measurement resolution fails.
pub fn emit_annotated_stim(
    circuit: &bloq_circuit::CoordCircuit,
    detectors: &[Vec<u32>],
    observables: &[(u32, Vec<u32>)],
) -> Result<String, StimVerifyError> {
    let (mut stim_text, frame) = emit_bare_stim(circuit)?;
    for parity in detectors {
        let lookbacks = resolve_lookbacks(&frame, parity)?;
        emit_detector_records(&mut stim_text, &lookbacks);
    }
    for (index, measurements) in observables {
        let lookbacks = resolve_lookbacks(&frame, measurements)?;
        emit_observable_include_records(&mut stim_text, *index, &lookbacks);
    }
    Ok(stim_text)
}

/// Resolve measurement ids to `rec[...]` lookbacks against the frame, sorted
/// like the program emitter's annotation lookbacks.
fn resolve_lookbacks(
    frame: &MeasurementFrame,
    measurements: &[u32],
) -> Result<Vec<i32>, CircuitError> {
    let mut lookbacks = measurements
        .iter()
        .map(|&measurement| frame.resolve_measurement(measurement))
        .collect::<Result<Vec<_>, CircuitError>>()?;
    lookbacks.sort_unstable();
    Ok(lookbacks)
}

fn emit_bare_stim(
    circuit: &bloq_circuit::CoordCircuit,
) -> Result<(String, MeasurementFrame), StimVerifyError> {
    let layout = QubitLayout::new(circuit.build_coord_to_index())?;
    Ok(emit_standalone_stim(circuit, None, &layout, true)?)
}

fn verify_stim_flows(chunk: &Chunk) -> Result<(), StimVerifyError> {
    let layout = chunk.circuit.build_coord_to_index();
    let (stim_text, frame) = emit_bare_stim(&chunk.circuit)?;
    let stim_circuit = stim_text.parse::<stim::Circuit>()?;
    let num_qubits = chunk.circuit.num_qubits();
    let flows = chunk
        .flows
        .iter()
        .map(|flow| flow_to_stim_flow(flow, &layout, num_qubits, &frame))
        .collect::<Result<Vec<_>, StimVerifyError>>()?;
    if !stim_circuit.has_all_flows(&flows, false)? {
        let mut missing = Vec::new();
        for (declared, flow) in chunk.flows.iter().zip(&flows) {
            if !stim_circuit.has_flow(flow, false)? {
                missing.push(format!("{declared}: {flow}"));
            }
        }
        return Err(StimVerifyError::ChunkDoesNotHaveFlows { flows: missing });
    }
    Ok(())
}

fn flow_to_stim_flow(
    flow: &Flow,
    layout: &FxHashMap<IVec2, u32>,
    num_qubits: u32,
    frame: &MeasurementFrame,
) -> Result<stim::Flow, StimVerifyError> {
    let lhs = flow_pauli_string(&flow.start, layout, num_qubits)?;
    let lhs = if flow.sign { format!("-{lhs}") } else { lhs };
    let mut rhs_terms = Vec::new();
    if !flow.end.is_empty() {
        rhs_terms.push(flow_pauli_string(&flow.end, layout, num_qubits)?);
    }
    for &measurement in &flow.measurements {
        let lookback = frame.resolve_measurement(measurement)?;
        rhs_terms.push(format!("rec[{lookback}]"));
    }
    if rhs_terms.is_empty() {
        rhs_terms.push(String::from("1"));
    }
    let text = format!("{lhs} -> {}", rhs_terms.join(" xor "));
    Ok(stim::Flow::new(&text)?)
}

fn flow_pauli_string(
    paulis: &PauliMap,
    layout: &FxHashMap<IVec2, u32>,
    num_qubits: u32,
) -> Result<String, StimVerifyError> {
    if paulis.is_empty() {
        Ok(String::from("1"))
    } else {
        Ok(paulis.to_pauli_string(layout, num_qubits as usize)?)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use bloq_circuit::{GateType, Pauli};

    use super::*;

    #[test]
    fn verifier_reports_interface_failures_without_circuit_wrappers() {
        let boundary: PauliMap = [(IVec2::ZERO, Pauli::Z)].into_iter().collect();
        let flow = Flow::new(boundary, PauliMap::empty());
        let mut chunk = Chunk {
            circuit: bloq_circuit::CoordCircuit::new(),
            flows: vec![flow.clone(), flow],
        };
        let error = chunk.verify_flows(None, None).unwrap_err();
        assert!(matches!(error, StimVerifyError::Flow(_)));
        assert!(error.source().unwrap().is::<FlowError>());
        assert!(
            error
                .to_string()
                .contains("multiple flows have the same START interface")
        );

        chunk.flows.pop();
        let error = chunk.verify_flows(Some(&[]), None).unwrap_err();
        assert!(matches!(error, StimVerifyError::UnmatchedInterface(_)));
        assert!(
            error
                .to_string()
                .starts_with("unmatched interface: chunk START interface")
        );
    }

    #[test]
    fn emission_failure_preserves_each_error_layer() {
        let error = StimVerifyError::from(crate::StimEmissionError::from(CircuitError::from(
            bloq_circuit::MeasurementFrameError::MeasurementCountOutOfRange {
                measurements: usize::MAX,
            },
        )));
        let emission = error.source().unwrap();
        assert!(emission.is::<crate::StimEmissionError>());
        let circuit = emission.source().unwrap();
        assert!(circuit.is::<CircuitError>());
        assert!(
            circuit
                .source()
                .unwrap()
                .is::<bloq_circuit::MeasurementFrameError>()
        );
    }

    #[test]
    fn stim_flow_verification_checks_signs() {
        let qubit = IVec2::ZERO;
        let mut circuit = bloq_circuit::CoordCircuit::new();
        circuit.do_gate(GateType::RZ, [qubit]).unwrap();
        circuit.tick();
        circuit.do_gate(GateType::X, [qubit]).unwrap();
        let boundary: PauliMap = [(qubit, Pauli::Z)].into_iter().collect();

        let signed = Chunk {
            circuit: circuit.clone(),
            flows: vec![Flow::new(PauliMap::empty(), boundary.clone()).with_sign(true)],
        };
        signed.verify_flows(None, None).unwrap();

        let unsigned = Chunk {
            circuit,
            flows: vec![Flow::new(PauliMap::empty(), boundary)],
        };
        assert!(matches!(
            unsigned.verify_flows(None, None),
            Err(StimVerifyError::ChunkDoesNotHaveFlows { .. })
        ));
    }
}
