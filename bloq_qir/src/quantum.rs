use std::collections::BTreeSet;
use std::sync::OnceLock;

use bloq_vm::instruction::{Clifford1, Pauli, QuantumOp};

use crate::emit::Emitter;
use crate::{QirEmissionError, invalid, unsupported};

type Result<T> = std::result::Result<T, QirEmissionError>;

pub(super) fn pointer(index: u32) -> String {
    if index == 0 {
        "ptr null".to_owned()
    } else {
        format!("ptr inttoptr (i64 {index} to ptr)")
    }
}

impl Emitter<'_> {
    fn gate(&mut self, name: &str, qubit: u32) {
        let suffix = match name {
            "sdg" => "s__adj",
            "tdg" => "t__adj",
            _ => "",
        };
        let suffix = if suffix.is_empty() {
            format!("{name}__body")
        } else {
            suffix.to_owned()
        };
        self.line(format!(
            "call void @__quantum__qis__{suffix}({})",
            pointer(qubit)
        ));
    }
    fn cx(&mut self, control: u32, target: u32) {
        self.line(format!(
            "call void @__quantum__qis__cx__body({}, {})",
            pointer(control),
            pointer(target)
        ));
    }
    fn axes(&mut self, basis: Pauli, qubit: u32, inverse: bool) {
        match (basis, inverse) {
            (Pauli::Z, _) => {}
            (Pauli::X, _) => self.gate("h", qubit),
            (Pauli::Y, false) => {
                self.gate("sdg", qubit);
                self.gate("h", qubit);
            }
            (Pauli::Y, true) => {
                self.gate("h", qubit);
                self.gate("s", qubit);
            }
        }
    }
    fn pauli(&mut self, basis: Pauli, qubit: u32) {
        match basis {
            Pauli::X => self.gate("x", qubit),
            Pauli::Z => self.gate("z", qubit),
            Pauli::Y => {
                self.gate("x", qubit);
                self.gate("z", qubit);
            }
        }
    }
    fn qubit(&self, qubit: u32) -> Result<()> {
        if qubit >= self.program.qubit_count {
            return Err(invalid(format!("qubit {qubit} exceeds resource count")));
        }
        Ok(())
    }
    pub(super) fn quantum_op(&mut self, op: &QuantumOp) -> Result<()> {
        match op {
            QuantumOp::Gate1 { gate, qubit } => {
                self.qubit(*qubit)?;
                for name in clifford(*gate) {
                    self.gate(name, *qubit);
                }
            }
            QuantumOp::Gate2 {
                control_basis,
                target_basis,
                control,
                target,
            } => {
                self.qubit(*control)?;
                self.qubit(*target)?;
                if control == target {
                    return Err(invalid("controlled Pauli needs distinct qubits"));
                }
                if *control_basis == Pauli::Z && *target_basis == Pauli::X {
                    self.cx(*control, *target);
                } else {
                    self.axes(*control_basis, *control, false);
                    self.axes(*target_basis, *target, false);
                    self.gate("h", *target);
                    self.cx(*control, *target);
                    self.gate("h", *target);
                    self.axes(*target_basis, *target, true);
                    self.axes(*control_basis, *control, true);
                }
            }
            QuantumOp::Pauli { basis, qubit } => {
                self.qubit(*qubit)?;
                self.pauli(*basis, *qubit);
            }
            QuantumOp::T {
                basis,
                qubit,
                adjoint,
            } => {
                self.qubit(*qubit)?;
                self.axes(*basis, *qubit, false);
                self.gate(if *adjoint { "tdg" } else { "t" }, *qubit);
                self.axes(*basis, *qubit, true);
            }
            QuantumOp::Reset { basis, qubit } => {
                self.qubit(*qubit)?;
                self.line(format!(
                    "call void @__quantum__qis__reset__body({})",
                    pointer(*qubit)
                ));
                self.axes(*basis, *qubit, true);
            }
            QuantumOp::Measure {
                observable,
                records,
                flip_probability,
            } => {
                if *flip_probability != 0.0 {
                    return Err(unsupported("measurement readout noise"));
                }
                let mut seen = BTreeSet::new();
                for &(qubit, _) in &observable.terms {
                    self.qubit(qubit)?;
                    if !seen.insert(qubit) {
                        return Err(invalid("Pauli product has duplicate factors"));
                    }
                }
                let mut result = if observable.terms.is_empty() {
                    "false".to_owned()
                } else {
                    let many = observable.terms.len() > 1;
                    let measured = if many {
                        self.scratch = true;
                        self.program.qubit_count
                    } else {
                        observable.terms[0].0
                    };
                    if many {
                        self.line(format!(
                            "call void @__quantum__qis__reset__body({})",
                            pointer(measured)
                        ));
                    }
                    for &(qubit, axis) in &observable.terms {
                        self.axes(axis, qubit, false);
                        if many {
                            self.cx(qubit, measured);
                        }
                    }
                    let site = self.results;
                    self.results = self
                        .results
                        .checked_add(1)
                        .ok_or_else(|| invalid("result count overflow"))?;
                    self.line(format!(
                        "call void @__quantum__qis__mz__body({}, {})",
                        pointer(measured),
                        pointer(site)
                    ));
                    let result = self.value(format!(
                        "call i1 @__quantum__rt__read_result({})",
                        pointer(site)
                    ));
                    for &(qubit, axis) in observable.terms.iter().rev() {
                        self.axes(axis, qubit, true);
                    }
                    result
                };
                if observable.negative {
                    result = self.value(format!("xor i1 {result}, true"));
                }
                for &record in records {
                    self.write('r', record, &result)?;
                }
            }
            QuantumOp::ConditionalPauli {
                basis,
                qubit,
                control,
            } => {
                self.qubit(*qubit)?;
                let apply = self.read('r', *control)?;
                let yes = self.label();
                let join = self.label();
                self.line(format!("br i1 {apply}, label %{yes}, label %{join}"));
                self.block(&yes);
                self.pauli(*basis, *qubit);
                self.line(format!("br label %{join}"));
                self.block(&join);
            }
            QuantumOp::Depolarize1 { probability, .. }
            | QuantumOp::Depolarize2 { probability, .. }
            | QuantumOp::PauliError { probability, .. } => {
                if *probability != 0.0 {
                    return Err(unsupported("stochastic quantum noise"));
                }
            }
        }
        Ok(())
    }
}

// Signed X/Z images independently pinned by bloq_vm::gate_table's Stim tests.
fn images(gate: Clifford1) -> (i8, i8) {
    use Clifford1::*;
    match gate {
        H => (3, 1),
        H_XY => (2, -3),
        H_YZ => (-1, 2),
        H_NXY => (-2, -3),
        H_NXZ => (-3, -1),
        H_NYZ => (-1, -2),
        SQRT_X => (1, -2),
        SQRT_X_DAG => (1, 2),
        SQRT_Y => (-3, 1),
        SQRT_Y_DAG => (3, -1),
        S => (2, 3),
        S_DAG => (-2, 3),
        C_XYZ => (2, 1),
        C_ZYX => (3, 2),
        C_NXYZ => (-2, -1),
        C_XNYZ => (-2, 1),
        C_XYNZ => (2, -1),
        C_NZYX => (-3, -2),
        C_ZNYX => (3, -2),
        C_ZYNX => (-3, 2),
    }
}

type CliffordTable = Vec<((i8, i8), Vec<&'static str>)>;

fn clifford(gate: Clifford1) -> &'static [&'static str] {
    static TABLE: OnceLock<CliffordTable> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = vec![((1, 3), Vec::new())];
        let mut cursor = 0;
        while cursor < table.len() {
            let (pair, path) = table[cursor].clone();
            cursor += 1;
            for (name, mapping) in [("h", [3_i8, -2, 1]), ("s", [2_i8, -1, 3])] {
                let transform = |p: i8| p.signum() * mapping[p.unsigned_abs() as usize - 1];
                let pair = (transform(pair.0), transform(pair.1));
                if !table.iter().any(|(existing, _)| *existing == pair) {
                    let mut path = path.clone();
                    path.push(name);
                    table.push((pair, path));
                }
            }
        }
        table
    });
    let image = images(gate);
    &table
        .iter()
        .find(|(pair, _)| *pair == image)
        .expect("H and S generate all signed Clifford images")
        .1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decompositions_have_the_required_signed_images() {
        use Clifford1::*;
        use bloq_vm::Gate1Q;
        let cases = [
            (H, Gate1Q::H),
            (H_XY, Gate1Q::Hxy),
            (H_YZ, Gate1Q::Hyz),
            (H_NXY, Gate1Q::Hnxy),
            (H_NXZ, Gate1Q::Hnxz),
            (H_NYZ, Gate1Q::Hnyz),
            (SQRT_X, Gate1Q::SqrtX),
            (SQRT_X_DAG, Gate1Q::SqrtXDag),
            (SQRT_Y, Gate1Q::SqrtY),
            (SQRT_Y_DAG, Gate1Q::SqrtYDag),
            (S, Gate1Q::S),
            (S_DAG, Gate1Q::SDag),
            (C_XYZ, Gate1Q::Cxyz),
            (C_ZYX, Gate1Q::Czyx),
            (C_NXYZ, Gate1Q::Cnxyz),
            (C_XNYZ, Gate1Q::Cxnyz),
            (C_XYNZ, Gate1Q::Cxynz),
            (C_NZYX, Gate1Q::Cnzyx),
            (C_ZNYX, Gate1Q::Cznyx),
            (C_ZYNX, Gate1Q::Czynx),
        ];
        for (gate, reference) in cases {
            let mut pair = (1_i8, 3_i8);
            for name in clifford(gate) {
                let table = if *name == "h" {
                    [3_i8, -2, 1]
                } else {
                    [2_i8, -1, 3]
                };
                let transform = |p: i8| p.signum() * table[p.unsigned_abs() as usize - 1];
                pair = (transform(pair.0), transform(pair.1));
            }
            let (x, z) = reference.images();
            let signed = |(axis, negative): (bloq_vm::PauliBasis, bool)| {
                let axis = match axis {
                    bloq_vm::PauliBasis::X => 1,
                    bloq_vm::PauliBasis::Y => 2,
                    bloq_vm::PauliBasis::Z => 3,
                };
                if negative { -axis } else { axis }
            };
            assert_eq!(pair, (signed(x), signed(z)), "{gate:?}");
        }
    }
}
