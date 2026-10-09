use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::process::{Command, Stdio};

use bloq_ir::ObservableOutput;
use bloq_vm::instruction::{
    BitId, BoolOp, Instruction, MemoryCycle, Program, QuantumStream, RecordParity, Stream, TaskId,
};

use crate::{
    PendingDecode, PendingSolve, QirArtifact, QirEmissionError, QirOptions, invalid, unsupported,
};

type Result<T> = std::result::Result<T, QirEmissionError>;

pub(super) struct Emitter<'a> {
    pub(super) program: &'a Program,
    options: &'a QirOptions,
    pub(super) code: String,
    pub(super) next: u32,
    pub(super) results: u32,
    pub(super) scratch: bool,
    done: BTreeSet<TaskId>,
    pending: BTreeMap<TaskId, PendingSolve>,
    cached: BTreeMap<TaskId, (String, String)>,
    retry: Option<String>,
}

pub(super) fn emit(program: &Program, options: &QirOptions) -> Result<QirArtifact> {
    bloq_vm::runtime::validate_program(program).map_err(|error| invalid(error.to_string()))?;
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
    let mut e = Emitter {
        program,
        options,
        code: String::from(
            "define i64 @bloq_entry() #0 {\nentry:\n  call void @__quantum__rt__initialize(ptr null)\n",
        ),
        next: 0,
        results: 0,
        scratch: false,
        done: BTreeSet::new(),
        pending: BTreeMap::new(),
        cached: BTreeMap::new(),
        retry: None,
    };
    for (kind, count) in [('b', program.bit_count), ('r', program.record_count)] {
        for index in 0..count {
            e.line(format!("%{kind}{index} = alloca i1"));
            e.line(format!("%{kind}v{index} = alloca i1"));
            e.line(format!("store i1 false, ptr %{kind}{index}"));
            e.line(format!("store i1 false, ptr %{kind}v{index}"));
        }
    }
    e.stream(&program.entry)?;
    e.finish_solves(&[])?;
    let mut outputs = Vec::new();
    for &record in &options.output_records {
        outputs.push(e.read('r', record)?);
    }
    for &bit in &options.output_bits {
        outputs.push(e.read('b', bit)?);
    }
    for (i, value) in outputs.iter().enumerate() {
        e.line(format!(
            "call void @__quantum__rt__bool_record_output(i1 {value}, ptr @out{i})"
        ));
    }
    e.code.push_str("  ret i64 0\ninvalid:\n  ret i64 3\nretry_exhausted:\n  ret i64 1\nwait_exhausted:\n  ret i64 2\n}\n");
    for i in 0..outputs.len() {
        let label = format!("out{i}");
        e.code.push_str(&format!(
            "@out{i} = private constant [{} x i8] c\"{label}\\00\"\n",
            label.len() + 1
        ));
    }
    let qubits = program
        .qubit_count
        .checked_add(u32::from(e.scratch))
        .ok_or_else(|| invalid("qubit count overflow"))?;
    e.code.push_str(&format!("attributes #0 = {{ \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"output_labeling_schema\"=\"ordered\" \"required_num_qubits\"=\"{qubits}\" \"required_num_results\"=\"{}\" }}\n", e.results));
    e.code.push_str(DECLARATIONS);
    e.code.push_str(crate::decoder_abi::DECLARATIONS);
    e.code.push_str("attributes #1 = { \"irreversible\" }\n!llvm.module.flags = !{!0, !1, !2, !3, !4, !6, !7}\n!0 = !{i32 1, !\"qir_major_version\", i32 2}\n!1 = !{i32 7, !\"qir_minor_version\", i32 1}\n!2 = !{i32 1, !\"dynamic_qubit_management\", i1 false}\n!3 = !{i32 1, !\"dynamic_result_management\", i1 false}\n!4 = !{i32 5, !\"int_computations\", !5}\n!5 = !{!\"i1\", !\"i32\", !\"i64\"}\n!6 = !{i32 1, !\"backwards_branching\", i2 2}\n!7 = !{i32 1, !\"multiple_return_points\", i1 true}\n");
    let llvm_ir = String::from_utf8(tool(
        &options.optimizer,
        &["-S", "-passes=mem2reg,verify", "-o", "-"],
        e.code.as_bytes(),
    )?)
    .map_err(|_| invalid("LLVM output is not UTF-8"))?;
    if llvm_ir.contains("alloca ") || llvm_ir.contains(" load ") || llvm_ir.contains(" store ") {
        return Err(invalid("SSA promotion left classical memory operations"));
    }
    let bitcode = tool(&options.assembler, &["-o", "-"], llvm_ir.as_bytes())?;
    Ok(QirArtifact {
        llvm_ir,
        bitcode,
        qubit_count: qubits,
        result_count: e.results,
    })
}

fn tool(path: &std::path::Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut child = Command::new(path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| invalid("LLVM stdin unavailable"))?
        .write_all(input)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(QirEmissionError::Llvm(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(output.stdout)
}

impl Emitter<'_> {
    pub(super) fn line(&mut self, text: impl AsRef<str>) {
        self.code.push_str("  ");
        self.code.push_str(text.as_ref());
        self.code.push('\n');
    }
    pub(super) fn value(&mut self, instruction: impl AsRef<str>) -> String {
        let name = format!("%v{}", self.next);
        self.next += 1;
        if instruction.as_ref().starts_with("alloca ") {
            let insertion = self
                .code
                .find("entry:\n")
                .expect("emitter owns an entry block")
                + "entry:\n".len();
            self.code
                .insert_str(insertion, &format!("  {name} = {}\n", instruction.as_ref()));
        } else {
            self.line(format!("{name} = {}", instruction.as_ref()));
        }
        name
    }
    pub(super) fn label(&mut self) -> String {
        let name = format!("l{}", self.next);
        self.next += 1;
        name
    }
    pub(super) fn block(&mut self, label: &str) {
        self.code.push_str(label);
        self.code.push_str(":\n");
    }
    pub(super) fn check(&mut self, valid: &str) {
        let next = self.label();
        self.line(format!("br i1 {valid}, label %{next}, label %invalid"));
        self.block(&next);
    }
    fn count(&self, kind: char) -> u32 {
        if kind == 'b' {
            self.program.bit_count
        } else {
            self.program.record_count
        }
    }
    pub(super) fn read(&mut self, kind: char, index: u32) -> Result<String> {
        if index >= self.count(kind) {
            return Err(invalid(format!("{kind}{index} exceeds register count")));
        }
        let valid = self.value(format!("load i1, ptr %{kind}v{index}"));
        self.check(&valid);
        Ok(self.value(format!("load i1, ptr %{kind}{index}")))
    }
    pub(super) fn write(&mut self, kind: char, index: u32, value: &str) -> Result<()> {
        if index >= self.count(kind) {
            return Err(invalid(format!("{kind}{index} exceeds register count")));
        }
        self.line(format!("store i1 {value}, ptr %{kind}{index}"));
        self.line(format!("store i1 true, ptr %{kind}v{index}"));
        Ok(())
    }
    fn clear(&mut self, kind: char, index: u32) -> Result<()> {
        if index >= self.count(kind) {
            return Err(invalid("invalid cleared register"));
        }
        self.line(format!("store i1 false, ptr %{kind}v{index}"));
        Ok(())
    }
    pub(super) fn parity(&mut self, parity: &RecordParity) -> Result<String> {
        let mut value = parity.constant.to_string();
        for &record in parity.records.iter() {
            let operand = self.read('r', record)?;
            value = self.value(format!("xor i1 {value}, {operand}"));
        }
        Ok(value)
    }
    fn bits(&mut self, bits: &[BitId]) -> Result<String> {
        let mut value = "false".to_owned();
        for &bit in bits {
            let operand = self.read('b', bit)?;
            value = self.value(format!("xor i1 {value}, {operand}"));
        }
        Ok(value)
    }
    fn boolean(&mut self, op: &BoolOp, args: Option<&[String]>, depth: u32) -> Result<String> {
        if depth > 256 {
            return Err(invalid("Boolean nesting exceeds 256"));
        }
        Ok(match op {
            BoolOp::Const(v) => v.to_string(),
            BoolOp::Copy(bit) => match args {
                Some(args) => args
                    .get(*bit as usize)
                    .cloned()
                    .ok_or_else(|| invalid("missing Boolean argument"))?,
                None => self.read('b', *bit)?,
            },
            BoolOp::Not(inner) => {
                let v = self.boolean(inner, args, depth + 1)?;
                self.value(format!("xor i1 {v}, true"))
            }
            BoolOp::Parity { inputs, constant } => {
                let mut v = constant.to_string();
                for &bit in inputs {
                    let rhs = self.boolean(&BoolOp::Copy(bit), args, depth + 1)?;
                    v = self.value(format!("xor i1 {v}, {rhs}"));
                }
                v
            }
            BoolOp::Xor(ops) | BoolOp::And(ops) | BoolOp::Or(ops) => {
                let (instruction, identity) = match op {
                    BoolOp::And(_) => ("and", "true"),
                    BoolOp::Or(_) => ("or", "false"),
                    _ => ("xor", "false"),
                };
                let mut v = identity.to_owned();
                for operand in ops {
                    let rhs = self.boolean(operand, args, depth + 1)?;
                    v = self.value(format!("{instruction} i1 {v}, {rhs}"));
                }
                v
            }
            BoolOp::Select {
                condition,
                when_false,
                when_true,
            } => {
                let condition = self.boolean(condition, args, depth + 1)?;
                let when_false = self.boolean(when_false, args, depth + 1)?;
                let when_true = self.boolean(when_true, args, depth + 1)?;
                self.value(format!(
                    "select i1 {condition}, i1 {when_true}, i1 {when_false}"
                ))
            }
            BoolOp::Call { body, inputs } => {
                let mut bound = Vec::new();
                for &input in inputs {
                    bound.push(self.boolean(&BoolOp::Copy(input), args, depth + 1)?);
                }
                self.boolean(body, Some(&bound), depth + 1)?
            }
        })
    }
    fn stream(&mut self, stream: &Stream) -> Result<()> {
        let mut remaining: BTreeSet<_> = stream.tasks.iter().copied().collect();
        if remaining.len() != stream.tasks.len() {
            return Err(invalid("duplicate stream member"));
        }
        while !remaining.is_empty() {
            let mut progress = false;
            for id in remaining.clone() {
                let task = self
                    .program
                    .tasks
                    .get(id as usize)
                    .ok_or_else(|| invalid("unknown task"))?;
                if !task.dependencies.iter().all(|dep| self.done.contains(dep)) {
                    continue;
                }
                if task.release != 0.0 || !task.duration.is_finite() || task.duration < 0.0 {
                    return Err(unsupported("source releases or invalid task durations"));
                }
                let execute = self.label();
                let skip = self.label();
                let join = self.label();
                if let Instruction::Quantum(quantum) = &task.instruction {
                    for (i, a) in quantum.alternatives.iter().enumerate() {
                        let assignments: BTreeMap<_, _> = a.when.iter().copied().collect();
                        if assignments.len() != a.when.len() {
                            return Err(invalid("duplicate alternative guard"));
                        }
                        for b in &quantum.alternatives[i + 1..] {
                            if !b.when.iter().any(|(bit, value)| {
                                assignments.get(bit).is_some_and(|other| other != value)
                            }) {
                                return Err(invalid("overlapping quantum alternatives"));
                            }
                        }
                    }
                }
                let branch_state = (task.activation.is_some()
                    && matches!(
                        task.instruction,
                        Instruction::WaitFor { .. } | Instruction::Rus { .. }
                    ))
                .then(|| (self.done.clone(), self.pending.clone(), self.cached.clone()));
                if let Some(gate) = task.activation {
                    let active = self.read('b', gate)?;
                    self.line(format!("br i1 {active}, label %{execute}, label %{skip}"));
                    self.block(&execute);
                }
                if task.activation.is_some() && matches!(task.instruction, Instruction::Decode(_)) {
                    return Err(unsupported("activated decoder requests"));
                }
                self.instruction(id, &task.instruction, task.output)?;
                if task.activation.is_some() {
                    self.line(format!("br label %{join}"));
                    self.block(&skip);
                    if let Instruction::Quantum(quantum) = &task.instruction {
                        for alternative in &quantum.alternatives {
                            for &record in &alternative.selected_records {
                                self.clear('r', record)?;
                            }
                        }
                    }
                    if let Some(output) = task.output {
                        self.write('b', output, "false")?;
                    }
                    self.line(format!("br label %{join}"));
                    self.block(&join);
                }
                if let Some((done, pending, cached)) = branch_state {
                    self.done = done;
                    self.pending = pending;
                    self.cached = cached;
                }
                if !matches!(task.instruction, Instruction::Decode(_))
                    || self
                        .cached
                        .contains_key(&decode_observable(&task.instruction))
                {
                    self.done.insert(id);
                }
                remaining.remove(&id);
                progress = true;
            }
            if !progress {
                if self.pending.is_empty() {
                    return Err(invalid("cyclic dependencies or a missing stream producer"));
                }
                self.finish_solves(&[])?;
            }
        }
        Ok(())
    }
    fn instruction(
        &mut self,
        id: TaskId,
        instruction: &Instruction,
        output: Option<BitId>,
    ) -> Result<()> {
        let mut result = None;
        match instruction {
            Instruction::Quantum(quantum) => {
                if quantum.alternatives.is_empty() {
                    return Err(invalid("quantum task has no alternative"));
                }
                let join = self.label();
                for alternative in &quantum.alternatives {
                    let mut selected = "true".to_owned();
                    for &(bit, expected) in &alternative.when {
                        let v = self.read('b', bit)?;
                        let v = self.value(format!("icmp eq i1 {v}, {expected}"));
                        selected = self.value(format!("and i1 {selected}, {v}"));
                    }
                    let yes = self.label();
                    let no = self.label();
                    self.line(format!("br i1 {selected}, label %{yes}, label %{no}"));
                    self.block(&yes);
                    for &record in &alternative.excluded_records {
                        self.clear('r', record)?;
                    }
                    self.quantum_stream(&alternative.stream, &alternative.restarts)?;
                    self.line(format!("br label %{join}"));
                    self.block(&no);
                }
                self.line("br label %invalid");
                self.block(&join);
            }
            Instruction::Eval(op) => result = Some(self.boolean(op, None, 0)?),
            Instruction::Accumulate(parity) => result = Some(self.parity(parity)?),
            Instruction::ReadoutRecipe { bits, .. } | Instruction::Observable { bits, .. } => {
                result = Some(self.bits(bits)?)
            }
            Instruction::Bind(_) | Instruction::SignalReady { .. } => {}
            Instruction::Decode(request) => {
                let raw = self.read('b', request.raw)?;
                if let Some((flip, raw_cached)) = self.cached.get(&request.observable).cloned() {
                    // The pair refers to one causal solve and identical raw data.
                    let equal = self.value(format!("icmp eq i1 {raw}, {raw_cached}"));
                    self.check(&equal);
                    result = Some(if request.output == ObservableOutput::Flip {
                        flip
                    } else {
                        self.value(format!("xor i1 {raw}, {flip}"))
                    });
                } else if let Some(pending) = self.pending.get(&request.observable) {
                    let previous_raw = pending.raw.clone();
                    let task = self.pending_decode(id);
                    self.pending
                        .get_mut(&request.observable)
                        .expect("pending solve")
                        .tasks
                        .push(task);
                    let equal = self.value(format!("icmp eq i1 {raw}, {previous_raw}"));
                    self.check(&equal);
                } else {
                    let binding = self
                        .options
                        .decoders
                        .iter()
                        .find(|binding| binding.observable == request.observable)
                        .cloned()
                        .ok_or_else(|| {
                            unsupported(format!(
                                "no decoder binding for observable {}",
                                request.observable
                            ))
                        })?;
                    if self
                        .pending
                        .values()
                        .any(|pending| pending.binding.decoder == binding.decoder)
                    {
                        return Err(unsupported(
                            "overlapping solves on the same decoder session",
                        ));
                    }
                    self.decoder_reset(binding.decoder);
                    for (chunk_index, chunk) in binding.syndrome.chunks(64).enumerate() {
                        let mut packed = "0".to_owned();
                        for (shift, parity) in chunk.iter().enumerate() {
                            let bit = self.parity(parity)?;
                            let wide = self.value(format!("zext i1 {bit} to i64"));
                            let shifted = self.value(format!("shl i64 {wide}, {shift}"));
                            packed = self.value(format!("or i64 {packed}, {shifted}"));
                        }
                        self.decoder_enqueue(binding.decoder, chunk.len(), &packed, chunk_index);
                    }
                    let consumed = self.value("alloca i1");
                    let flip = self.value("alloca i1");
                    self.line(format!("store i1 false, ptr {consumed}"));
                    self.line(format!("store i1 false, ptr {flip}"));
                    let task = self.pending_decode(id);
                    self.pending.insert(
                        request.observable,
                        PendingSolve {
                            binding,
                            raw,
                            tasks: vec![task],
                            consumed,
                            flip,
                        },
                    );
                }
            }
            Instruction::WaitFor { until, memory } => {
                if until.iter().any(|until| !self.done.contains(until)) {
                    if self
                        .pending
                        .values()
                        .any(|solve| solve.tasks.iter().all(|task| !until.contains(&task.task)))
                    {
                        return Err(unsupported(
                            "protection wait with an unrelated pending decoder session",
                        ));
                    }
                    self.finish_solves(memory)?;
                }
                if until.iter().any(|until| !self.done.contains(until)) {
                    return Err(unsupported(
                        "wait for a non-decoder task that has not executed",
                    ));
                }
            }
            Instruction::MemoryRounds { stream, .. } => self.quantum_stream(stream, &[])?,
            Instruction::Idle {
                duration,
                operations,
                ..
            } => {
                if *duration != 0.0 {
                    return Err(unsupported("explicit physical idle duration"));
                }
                for op in operations {
                    self.quantum_op(op)?;
                }
            }
            Instruction::Discard(condition) => {
                let reject = self.boolean(condition, None, 0)?;
                let next = self.label();
                self.line(format!(
                    "br i1 {reject}, label %retry_exhausted, label %{next}"
                ));
                self.block(&next);
            }
            Instruction::Rus {
                body,
                restart,
                attempt_bits,
                attempt_records,
                retry_prepare,
                decoder_hold,
                cultivation_exits,
                ..
            } => {
                if self.retry.is_some() {
                    return Err(unsupported("nested RUS"));
                }
                self.finish_solves(&[])?;
                let previous_done = self.done.clone();
                let previous_cached = self.cached.clone();
                let counter = self.value("alloca i32");
                self.line(format!("store i32 0, ptr {counter}"));
                let start = self.label();
                let retry = self.label();
                let accepted = self.label();
                self.line(format!("br label %{start}"));
                self.block(&start);
                let n = self.value(format!("load i32, ptr {counter}"));
                let allowed =
                    self.value(format!("icmp ult i32 {n}, {}", self.options.max_attempts));
                let body_start = self.label();
                self.line(format!(
                    "br i1 {allowed}, label %{body_start}, label %retry_exhausted"
                ));
                self.block(&body_start);
                for &bit in attempt_bits {
                    self.clear('b', bit)?;
                }
                for &record in attempt_records {
                    self.clear('r', record)?;
                }
                self.retry = Some(retry.clone());
                self.cached.clear();
                if !retry_prepare.moments.is_empty() {
                    return Err(unsupported(
                        "explicit retry preparation; author resets in the RUS body",
                    ));
                }
                if !cultivation_exits.is_empty() || !decoder_hold.is_empty() {
                    return Err(unsupported(
                        "factory GAP timing needs a separately calibrated execution contract",
                    ));
                }
                self.stream(body)?;
                self.finish_solves(&[])?;
                let restart = self.boolean(restart, None, 0)?;
                self.line(format!(
                    "br i1 {restart}, label %{retry}, label %{accepted}"
                ));
                self.block(&retry);
                let n = self.value(format!("load i32, ptr {counter}"));
                let next = self.value(format!("add i32 {n}, 1"));
                self.line(format!("store i32 {next}, ptr {counter}"));
                self.line(format!("br label %{start}"));
                self.block(&accepted);
                result = Some(match body.value {
                    Some(bit) => self.read('b', bit)?,
                    None => "false".to_owned(),
                });
                self.retry = None;
                self.done = previous_done;
                self.cached = previous_cached;
            }
        }
        if let (Some(output), Some(result)) = (output, result) {
            self.write('b', output, &result)?;
        }
        Ok(())
    }
    fn pending_decode(&mut self, task: TaskId) -> PendingDecode {
        let published = self.value("alloca i1");
        self.line(format!("store i1 false, ptr {published}"));
        PendingDecode { task, published }
    }

    fn finish_solves(&mut self, memory: &[MemoryCycle]) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if !memory.is_empty() {
            let counter = self.value("alloca i32");
            self.line(format!("store i32 0, ptr {counter}"));
            let poll = self.label();
            let round = self.label();
            let ready = self.label();
            self.line(format!("br label %{poll}"));
            self.block(&poll);
            let mut all = "true".to_owned();
            let decoders: Vec<_> = self
                .pending
                .values()
                .map(|pending| (pending.binding.decoder, pending.consumed.clone()))
                .collect();
            for (decoder, consumed) in decoders {
                let v = self.decoder_ready(decoder);
                let consumed = self.value(format!("load i1, ptr {consumed}"));
                let v = self.value(format!("or i1 {v}, {consumed}"));
                all = self.value(format!("and i1 {all}, {v}"));
            }
            self.line(format!("br i1 {all}, label %{ready}, label %{round}"));
            self.block(&round);
            let n = self.value(format!("load i32, ptr {counter}"));
            let allowed = self.value(format!(
                "icmp ult i32 {n}, {}",
                self.options.max_wait_rounds
            ));
            let next = self.label();
            self.line(format!(
                "br i1 {allowed}, label %{next}, label %wait_exhausted"
            ));
            self.block(&next);
            for cycle in memory {
                if !cycle.boundary_flows.is_empty() || !cycle.initializers.is_empty() {
                    return Err(unsupported("dynamic detector frontiers"));
                }
            }
            for stream in crate::cycle_streams(memory) {
                self.quantum_stream(stream, &[])?;
            }
            let n = self.value(format!("add i32 {n}, 1"));
            self.line(format!("store i32 {n}, ptr {counter}"));
            self.line(format!("br label %{poll}"));
            self.block(&ready);
        }
        let pending = std::mem::take(&mut self.pending);
        for (observable, solve) in pending {
            let consumed = self.value(format!("load i1, ptr {}", solve.consumed));
            let consume = self.label();
            let publish = self.label();
            self.line(format!(
                "br i1 {consumed}, label %{publish}, label %{consume}"
            ));
            self.block(&consume);
            let mask = self.decoder_consume(solve.binding.decoder, solve.binding.correction_count);
            let shifted = self.value(format!("lshr i64 {mask}, {}", solve.binding.correction_bit));
            let selected = self.value(format!("and i64 {shifted}, 1"));
            let flip = self.value(format!("icmp ne i64 {selected}, 0"));
            self.line(format!("store i1 {flip}, ptr {}", solve.flip));
            self.line(format!("store i1 true, ptr {}", solve.consumed));
            self.line(format!("br label %{publish}"));
            self.block(&publish);
            let flip = self.value(format!("load i1, ptr {}", solve.flip));
            for task in solve.tasks {
                let published = self.value(format!("load i1, ptr {}", task.published));
                let write = self.label();
                let next = self.label();
                self.line(format!("br i1 {published}, label %{next}, label %{write}"));
                self.block(&write);
                let item = &self.program.tasks[task.task as usize];
                let Instruction::Decode(request) = &item.instruction else {
                    return Err(invalid("pending task is not Decode"));
                };
                let result = if request.output == ObservableOutput::Flip {
                    flip.clone()
                } else {
                    self.value(format!("xor i1 {}, {flip}", solve.raw))
                };
                if let Some(output) = item.output {
                    self.write('b', output, &result)?;
                }
                self.line(format!("store i1 true, ptr {}", task.published));
                self.line(format!("br label %{next}"));
                self.block(&next);
                self.done.insert(task.task);
            }
            self.cached.insert(observable, (flip, solve.raw));
        }
        Ok(())
    }
    pub(super) fn quantum_stream(
        &mut self,
        stream: &QuantumStream,
        restarts: &[RecordParity],
    ) -> Result<()> {
        if !restarts.is_empty() && self.retry.is_none() {
            return Err(unsupported("restart parity outside RUS"));
        }
        let mut evaluated = Vec::new();
        for _ in restarts {
            let flag = self.value("alloca i1");
            self.line(format!("store i1 false, ptr {flag}"));
            evaluated.push(flag);
        }
        for moment in &stream.moments {
            if !moment.duration.is_finite() || moment.duration < 0.0 {
                return Err(invalid("invalid moment duration"));
            }
            for op in &moment.operations {
                self.quantum_op(op)?;
            }
            if let Some(retry) = self.retry.clone() {
                for (parity, flag) in restarts.iter().zip(&evaluated) {
                    let checked = self.value(format!("load i1, ptr {flag}"));
                    let mut available = self.value(format!("xor i1 {checked}, true"));
                    for &record in parity.records.iter() {
                        if record >= self.program.record_count {
                            return Err(invalid("restart record out of bounds"));
                        }
                        let v = self.value(format!("load i1, ptr %rv{record}"));
                        available = self.value(format!("and i1 {available}, {v}"));
                    }
                    let test = self.label();
                    let next = self.label();
                    self.line(format!("br i1 {available}, label %{test}, label %{next}"));
                    self.block(&test);
                    self.line(format!("store i1 true, ptr {flag}"));
                    let rejected = self.parity(parity)?;
                    self.line(format!("br i1 {rejected}, label %{retry}, label %{next}"));
                    self.block(&next);
                }
            }
        }
        Ok(())
    }
}

fn decode_observable(instruction: &Instruction) -> TaskId {
    match instruction {
        Instruction::Decode(request) => request.observable,
        _ => u32::MAX,
    }
}

const DECLARATIONS: &str = "declare void @__quantum__rt__initialize(ptr)\ndeclare i1 @__quantum__rt__read_result(ptr)\ndeclare void @__quantum__rt__bool_record_output(i1, ptr)\ndeclare void @__quantum__qis__h__body(ptr)\ndeclare void @__quantum__qis__x__body(ptr)\ndeclare void @__quantum__qis__z__body(ptr)\ndeclare void @__quantum__qis__s__body(ptr)\ndeclare void @__quantum__qis__s__adj(ptr)\ndeclare void @__quantum__qis__t__body(ptr)\ndeclare void @__quantum__qis__t__adj(ptr)\ndeclare void @__quantum__qis__cx__body(ptr, ptr)\ndeclare void @__quantum__qis__reset__body(ptr)\ndeclare void @__quantum__qis__mz__body(ptr, ptr) #1\n";
