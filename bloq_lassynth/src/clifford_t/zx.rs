//! QuiZX canonicalization of Clifford+T components into surface constraints.

use std::collections::HashMap;

use bloq_utils::{Pauli, PauliString, PhasedPauliString};
use quizx::circuit::Circuit;
use quizx::fscalar::Zero;
use quizx::graph::{BasisElem, EType, GraphLike, V, VType};
use quizx::simplify::full_simp;
use quizx::vec_graph::Graph;

use super::CliffordTError;
use crate::lassynth::{component_set, pauli_span_combination};

pub(super) struct CanonicalComponent {
    pub(super) stabilizers: Vec<PhasedPauliString>,
    pub(super) parity_rows: Vec<usize>,
    pub(super) t_count: usize,
}

#[derive(Clone, Copy)]
struct Injection {
    spider: V,
    magic: V,
    selective: V,
}

#[derive(Clone, Copy)]
struct Leg {
    qubit: usize,
    boundary: Option<V>,
    edge: EType,
}

pub(super) fn canonicalize(
    circuit: &Circuit,
    zero_inputs: &[bool],
    measured_outputs: &[bool],
) -> Result<CanonicalComponent, CliffordTError> {
    let mut graph: Graph = circuit.to_graph();
    graph.plug_inputs(&basis_elements(zero_inputs));
    graph.plug_outputs(&basis_elements(measured_outputs));
    full_simp(&mut graph);
    if graph.scalar().is_zero() {
        return Err(CliffordTError::ZeroMap);
    }
    expand_boundary_edges(&mut graph);
    let injections = rewrite_t_spiders(&mut graph)?;
    let GraphTableau {
        tableau,
        boundaries,
        outputs,
        internal,
        fixed_x,
    } = graph_tableau(&graph, &injections)?;
    let width = boundaries.len();
    let signatures = tableau
        .rows
        .iter()
        .map(|row| row.signature(&outputs))
        .collect::<Vec<_>>();
    let mut named = Vec::with_capacity(injections.len());
    for event in 0..injections.len() {
        let target = PauliString::single(
            outputs.len() + injections.len(),
            outputs.len() + event,
            Pauli::X,
        );
        let combination = pauli_span_combination(&target, &signatures)
            .ok_or(CliffordTError::NonCausalTMeasurement { injection: event })?;
        named.push(tableau.product(&combination));
    }

    let named_boundary = named
        .iter()
        .map(|row| project_boundary(row, &boundaries, &internal, &fixed_x))
        .collect::<Result<Vec<_>, _>>()?;
    // Reduce only event-free rows. Mixing the named rows back in would
    // reintroduce record dependence that the SAT table's other rows lack.
    let mut rows = Vec::with_capacity(tableau.rows.len());
    for row in &tableau.rows {
        let mut row = row.clone();
        for (event, parity) in named.iter().enumerate() {
            if row.events[event] {
                row.multiply(parity);
            }
        }
        rows.push(project_boundary(&row, &boundaries, &internal, &fixed_x)?);
    }
    let reduced = canonical_rows(rows);
    let mut basis = Vec::with_capacity(width);
    for (index, row) in named_boundary.iter().chain(&reduced).enumerate() {
        let selected = basis
            .iter()
            .map(|selected: &PhasedPauliString| selected.paulis.clone())
            .collect::<Vec<_>>();
        if pauli_span_combination(&row.operator.paulis, &selected).is_some() {
            if index < named_boundary.len() {
                return Err(CliffordTError::UnsupportedSimplifiedZx);
            }
        } else {
            basis.push(row.operator.clone());
        }
        if basis.len() == width {
            break;
        }
    }
    if basis.len() != width {
        return Err(CliffordTError::UnsupportedSimplifiedZx);
    }

    Ok(CanonicalComponent {
        stabilizers: basis,
        parity_rows: (0..injections.len()).collect(),
        t_count: injections.len(),
    })
}

fn canonical_rows(mut rows: Vec<Row>) -> Vec<Row> {
    let width = rows.first().map_or(0, |row| row.operator.paulis.len());
    let mut pivot = 0;
    for component in 0..2 * width {
        let Some(found) =
            (pivot..rows.len()).find(|&row| component_set(&rows[row].operator.paulis, component))
        else {
            continue;
        };
        rows.swap(pivot, found);
        let leading = rows[pivot].clone();
        for (index, row) in rows.iter_mut().enumerate() {
            if index != pivot && component_set(&row.operator.paulis, component) {
                row.multiply(&leading);
            }
        }
        pivot += 1;
    }
    rows.truncate(pivot);
    rows
}

fn basis_elements(fixed: &[bool]) -> Vec<BasisElem> {
    fixed
        .iter()
        .map(|fixed| {
            if *fixed {
                BasisElem::Z0
            } else {
                BasisElem::SKIP
            }
        })
        .collect()
}

fn quarter_turns(graph: &Graph, vertex: V) -> Result<i32, CliffordTError> {
    let (numerator, denominator) = graph.phase(vertex).to_rational().into_raw();
    numerator
        .checked_mul(4)
        .filter(|value| value % denominator == 0)
        .and_then(|value| i32::try_from(value / denominator).ok())
        .ok_or(CliffordTError::UnsupportedSimplifiedZx)
}

fn expand_boundary_edges(graph: &mut Graph) {
    for (left, right, edge) in graph.edge_vec() {
        if graph.vertex_type(left) != VType::B || graph.vertex_type(right) != VType::B {
            continue;
        }
        graph.remove_edge(left, right);
        let spider = graph.add_vertex(VType::Z);
        graph.add_edge_with_type(left, spider, edge);
        graph.add_edge(right, spider);
    }
}

fn rewrite_t_spiders(graph: &mut Graph) -> Result<Vec<Injection>, CliffordTError> {
    let mut injections = Vec::new();
    for spider in graph.vertex_vec() {
        if !matches!(graph.vertex_type(spider), VType::Z | VType::X) {
            continue;
        }
        let quarters = quarter_turns(graph, spider)?.rem_euclid(8);
        if quarters % 2 == 0 {
            continue;
        }
        graph.set_phase(spider, (i64::from(quarters - 1), 4));
        let edge = if graph.vertex_type(spider) == VType::Z {
            EType::N
        } else {
            EType::H
        };
        let magic = graph.add_vertex(VType::B);
        let selective = graph.add_vertex(VType::B);
        graph.add_edge_with_type(spider, magic, edge);
        graph.add_edge_with_type(spider, selective, edge);
        injections.push(Injection {
            spider,
            magic,
            selective,
        });
    }
    let mut inputs = graph.inputs().clone();
    inputs.extend(injections.iter().map(|injection| injection.magic));
    graph.set_inputs(inputs);
    let mut outputs = graph.outputs().clone();
    outputs.extend(injections.iter().map(|injection| injection.selective));
    graph.set_outputs(outputs);
    Ok(injections)
}

/// One Clifford tableau over every spider leg of the simplified diagram, with
/// the column bookkeeping needed to project it back onto the open boundaries.
struct GraphTableau {
    tableau: Tableau,
    /// Columns of the open inputs followed by the open outputs.
    boundaries: Vec<usize>,
    /// Columns of the open outputs alone, for the causality signature.
    outputs: Vec<usize>,
    /// Spider-to-spider edges, as the pair of leg columns each one joins.
    internal: Vec<(usize, usize, EType)>,
    /// Virtual T-record columns, postselected onto X.
    fixed_x: Vec<usize>,
}

fn graph_tableau(graph: &Graph, injections: &[Injection]) -> Result<GraphTableau, CliffordTError> {
    let mut legs = HashMap::<V, Vec<Leg>>::new();
    let mut boundaries = HashMap::<V, usize>::new();
    let mut internal = Vec::new();
    let mut width = 0;
    let auxiliary = injections
        .iter()
        .flat_map(|injection| [injection.magic, injection.selective])
        .collect::<std::collections::HashSet<_>>();
    let mut edges = graph.edge_vec();
    edges.sort_by_key(|&(left, right, _)| (left, right));
    for (left, right, edge) in edges {
        match (graph.vertex_type(left), graph.vertex_type(right)) {
            (VType::B, VType::B) => return Err(CliffordTError::UnsupportedSimplifiedZx),
            (VType::B, _) | (_, VType::B) => {
                let (boundary, spider) = if graph.vertex_type(left) == VType::B {
                    (left, right)
                } else {
                    (right, left)
                };
                if boundaries.insert(boundary, width).is_some() {
                    return Err(CliffordTError::UnsupportedSimplifiedZx);
                }
                if !auxiliary.contains(&boundary) {
                    legs.entry(spider).or_default().push(Leg {
                        qubit: width,
                        boundary: Some(boundary),
                        edge,
                    });
                }
                width += 1;
            }
            (VType::Z | VType::X, VType::Z | VType::X) => {
                legs.entry(left).or_default().push(Leg {
                    qubit: width,
                    boundary: None,
                    edge,
                });
                legs.entry(right).or_default().push(Leg {
                    qubit: width + 1,
                    boundary: None,
                    edge,
                });
                internal.push((width, width + 1, edge));
                width += 2;
            }
            _ => return Err(CliffordTError::UnsupportedSimplifiedZx),
        }
    }

    let virtuals = (0..injections.len())
        .map(|event| width + event)
        .collect::<Vec<_>>();
    width += virtuals.len();
    let mut tableau = Tableau::new(width, injections.len());
    let injection_by_spider = injections
        .iter()
        .copied()
        .enumerate()
        .map(|(event, injection)| (injection.spider, (event, injection)))
        .collect::<HashMap<_, _>>();
    let mut vertices = graph.vertex_vec();
    vertices.sort_unstable();
    for spider in vertices {
        let kind = graph.vertex_type(spider);
        if !matches!(kind, VType::Z | VType::X) {
            continue;
        }
        let mut spider_legs = legs.remove(&spider).unwrap_or_default();
        if let Some(&(event, _)) = injection_by_spider.get(&spider) {
            spider_legs.push(Leg {
                qubit: virtuals[event],
                boundary: None,
                edge: EType::N,
            });
        }
        spider_legs.sort_by_key(|leg| leg.qubit);
        if spider_legs.is_empty() {
            continue;
        }
        prepare_spider(
            &mut tableau,
            kind,
            quarter_turns(graph, spider)?,
            &spider_legs,
        )?;
        for leg in spider_legs {
            if leg.boundary.is_some() && leg.edge == EType::H {
                tableau.h(leg.qubit);
            }
        }
    }

    for (event, injection) in injections.iter().enumerate() {
        let magic = boundaries[&injection.magic];
        let selective = boundaries[&injection.selective];
        let virtual_leg = virtuals[event];
        if graph.vertex_type(injection.spider) == VType::X {
            tableau.h(virtual_leg);
        }
        tableau.h(magic);
        tableau.cx(magic, selective);
        tableau.postselect(
            PauliString::from_terms(width, [(virtual_leg, Pauli::Z), (selective, Pauli::Z)]),
            Some(event),
        )?;
        tableau.postselect(PauliString::single(width, virtual_leg, Pauli::X), None)?;
    }

    for &(left, right, edge) in &internal {
        let pair =
            |a, pauli_a, b, pauli_b| PauliString::from_terms(width, [(a, pauli_a), (b, pauli_b)]);
        let observables = if edge == EType::H {
            [
                pair(left, Pauli::X, right, Pauli::Z),
                pair(left, Pauli::Z, right, Pauli::X),
            ]
        } else {
            [
                pair(left, Pauli::X, right, Pauli::X),
                pair(left, Pauli::Z, right, Pauli::Z),
            ]
        };
        for observable in observables {
            tableau.postselect(observable, None)?;
        }
    }

    let ordered = graph
        .inputs()
        .iter()
        .chain(graph.outputs())
        .map(|boundary| {
            boundaries
                .get(boundary)
                .copied()
                .ok_or(CliffordTError::UnsupportedSimplifiedZx)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let outputs = graph
        .outputs()
        .iter()
        .map(|boundary| {
            boundaries
                .get(boundary)
                .copied()
                .ok_or(CliffordTError::UnsupportedSimplifiedZx)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(GraphTableau {
        tableau,
        boundaries: ordered,
        outputs,
        internal,
        fixed_x: virtuals,
    })
}

fn project_boundary(
    row: &Row,
    boundaries: &[usize],
    internal: &[(usize, usize, EType)],
    fixed_x: &[usize],
) -> Result<Row, CliffordTError> {
    let mut row = row.clone();
    let width = row.operator.paulis.len();
    for &(left, right, edge) in internal {
        let generators = if edge == EType::H {
            [(Pauli::X, Pauli::Z), (Pauli::Z, Pauli::X)]
        } else {
            [(Pauli::X, Pauli::X), (Pauli::Z, Pauli::Z)]
        };
        let mut cleared = None;
        for selected in 0..4 {
            let mut candidate = row.clone();
            for (index, &(left_pauli, right_pauli)) in generators.iter().enumerate() {
                if selected & (1 << index) != 0 {
                    let mut generator = Row::identity(width, row.events.len());
                    generator.operator.paulis =
                        PauliString::from_terms(width, [(left, left_pauli), (right, right_pauli)]);
                    candidate.multiply(&generator);
                }
            }
            if candidate.operator.paulis.get(left) == Pauli::I
                && candidate.operator.paulis.get(right) == Pauli::I
            {
                cleared = Some(candidate);
                break;
            }
        }
        row = cleared.ok_or(CliffordTError::UnsupportedSimplifiedZx)?;
    }
    for &qubit in fixed_x {
        match row.operator.paulis.get(qubit) {
            Pauli::I => {}
            Pauli::X => {
                let mut generator = Row::identity(width, row.events.len());
                generator.operator.paulis = PauliString::single(width, qubit, Pauli::X);
                row.multiply(&generator);
            }
            _ => return Err(CliffordTError::UnsupportedSimplifiedZx),
        }
    }
    Ok(row.project(boundaries))
}

fn prepare_spider(
    tableau: &mut Tableau,
    kind: VType,
    quarters: i32,
    legs: &[Leg],
) -> Result<(), CliffordTError> {
    let first = legs[0].qubit;
    tableau.h(first);
    for leg in &legs[1..] {
        tableau.cx(first, leg.qubit);
    }
    match quarters.rem_euclid(8) {
        0 => {}
        2 => tableau.s(first),
        4 => tableau.z(first),
        6 => tableau.sdg(first),
        _ => return Err(CliffordTError::UnsupportedSimplifiedZx),
    }
    if kind == VType::X {
        for leg in legs {
            tableau.h(leg.qubit);
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Row {
    operator: PhasedPauliString,
    events: Vec<bool>,
}

impl Row {
    fn identity(width: usize, events: usize) -> Self {
        Self {
            operator: PhasedPauliString::positive(PauliString::new(width)),
            events: vec![false; events],
        }
    }

    fn multiply(&mut self, other: &Self) {
        self.operator.multiply_assign(&other.operator);
        debug_assert!(self.operator.hermitian_sign().is_some());
        self.events
            .iter_mut()
            .zip(&other.events)
            .for_each(|(left, right)| *left ^= right);
    }

    fn project(&self, qubits: &[usize]) -> Self {
        Self {
            operator: PhasedPauliString::new(
                PauliString::from_terms(
                    qubits.len(),
                    qubits
                        .iter()
                        .copied()
                        .enumerate()
                        .filter_map(|(index, qubit)| {
                            let pauli = self.operator.paulis.get(qubit);
                            (pauli != Pauli::I).then_some((index, pauli))
                        }),
                ),
                self.operator.phase(),
            ),
            events: self.events.clone(),
        }
    }

    fn signature(&self, outputs: &[usize]) -> PauliString {
        let output_count = outputs.len();
        PauliString::from_terms(
            output_count + self.events.len(),
            outputs
                .iter()
                .copied()
                .enumerate()
                .filter_map(|(index, qubit)| {
                    let pauli = self.operator.paulis.get(qubit);
                    (pauli != Pauli::I).then_some((index, pauli))
                })
                .chain(
                    self.events
                        .iter()
                        .enumerate()
                        .filter_map(|(event, set)| set.then_some((output_count + event, Pauli::X))),
                ),
        )
    }
}

struct Tableau {
    rows: Vec<Row>,
    event_count: usize,
}

impl Tableau {
    fn new(width: usize, event_count: usize) -> Self {
        Self {
            rows: (0..width)
                .map(|qubit| Row {
                    operator: PhasedPauliString::positive(PauliString::single(
                        width,
                        qubit,
                        Pauli::Z,
                    )),
                    events: vec![false; event_count],
                })
                .collect(),
            event_count,
        }
    }

    fn h(&mut self, qubit: usize) {
        for row in &mut self.rows {
            row.operator.conjugate_h(qubit);
        }
    }

    fn s(&mut self, qubit: usize) {
        for row in &mut self.rows {
            row.operator.conjugate_s(qubit);
        }
    }

    fn sdg(&mut self, qubit: usize) {
        for row in &mut self.rows {
            row.operator.conjugate_sdg(qubit);
        }
    }

    fn z(&mut self, qubit: usize) {
        for row in &mut self.rows {
            row.operator.conjugate_z(qubit);
        }
    }

    fn cx(&mut self, control: usize, target: usize) {
        for row in &mut self.rows {
            row.operator.conjugate_cx(control, target);
        }
    }

    fn postselect(
        &mut self,
        observable: PauliString,
        event: Option<usize>,
    ) -> Result<(), CliffordTError> {
        let pivot = self
            .rows
            .iter()
            .position(|row| !row.operator.paulis.commutes_with(&observable));
        let Some(pivot) = pivot else {
            let rows = self
                .rows
                .iter()
                .map(|row| row.operator.paulis.clone())
                .collect::<Vec<_>>();
            let combination = pauli_span_combination(&observable, &rows)
                .ok_or(CliffordTError::UnsupportedSimplifiedZx)?;
            let existing = self.product(&combination);
            return if existing.operator.hermitian_sign() != Some(false)
                || existing.events.iter().any(|event| *event)
            {
                Err(CliffordTError::UnsupportedSimplifiedZx)
            } else {
                Ok(())
            };
        };
        let leading = self.rows[pivot].clone();
        for (index, row) in self.rows.iter_mut().enumerate() {
            if index != pivot && !row.operator.paulis.commutes_with(&observable) {
                row.multiply(&leading);
            }
        }
        let mut replacement = Row::identity(observable.len(), self.event_count);
        replacement.operator.paulis = observable;
        if let Some(event) = event {
            replacement.events[event] = true;
        }
        self.rows[pivot] = replacement;
        Ok(())
    }

    fn product(&self, rows: &[usize]) -> Row {
        let mut product = Row::identity(self.rows[0].operator.paulis.len(), self.event_count);
        for &row in rows {
            product.multiply(&self.rows[row]);
        }
        product
    }
}

#[cfg(test)]
mod tests {
    use quizx::circuit::Circuit;

    use super::canonicalize;

    #[test]
    fn whole_zx_simplification_cancels_inverse_t_phases() {
        let circuit = Circuit::from_qasm("qreg q[1]; t q[0]; tdg q[0];").unwrap();
        assert_eq!(
            canonicalize(&circuit, &[false], &[false]).unwrap().t_count,
            0
        );
    }

    #[test]
    fn t_rewrite_exposes_a_past_closing_parity_row() {
        let circuit = Circuit::from_qasm("qreg q[1]; t q[0];").unwrap();
        let canonical = canonicalize(&circuit, &[false], &[false]).unwrap();
        assert_eq!(canonical.t_count, 1);
        assert_eq!(
            canonical.stabilizers[canonical.parity_rows[0]]
                .paulis
                .to_string(),
            "ZZ__"
        );
    }

    #[test]
    fn cnot_has_the_canonical_choi_relation() {
        let circuit = Circuit::from_qasm("qreg q[2]; cx q[0],q[1];").unwrap();
        let canonical = canonicalize(&circuit, &[false; 2], &[false; 2]).unwrap();
        let rows = canonical
            .stabilizers
            .iter()
            .map(|row| row.paulis.clone())
            .collect::<Vec<_>>();
        for expected in ["X_XX", "_X_X", "Z_Z_", "_ZZZ"] {
            let expected = bloq_utils::PauliString::try_from(expected).unwrap();
            assert!(super::pauli_span_combination(&expected, &rows).is_some());
        }
    }

    #[test]
    fn clifford_equivalent_sources_have_one_surface_problem() {
        let cnot = Circuit::from_qasm("qreg q[2]; cx q[0],q[1];").unwrap();
        let hczh = Circuit::from_qasm("qreg q[2]; h q[1]; cz q[0],q[1]; h q[1];").unwrap();

        assert_eq!(
            canonicalize(&cnot, &[false; 2], &[false; 2])
                .unwrap()
                .stabilizers,
            canonicalize(&hczh, &[false; 2], &[false; 2])
                .unwrap()
                .stabilizers,
        );
    }

    #[test]
    fn terminal_z_measurement_discards_diagonal_phases() {
        let circuit = Circuit::from_qasm("qreg q[1]; h q[0]; t q[0];").unwrap();
        assert_eq!(
            canonicalize(&circuit, &[false], &[true]).unwrap().t_count,
            0
        );
    }
}
