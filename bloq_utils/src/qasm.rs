//! Shared OpenQASM syntax for small Clifford+T components and Pauli feedback.
//!
//! This module parses instructions; it does not execute a circuit or depend on
//! a quantum engine. Conditions use OpenQASM 2 register equality. Nested `if`s
//! express conjunctions; several conditional Paulis can express XOR.

use std::collections::BTreeMap;

use num_traits::{CheckedAdd, CheckedMul, CheckedSub};
use openqasm::GenericError as _;
use openqasm::ast::{Decl, Expr, Reg, Stmt};
use openqasm::translate::Value;
use thiserror::Error;

use crate::{Basis, PauliBasis};

/// Gates accepted by the shared parser, without an execution representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QasmGate {
    /// Identity.
    Identity,
    /// Hadamard.
    H,
    /// A single-qubit Pauli.
    Pauli(PauliBasis),
    /// An X/Z phase in units of pi/4. Rotations have this form up to global phase.
    Phase {
        /// Rotation axis.
        basis: Basis,
        /// Signed angle in quarter turns of pi.
        quarters: i64,
    },
    /// Controlled X.
    Cx,
    /// Controlled Z.
    Cz,
    /// X-basis controlled X.
    Xcx,
    /// Swap.
    Swap,
    /// Toffoli.
    Ccx,
    /// Controlled-controlled Z.
    Ccz,
}

/// One parsed instruction. Indices address [`QasmProgram::qubits`] and `bits`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QasmInstruction {
    /// A gate, optionally conditioned on a conjunction of `(bit, value)` tests.
    Gate {
        /// Gate kind.
        gate: QasmGate,
        /// Operand indices into [`QasmProgram::qubits`].
        qubits: Vec<usize>,
        /// Required classical bit values.
        condition: Vec<(usize, bool)>,
    },
    /// A computational-basis measurement with a retained classical result.
    Measure {
        /// Index into [`QasmProgram::qubits`].
        qubit: usize,
        /// Index into [`QasmProgram::bits`].
        bit: usize,
    },
    /// Reset to zero. Consumers decide which reset placements they support.
    Reset {
        /// Index into [`QasmProgram::qubits`].
        qubit: usize,
    },
}

/// Parsed registers and instructions, in declaration/program order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QasmProgram {
    qubits: Vec<String>,
    bits: Vec<String>,
    instructions: Vec<QasmInstruction>,
}

impl QasmProgram {
    /// Quantum wire names, always indexed (for example, `q[0]`).
    pub fn qubits(&self) -> &[String] {
        &self.qubits
    }

    /// Classical bit names, always indexed (for example, `m[0]`).
    pub fn bits(&self) -> &[String] {
        &self.bits
    }

    /// Instructions with barriers removed and register-wide gates expanded.
    pub fn instructions(&self) -> &[QasmInstruction] {
        &self.instructions
    }
}

/// Errors in the supported OpenQASM subset.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QasmError {
    /// Upstream syntax or type checking failed.
    #[error("OpenQASM parse failed: {0}")]
    Parse(String),
    /// The program violates the supported component boundary.
    #[error("invalid OpenQASM component: {0}")]
    Boundary(String),
    /// Register storage exceeds the supported bound.
    #[error("too many quantum or classical register entries")]
    TooManyQubits,
    /// The program uses an unsupported gate or angle.
    #[error("unsupported gate {0:?}")]
    UnsupportedGate(String),
}

const BUILTINS: &str = "
    opaque id q; opaque h q; opaque x q; opaque y q; opaque z q;
    opaque s q; opaque sdg q; opaque t q; opaque tdg q;
    opaque rx(phase) q; opaque rz(phase) q;
    opaque cx a,b; opaque cz a,b; opaque xcx a,b; opaque swap a,b;
    opaque ccx a,b,c; opaque ccz a,b,c;
";

#[derive(Clone, Copy)]
struct Register {
    offset: usize,
    len: usize,
}

/// Parses the supported OpenQASM 2 subset.
///
/// Supports the usual Clifford+T gates, exact pi/4 rotations, resets, Z
/// measurements, barriers, and `if (register == integer)` controlled X/Y/Z.
/// Nested conditions are allowed. Classical bits are single assignment and
/// must have been measured before use in a condition. Custom gates, decimal
/// angles, arbitrary includes, and conditional non-Pauli gates are unsupported.
/// `qelib1.inc` gates are built in; includes never read the filesystem.
///
/// # Errors
///
/// Returns a [`QasmError`] for unsupported syntax, invalid registers, repeated
/// classical assignments, or a condition reading an unmeasured bit. Register
/// storage is bounded to 65,536 quantum and 65,536 classical entries.
/// Conditions allow at most 63 bits per compared register and 64 total bit tests.
pub fn parse_qasm(input: &str) -> Result<QasmProgram, QasmError> {
    let mut source = strip_comments(input)?.replace('\r', "\n");
    // The upstream lexer treats this whole signature as one literal token.
    if let Some((signature, rest)) = source.split_once(';')
        && let Some(version) = signature.trim().strip_prefix("OPENQASM")
        && version.starts_with(char::is_whitespace)
    {
        source = format!("OPENQASM {};{rest}", version.trim());
    }
    for statement in source.split(';') {
        if let Some(include) = statement.trim().strip_prefix("include")
            && include.trim() != "\"qelib1.inc\""
        {
            return Err(QasmError::Boundary(
                "only the built-in qelib1.inc include is supported".into(),
            ));
        }
    }
    if source.contains('{') || source.contains('}') {
        return Err(QasmError::Boundary(
            "custom gate declarations are not supported".into(),
        ));
    }
    let mut cache = openqasm::SourceCache::new();
    let mut parser =
        openqasm::Parser::new(&mut cache).with_file_policy(openqasm::parser::FilePolicy::Ignore);
    parser.parse_source::<String>(source, None);
    parser.parse_source::<String>(BUILTINS.into(), None);
    let ast = parser
        .done()
        .to_errors()
        .map_err(|error| QasmError::Parse(error.to_string()))?;
    let mut program = QasmProgram {
        qubits: Vec::new(),
        bits: Vec::new(),
        instructions: Vec::new(),
    };
    let mut quantum = BTreeMap::new();
    let mut classical = BTreeMap::new();
    for declaration in &ast.decls {
        match &**declaration {
            Decl::QReg { reg } => register(reg, &mut quantum, &mut program.qubits)?,
            Decl::CReg { reg } => register(reg, &mut classical, &mut program.bits)?,
            _ => {}
        }
    }
    // Upstream type checking shifts `1_u64 << register_width` without a
    // bound. Check condition widths first so malformed input cannot panic.
    for declaration in &ast.decls {
        if let Decl::Stmt(statement) = &**declaration {
            let mut statement: &Stmt = statement;
            let mut tests = 0;
            while let Stmt::Conditional { reg, then, .. } = statement {
                let width = classical.get(reg.name.as_str()).map_or(0, |register| {
                    if reg.index.is_some() { 1 } else { register.len }
                });
                tests += width;
                if width >= 64 || tests > 64 {
                    return Err(QasmError::Boundary(
                        "conditions allow at most 63 bits per register and 64 total bit tests"
                            .into(),
                    ));
                }
                statement = then;
            }
        }
    }
    ast.type_check()
        .to_errors()
        .map_err(|error| QasmError::Parse(error.to_string()))?;

    let mut measured = vec![false; program.bits.len()];
    for declaration in &ast.decls {
        if let Decl::Stmt(statement) = &**declaration {
            parse_statement(
                statement,
                &quantum,
                &classical,
                &mut measured,
                &[],
                &mut program.instructions,
            )?;
        }
    }
    Ok(program)
}

fn register(
    reg: &Reg,
    registers: &mut BTreeMap<String, Register>,
    names: &mut Vec<String>,
) -> Result<(), QasmError> {
    let len = usize::try_from(reg.index.unwrap_or(1)).map_err(|_| QasmError::TooManyQubits)?;
    if len == 0 {
        return Err(QasmError::Boundary("register size must be nonzero".into()));
    }
    if len > 65_536 || names.len() > 65_536 - len {
        return Err(QasmError::TooManyQubits);
    }
    registers.insert(
        reg.name.to_string(),
        Register {
            offset: names.len(),
            len,
        },
    );
    names.extend((0..len).map(|index| format!("{}[{index}]", reg.name)));
    Ok(())
}

fn operands(reg: &Reg, registers: &BTreeMap<String, Register>) -> Vec<usize> {
    let register = registers[reg.name.as_str()];
    match reg.index {
        Some(index) => vec![register.offset + index as usize],
        None => (register.offset..register.offset + register.len).collect(),
    }
}

fn parse_statement(
    statement: &Stmt,
    quantum: &BTreeMap<String, Register>,
    classical: &BTreeMap<String, Register>,
    measured: &mut [bool],
    condition: &[(usize, bool)],
    output: &mut Vec<QasmInstruction>,
) -> Result<(), QasmError> {
    match statement {
        Stmt::Conditional { reg, val, then } => {
            let bits = operands(reg, classical);
            let mut condition = condition.to_vec();
            for (index, bit) in bits.into_iter().enumerate() {
                if !measured[bit] {
                    return Err(QasmError::Boundary(format!(
                        "condition reads unmeasured bit {}[{index}]",
                        reg.name
                    )));
                }
                condition.push((bit, **val & (1_u64 << index) != 0));
            }
            parse_statement(then, quantum, classical, measured, &condition, output)
        }
        Stmt::Gate { name, params, args } => {
            let name = name.as_str();
            let phase = |basis| {
                let value = exact_value(&params[0]).ok_or_else(|| {
                    QasmError::UnsupportedGate(format!(
                        "{name}: rotation must be an exact multiple of pi/4 using integer arithmetic and pi"
                    ))
                })?;
                let denominator = *value.b.denom();
                let quarters = (*value.b.numer()).checked_mul(4);
                if *value.a.numer() != 0
                    || 4 % denominator != 0
                    || quarters.is_none_or(|value| value % denominator != 0)
                {
                    return Err(QasmError::UnsupportedGate(format!(
                        "{name}: rotation must be an exact multiple of pi/4"
                    )));
                }
                Ok(QasmGate::Phase {
                    basis,
                    quarters: quarters.expect("checked above") / denominator,
                })
            };
            let gate = match name {
                "id" => QasmGate::Identity,
                "h" => QasmGate::H,
                "x" => QasmGate::Pauli(PauliBasis::X),
                "y" => QasmGate::Pauli(PauliBasis::Y),
                "z" => QasmGate::Pauli(PauliBasis::Z),
                "s" => QasmGate::Phase {
                    basis: Basis::Z,
                    quarters: 2,
                },
                "sdg" => QasmGate::Phase {
                    basis: Basis::Z,
                    quarters: -2,
                },
                "t" => QasmGate::Phase {
                    basis: Basis::Z,
                    quarters: 1,
                },
                "tdg" => QasmGate::Phase {
                    basis: Basis::Z,
                    quarters: -1,
                },
                "rx" => phase(Basis::X)?,
                "rz" => phase(Basis::Z)?,
                "cx" => QasmGate::Cx,
                "cz" => QasmGate::Cz,
                "xcx" => QasmGate::Xcx,
                "swap" => QasmGate::Swap,
                "ccx" => QasmGate::Ccx,
                "ccz" => QasmGate::Ccz,
                _ => return Err(QasmError::UnsupportedGate(name.to_string())),
            };
            if !condition.is_empty() && !matches!(gate, QasmGate::Pauli(_) | QasmGate::Identity) {
                return Err(QasmError::Boundary(
                    "only Pauli gates may be conditional".into(),
                ));
            }
            let args = args
                .iter()
                .map(|reg| operands(reg, quantum))
                .collect::<Vec<_>>();
            append_gate(gate, &args, condition, output)
        }
        Stmt::CX { copy, xor } => {
            if !condition.is_empty() {
                return Err(QasmError::Boundary(
                    "only Pauli gates may be conditional".into(),
                ));
            }
            append_gate(
                QasmGate::Cx,
                &[operands(copy, quantum), operands(xor, quantum)],
                condition,
                output,
            )
        }
        Stmt::Measure { from, to } => {
            if !condition.is_empty() {
                return Err(QasmError::Boundary(
                    "conditional measurements are unsupported".into(),
                ));
            }
            let from = operands(from, quantum);
            let to = operands(to, classical);
            if from.len() != to.len() {
                return Err(QasmError::Boundary(
                    "measurement register widths must match".into(),
                ));
            }
            for (qubit, bit) in from.into_iter().zip(to) {
                if std::mem::replace(&mut measured[bit], true) {
                    return Err(QasmError::Boundary(
                        "classical bits must be assigned only once".into(),
                    ));
                }
                output.push(QasmInstruction::Measure { qubit, bit });
            }
            Ok(())
        }
        Stmt::Reset { reg } => {
            if !condition.is_empty() {
                return Err(QasmError::Boundary(
                    "conditional resets are unsupported".into(),
                ));
            }
            output.extend(
                operands(reg, quantum)
                    .into_iter()
                    .map(|qubit| QasmInstruction::Reset { qubit }),
            );
            Ok(())
        }
        Stmt::Barrier { .. } => Ok(()),
        Stmt::U { .. } => Err(QasmError::UnsupportedGate("U".into())),
    }
}

fn append_gate(
    gate: QasmGate,
    operands: &[Vec<usize>],
    condition: &[(usize, bool)],
    output: &mut Vec<QasmInstruction>,
) -> Result<(), QasmError> {
    let width = operands.iter().map(Vec::len).max().unwrap_or(0);
    for index in 0..width {
        let qubits = operands
            .iter()
            .map(|arg| arg[if arg.len() == 1 { 0 } else { index }])
            .collect::<Vec<_>>();
        if qubits
            .iter()
            .enumerate()
            .any(|(index, qubit)| qubits[..index].contains(qubit))
        {
            return Err(QasmError::Boundary("gate operands must be distinct".into()));
        }
        output.push(QasmInstruction::Gate {
            gate,
            qubits,
            condition: condition.to_vec(),
        });
    }
    Ok(())
}

// Keep every intermediate exactly a + b*pi, before any downstream conversion.
fn exact_value(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Int(value) => Some(Value::int(i64::try_from(*value).ok()?)),
        Expr::Pi => Some(Value::PI),
        Expr::Neg(value) => exact_value(value)?.checked_neg(),
        Expr::Add(left, right)
        | Expr::Sub(left, right)
        | Expr::Mul(left, right)
        | Expr::Div(left, right) => {
            let left = exact_value(left)?;
            let right = exact_value(right)?;
            match expr {
                Expr::Add(..) => Some(Value {
                    a: left.a.checked_add(&right.a)?,
                    b: left.b.checked_add(&right.b)?,
                }),
                Expr::Sub(..) => Some(Value {
                    a: left.a.checked_sub(&right.a)?,
                    b: left.b.checked_sub(&right.b)?,
                }),
                Expr::Mul(..) if *left.b.numer() == 0 || *right.b.numer() == 0 => Some(Value {
                    a: left.a.checked_mul(&right.a)?,
                    b: left
                        .a
                        .checked_mul(&right.b)?
                        .checked_add(&left.b.checked_mul(&right.a)?)?,
                }),
                Expr::Div(..) if *right.b.numer() == 0 => left.checked_div(right),
                _ => None,
            }
        }
        _ => None,
    }
}

fn strip_comments(input: &str) -> Result<String, QasmError> {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '/' {
            output.push(character);
            continue;
        }
        match chars.peek().copied() {
            Some('/') => {
                chars.next();
                for next in chars.by_ref() {
                    if matches!(next, '\n' | '\r') {
                        output.push('\n');
                        break;
                    }
                }
            }
            Some('*') => {
                chars.next();
                let mut previous = '\0';
                let mut closed = false;
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        closed = true;
                        break;
                    }
                    previous = next;
                }
                if !closed {
                    return Err(QasmError::Boundary("unterminated block comment".into()));
                }
                output.push(' ');
            }
            _ => output.push(character),
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurements_and_nested_register_conditions_keep_their_identity() {
        let source = "OPENQASM 2.0; qreg q[4]; creg c[2]; creg other[1];
            reset q[2]; cx q[0],q[2]; cx q[1],q[2]; measure q[2] -> c[0];
            measure q[3] -> c[1]; measure q[1] -> other[0];
            if(c==3) if(other==0) y q[0];";
        let parsed = parse_qasm(source).unwrap();
        assert_eq!(parsed.bits(), ["c[0]", "c[1]", "other[0]"]);
        assert!(
            matches!(parsed.instructions().last(), Some(QasmInstruction::Gate {
            gate: QasmGate::Pauli(PauliBasis::Y), qubits, condition
        }) if qubits == &[0] && condition == &[(0,true), (1,true), (2,false)])
        );
        for body in [
            "if(c==1) x q[0];",                      // Unmeasured condition.
            "measure q[0] -> c; measure q[1] -> c;", // Overwritten result.
            "measure q[0] -> c; if(c==1) h q[1];",
            "cx q[0],q[0];",
            "measure q[0] -> c; if(c==2) x q[1];",
            "include \"unknown.inc\";",
        ] {
            assert!(
                parse_qasm(&format!("OPENQASM 2.0; qreg q[2]; creg c[1]; {body}")).is_err(),
                "{body}"
            );
        }
        for width in [64, 65, 65_536] {
            parse_qasm(&format!(
                "OPENQASM 2.0; qreg q[1]; creg c[{width}]; if(c==1) x q;"
            ))
            .unwrap_err();
        }
        parse_qasm("OPENQASM 2.0; qreg q[63]; creg c[63]; measure q -> c; if(c==1) x q[0];")
            .unwrap();
    }

    #[test]
    fn rotations_are_checked_before_lossy_conversion() {
        for angle in [
            "100000000",
            "0.7853982",
            "pi/8",
            "pi/(4+0.00000001)",
            "sin(pi/4)",
            "pi*pi-pi*pi+pi/4",
            "9223372036854775807+1",
        ] {
            let source = format!("OPENQASM 2.0; qreg q[1]; rz({angle}) q;");
            assert!(
                matches!(parse_qasm(&source), Err(QasmError::UnsupportedGate(_))),
                "{angle}"
            );
        }
        for (angle, quarters) in [
            ("pi/4", 1),
            ("-3*pi/4", -3),
            ("(2+1)*pi/(2*2)", 3),
            ("(1-1)+pi/4", 1),
            ("0", 0),
        ] {
            let parsed = parse_qasm(&format!("OPENQASM 2.0; qreg q[2]; rx({angle}) q;")).unwrap();
            assert_eq!(parsed.instructions().len(), 2);
            for instruction in parsed.instructions() {
                assert!(
                    matches!(instruction, QasmInstruction::Gate { gate: QasmGate::Phase { basis: Basis::X, quarters: actual }, .. } if *actual == quarters)
                );
            }
        }
        assert!(matches!(
            parse_qasm("OPENQASM 2.0; qreg q[1]; U(100000000,0,0) q;"),
            Err(QasmError::UnsupportedGate(_))
        ));
    }
}
