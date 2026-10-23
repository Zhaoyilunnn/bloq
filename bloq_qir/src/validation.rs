//! QIR input contract, checked without invoking the VM runtime.
//!
//! Validate arena references even in inactive tasks. Physical support is collected
//! from instructions as well as scheduling metadata, so RUS cannot hide an
//! operation on an unowned qubit. Runtime scheduling and logical patch I/O remain
//! outside this backend; LLVM and the emitter check the generated control flow.
use std::collections::BTreeSet;

use crate::{QirEmissionError, QirOptions, invalid, unsupported};
use bloq_vm::instruction::{
    BoolOp, Instruction, MemoryCycle, PauliProduct, Program, QuantumOp, QuantumStream,
    RecordParity, SourceRole, Stream, Task,
};

type Result<T = ()> = std::result::Result<T, QirEmissionError>;

fn require(condition: bool, message: &'static str) -> Result {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}

pub(super) fn validate_qir_input(program: &Program, options: &QirOptions) -> Result {
    if !program.inputs.is_empty() || !program.outputs.is_empty() {
        return Err(unsupported(
            "logical patch I/O requires authored preparation and terminal readout",
        ));
    }
    if options.max_attempts == 0 || options.max_wait_rounds == 0 {
        return Err(invalid("execution bounds must be positive"));
    }
    if program.tasks.len() > 100_000
        || program.bit_count > 1_000_000
        || program.record_count > 1_000_000
    {
        return Err(invalid("program exceeds emission limits"));
    }
    let mut bindings = BTreeSet::new();
    for binding in &options.decoders {
        if !bindings.insert(binding.observable)
            || binding.syndrome.is_empty()
            || binding.correction_count == 0
            || binding.correction_count > 64
            || binding.correction_bit >= binding.correction_count
            || !matches!(
                program
                    .tasks
                    .get(binding.observable as usize)
                    .map(|task| &task.instruction),
                Some(Instruction::Observable { .. })
            )
        {
            return Err(invalid("invalid or duplicate decoder binding"));
        }
    }
    let v = Validator(program);
    v.stream(&program.entry)?;
    for &bit in &options.output_bits {
        v.bit(bit)?;
    }
    for &record in &options.output_records {
        v.record(record)?;
    }
    for binding in &options.decoders {
        for parity in &binding.syndrome {
            v.parity(parity)?;
        }
    }
    // Complete all bounds checks before following references in RUS bodies.
    let supports = program
        .tasks
        .iter()
        .map(|task| v.task(task))
        .collect::<Result<Vec<_>>>()?;
    for task in &program.tasks {
        if let Instruction::Rus {
            body,
            owned_qubits,
            retry_prepare,
            decoder_hold,
            cultivation_exits,
            ..
        } = &task.instruction
        {
            let owned: BTreeSet<_> = owned_qubits.iter().copied().collect();
            require(
                owned.len() == owned_qubits.len(),
                "duplicate RUS owned qubit",
            )?;
            require(
                decoder_hold.is_empty() == cultivation_exits.is_empty()
                    && (task.source != Some(SourceRole::Factory) || !decoder_hold.is_empty()),
                "invalid RUS isolation",
            )?;
            let mut support = BTreeSet::new();
            v.quantum_stream(retry_prepare, &mut support)?;
            for cycle in decoder_hold {
                v.cycle(cycle, &mut support)?;
            }
            for &id in &body.tasks {
                require(
                    !matches!(
                        program.tasks[id as usize].instruction,
                        Instruction::Rus { .. }
                    ),
                    "nested dynamic RUS is unsupported",
                )?;
                support.extend(&supports[id as usize]);
            }
            require(
                support.is_subset(&owned),
                "RUS body touches qubits outside its isolated source support",
            )?;
            require(
                cultivation_exits
                    .iter()
                    .all(|exit| body.tasks.contains(exit)),
                "RUS cultivation exit is outside its body",
            )?;
        }
    }
    Ok(())
}

struct Validator<'a>(&'a Program);

impl Validator<'_> {
    fn bit(&self, id: u32) -> Result {
        require(id < self.0.bit_count, "invalid bit operand")
    }
    fn record(&self, id: u32) -> Result {
        require(id < self.0.record_count, "invalid record operand")
    }
    fn task_id(&self, id: u32) -> Result {
        require((id as usize) < self.0.tasks.len(), "invalid task reference")
    }
    fn qubit(&self, id: u32, support: &mut BTreeSet<u32>) -> Result {
        require(id < self.0.qubit_count, "invalid qubit operand")?;
        support.insert(id);
        Ok(())
    }
    fn stream(&self, stream: &Stream) -> Result {
        let mut seen = BTreeSet::new();
        for &id in &stream.tasks {
            self.task_id(id)?;
            require(seen.insert(id), "duplicate stream member")?;
        }
        for &id in &stream.bindings {
            self.task_id(id)?;
        }
        if let Some(bit) = stream.value {
            self.bit(bit)?;
        }
        Ok(())
    }
    fn parity(&self, parity: &RecordParity) -> Result {
        for &id in parity.records.iter() {
            self.record(id)?;
        }
        Ok(())
    }
    fn product(&self, product: &PauliProduct, support: &mut BTreeSet<u32>) -> Result {
        let mut seen = BTreeSet::new();
        for &(id, _) in &product.terms {
            self.qubit(id, support)?;
            require(seen.insert(id), "duplicate Pauli factor")?;
        }
        Ok(())
    }
    fn quantum(&self, op: &QuantumOp, support: &mut BTreeSet<u32>) -> Result {
        match op {
            QuantumOp::Gate1 { qubit, .. }
            | QuantumOp::Pauli { qubit, .. }
            | QuantumOp::T { qubit, .. }
            | QuantumOp::Reset { qubit, .. } => self.qubit(*qubit, support)?,
            QuantumOp::Gate2 {
                control, target, ..
            } => {
                self.qubit(*control, support)?;
                self.qubit(*target, support)?;
                require(control != target, "controlled Pauli needs distinct qubits")?;
            }
            QuantumOp::Measure {
                observable,
                records,
                flip_probability,
            } => {
                self.product(observable, support)?;
                require(!records.is_empty(), "measurement has no output record")?;
                require(
                    flip_probability.is_finite() && (0.0..=1.0).contains(flip_probability),
                    "invalid measurement probability",
                )?;
                for &id in records {
                    self.record(id)?;
                }
            }
            QuantumOp::ConditionalPauli { qubit, control, .. } => {
                self.qubit(*qubit, support)?;
                self.record(*control)?;
            }
            QuantumOp::Depolarize1 {
                probability,
                qubits,
            }
            | QuantumOp::PauliError {
                probability,
                qubits,
                ..
            } => {
                require(
                    probability.is_finite() && (0.0..=1.0).contains(probability),
                    "invalid noise probability",
                )?;
                for &id in qubits {
                    self.qubit(id, support)?;
                }
            }
            QuantumOp::Depolarize2 { probability, pairs } => {
                require(
                    probability.is_finite() && (0.0..=1.0).contains(probability),
                    "invalid noise probability",
                )?;
                for &(left, right) in pairs {
                    self.qubit(left, support)?;
                    self.qubit(right, support)?;
                    require(left != right, "noise pair needs distinct qubits")?;
                }
            }
        }
        Ok(())
    }
    fn quantum_stream(&self, stream: &QuantumStream, support: &mut BTreeSet<u32>) -> Result {
        for moment in &stream.moments {
            require(
                moment.duration.is_finite() && moment.duration >= 0.0,
                "invalid moment duration",
            )?;
            for op in &moment.operations {
                self.quantum(op, support)?;
            }
        }
        Ok(())
    }
    fn cycle(&self, cycle: &MemoryCycle, support: &mut BTreeSet<u32>) -> Result {
        self.quantum_stream(&cycle.stream, support)?;
        let duration: f64 = cycle.stream.moments.iter().map(|m| m.duration).sum();
        require(
            duration.is_finite()
                && duration > 0.0
                && cycle.round_duration.is_finite()
                && cycle.round_duration > 0.0
                && (duration - cycle.round_duration).abs() <= 1e-9,
            "invalid memory duration",
        )?;
        for parity in &cycle.detectors {
            self.parity(parity)?;
        }
        for initializer in &cycle.initializers {
            self.record(initializer.record)?;
            self.parity(&initializer.parity)?;
        }
        for flow in &cycle.boundary_flows {
            self.product(&flow.start, support)?;
            self.product(&flow.end, support)?;
            self.parity(&flow.parity)?;
            if let Some(record) = flow.frontier_record {
                self.record(record)?;
            }
        }
        Ok(())
    }
    fn task(&self, task: &Task) -> Result<BTreeSet<u32>> {
        require(
            task.release.is_finite()
                && task.release >= 0.0
                && task.duration.is_finite()
                && task.duration >= 0.0,
            "invalid task timing",
        )?;
        for &id in &task.dependencies {
            self.task_id(id)?;
        }
        for bit in task.activation.iter().chain(task.output.iter()) {
            self.bit(*bit)?;
        }
        let mut support = BTreeSet::new();
        for &id in &task.qubits {
            self.qubit(id, &mut support)?;
        }
        match &task.instruction {
            Instruction::Quantum(quantum) => {
                for alternative in &quantum.alternatives {
                    for &(bit, _) in &alternative.when {
                        self.bit(bit)?;
                    }
                    for &id in alternative
                        .selected_records
                        .iter()
                        .chain(alternative.excluded_records.iter())
                    {
                        self.record(id)?;
                    }
                    for parity in alternative
                        .detectors
                        .iter()
                        .chain(alternative.restarts.iter())
                    {
                        self.parity(parity)?;
                    }
                    self.quantum_stream(&alternative.stream, &mut support)?;
                }
            }
            Instruction::Eval(expr) | Instruction::Discard(expr) => {
                boolean(expr, self.0.bit_count)?
            }
            Instruction::Accumulate(parity) => self.parity(parity)?,
            Instruction::Bind(bindings) => {
                for binding in bindings {
                    self.product(&binding.operator, &mut support)?;
                }
            }
            Instruction::Observable { bits, bindings, .. }
            | Instruction::ReadoutRecipe { bits, bindings } => {
                for &id in bits {
                    self.bit(id)?;
                }
                for &id in bindings {
                    self.task_id(id)?;
                }
            }
            Instruction::Decode(request) => {
                self.task_id(request.observable)?;
                self.bit(request.raw)?;
                require(
                    matches!(
                        self.0.tasks[request.observable as usize].instruction,
                        Instruction::Observable { .. }
                    ),
                    "decoder target is not an observable",
                )?;
                require(
                    request.round_duration.is_finite() && request.round_duration >= 0.0,
                    "invalid decoder duration",
                )?;
            }
            Instruction::Rus {
                body,
                restart,
                owned_qubits,
                attempt_bits,
                attempt_records,
                retry_prepare,
                decoder_hold,
                cultivation_exits,
                ..
            } => {
                self.stream(body)?;
                boolean(restart, self.0.bit_count)?;
                for &id in owned_qubits {
                    self.qubit(id, &mut support)?;
                }
                for &id in attempt_bits {
                    self.bit(id)?;
                }
                for &id in attempt_records {
                    self.record(id)?;
                }
                for &id in cultivation_exits {
                    self.task_id(id)?;
                }
                self.quantum_stream(retry_prepare, &mut support)?;
                for cycle in decoder_hold {
                    self.cycle(cycle, &mut support)?;
                }
            }
            Instruction::WaitFor { until, memory } => {
                for &id in until {
                    self.task_id(id)?;
                }
                for cycle in memory {
                    self.cycle(cycle, &mut support)?;
                }
            }
            Instruction::MemoryRounds {
                stream, detectors, ..
            } => {
                self.quantum_stream(stream, &mut support)?;
                for parity in detectors {
                    self.parity(parity)?;
                }
            }
            Instruction::Idle {
                duration,
                noise_ratio,
                operations,
            } => {
                require(
                    duration.is_finite()
                        && *duration >= 0.0
                        && noise_ratio.is_finite()
                        && *noise_ratio >= 0.0,
                    "invalid idle timing",
                )?;
                for op in operations {
                    self.quantum(op, &mut support)?;
                }
            }
            Instruction::SignalReady { .. } => {}
        }
        Ok(support)
    }
}

fn boolean(expression: &BoolOp, count: u32) -> Result {
    // An explicit stack also checks unused Call arguments and unselected arms.
    let mut pending = vec![(expression, count)];
    while let Some((expr, count)) = pending.pop() {
        let check = |id: u32| require(id < count, "invalid bit operand");
        match expr {
            BoolOp::Const(_) => {}
            BoolOp::Copy(id) => check(*id)?,
            BoolOp::Not(inner) => pending.push((inner, count)),
            BoolOp::Parity { inputs, .. } => {
                for &id in inputs {
                    check(id)?;
                }
            }
            BoolOp::Xor(inputs) | BoolOp::And(inputs) | BoolOp::Or(inputs) => {
                pending.extend(inputs.iter().map(|expr| (expr, count)));
            }
            BoolOp::Select {
                condition,
                when_false,
                when_true,
            } => {
                pending.extend([
                    (condition.as_ref(), count),
                    (when_false.as_ref(), count),
                    (when_true.as_ref(), count),
                ]);
            }
            BoolOp::Call { body, inputs } => {
                for &id in inputs {
                    check(id)?;
                }
                let slots = u32::try_from(inputs.len())
                    .map_err(|_| invalid("too many function arguments"))?;
                pending.push((body, slots));
            }
        }
    }
    Ok(())
}
