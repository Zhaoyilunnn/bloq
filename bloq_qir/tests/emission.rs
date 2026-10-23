#[path = "../examples/common/mod.rs"]
mod common;

use bloq_qir::{QirEmissionError, QirOptions, emit_program_qir};
use bloq_vm::instruction::*;

#[test]
fn conditional_decoder_completion_has_valid_ssa_on_both_paths() {
    for active in [false, true] {
        for rus in [false, true] {
            let (program, options) = common::conditional_wait(active, rus);
            emit_program_qir(&program, &options).expect("conditional completion and reuse");
        }
    }
}

#[test]
fn correction_selection_does_not_narrow_the_integer_mask() {
    let (program, mut options) = common::repetition(1);
    for bit in [0, 1, 63] {
        options.decoders[0].correction_count = 64;
        options.decoders[0].correction_bit = bit;
        let artifact = emit_program_qir(&program, &options).expect("emit mask selection");
        assert!(artifact.llvm_ir.contains("and i64"));
        assert!(artifact.llvm_ir.contains("icmp ne i64"));
        assert!(!artifact.llvm_ir.contains("trunc i64"));
    }
}

#[test]
fn qir_rejects_unowned_rus_support() {
    let (mut program, options) = common::retry();
    let Instruction::Rus { owned_qubits, .. } = &mut program.tasks[0].instruction else {
        panic!("RUS fixture")
    };
    *owned_qubits = Box::new([]);
    assert!(matches!(
        emit_program_qir(&program, &options),
        Err(QirEmissionError::InvalidProgram(message)) if message.contains("isolated source")
    ));
}

#[test]
fn native_llvm_verifies_feedback_phase_retry_and_decoder_loops() {
    for (program, options) in [
        common::feedback(),
        common::phase(),
        common::retry(),
        common::repetition(1),
        common::products(),
        common::early_retry(),
        common::decoder_publication(),
    ] {
        let artifact =
            emit_program_qir(&program, &options).expect("verify SSA and assemble bitcode");
        assert!(artifact.llvm_ir.contains("define i64 @bloq_entry"));
        assert!(artifact.llvm_ir.contains("!\"qir_major_version\", i32 2"));
        assert!(!artifact.llvm_ir.contains("alloca "));
        assert!(!artifact.llvm_ir.contains(" store "));
        assert_eq!(&artifact.bitcode[..4], b"BC\xc0\xde");
    }
}

#[test]
fn decode_pair_has_one_submission_and_one_consumption() {
    let (program, options) = common::repetition(1);
    let artifact = emit_program_qir(&program, &options).expect("emit paired decoder requests");
    assert_eq!(
        artifact
            .llvm_ir
            .matches("call void @enqueue_syndromes_ui64")
            .count(),
        1
    );
    assert_eq!(
        artifact
            .llvm_ir
            .matches("call i64 @get_corrections_ui64")
            .count(),
        1
    );
    assert_eq!(
        artifact
            .llvm_ir
            .matches("call i1 @decoder_ready_ui64")
            .count(),
        1
    );
}

#[test]
fn protection_wait_rejects_an_unrelated_pending_decoder() {
    let (mut program, mut options) = common::repetition(1);
    let mut observable = program.tasks[2].clone();
    observable.output = Some(4);
    let mut decode = program.tasks[3].clone();
    decode.dependencies = Box::new([7]);
    decode.output = Some(5);
    let Instruction::Decode(request) = &mut decode.instruction else {
        panic!("decoder fixture")
    };
    request.observable = 7;
    request.raw = 4;
    program.tasks.extend([observable, decode]);
    program.bit_count += 2;
    program.entry.tasks = (0..9).collect();
    program.tasks[5].dependencies = Box::new([2, 7]);
    let mut binding = options.decoders[0].clone();
    binding.observable = 7;
    binding.decoder = 1;
    options.decoders.push(binding);
    assert!(matches!(
        emit_program_qir(&program, &options),
        Err(QirEmissionError::Unsupported(message)) if message.contains("unrelated pending")
    ));
}

#[test]
fn missing_decoder_and_unsupported_noise_are_typed_errors() {
    let (program, _) = common::repetition(1);
    assert!(matches!(
        emit_program_qir(&program, &QirOptions::default()),
        Err(QirEmissionError::Unsupported(_))
    ));
    let (mut program, options) = common::feedback();
    let Instruction::Quantum(quantum) = &mut program.tasks[0].instruction else {
        panic!("quantum fixture")
    };
    quantum.alternatives[0].stream.moments[0].operations[0] = QuantumOp::Depolarize1 {
        probability: 0.1,
        qubits: Box::new([0]),
    };
    assert!(matches!(
        emit_program_qir(&program, &options),
        Err(QirEmissionError::Unsupported(_))
    ));
}

#[test]
fn cancelling_parity_and_unused_call_arguments_keep_validity_checks() {
    let op = BoolOp::Call {
        body: std::sync::Arc::new(BoolOp::Const(true)),
        inputs: Box::new([0]),
    };
    let program = Program {
        bit_count: 2,
        tasks: vec![common::task(Instruction::Eval(op), &[], Some(1))],
        entry: Stream {
            tasks: Box::new([0]),
            ..Stream::default()
        },
        ..Program::default()
    };
    let artifact = emit_program_qir(
        &program,
        &QirOptions {
            output_bits: vec![1],
            ..QirOptions::default()
        },
    )
    .expect("unknown input is explicit control, not poison");
    assert!(artifact.llvm_ir.contains("br i1 false"));
    let mut program = program;
    program.tasks[0].instruction = Instruction::Eval(BoolOp::Parity {
        inputs: Box::new([0, 0]),
        constant: false,
    });
    let artifact = emit_program_qir(&program, &QirOptions::default())
        .expect("retain duplicate input requirements");
    assert!(artifact.llvm_ir.matches("br i1 false").count() >= 2);
}

#[test]
fn a_memory_stream_is_not_repeated_by_its_rounds_metadata() {
    let program = Program {
        qubit_count: 1,
        tasks: vec![common::task(
            Instruction::MemoryRounds {
                rounds: 7,
                stream: common::stream(vec![QuantumOp::Pauli {
                    basis: Pauli::X,
                    qubit: 0,
                }]),
                detectors: Box::new([]),
            },
            &[],
            None,
        )],
        entry: Stream {
            tasks: Box::new([0]),
            ..Stream::default()
        },
        ..Program::default()
    };
    let artifact =
        emit_program_qir(&program, &QirOptions::default()).expect("emit preexpanded memory once");
    assert_eq!(
        artifact
            .llvm_ir
            .matches("call void @__quantum__qis__x__body")
            .count(),
        1
    );
}

#[test]
fn measurements_share_aliases_and_multi_pauli_uses_one_scratch_qubit() {
    let mut op = common::measure(0, 0);
    let QuantumOp::Measure {
        observable,
        records,
        ..
    } = &mut op
    else {
        panic!("measurement fixture")
    };
    observable.negative = true;
    observable.terms = Box::new([(0, Pauli::X), (1, Pauli::Y)]);
    *records = Box::new([0, 1]);
    let program = Program {
        qubit_count: 2,
        record_count: 2,
        tasks: vec![common::task(common::quantum(vec![op]), &[], None)],
        entry: Stream {
            tasks: Box::new([0]),
            ..Stream::default()
        },
        ..Program::default()
    };
    let artifact = emit_program_qir(
        &program,
        &QirOptions {
            output_records: vec![0, 1],
            ..QirOptions::default()
        },
    )
    .expect("QND signed product");
    assert_eq!(artifact.qubit_count, 3);
    assert_eq!(artifact.result_count, 1);
    assert_eq!(
        artifact
            .llvm_ir
            .matches("call void @__quantum__qis__mz__body")
            .count(),
        1
    );
}

// Input errors must be reported before starting external LLVM tools, including
// operands in tasks that the entry stream does not execute.
fn reject_before_llvm(program: &Program, options: &QirOptions) {
    let mut options = options.clone();
    options.optimizer = "/nonexistent/qir-validation-test-opt".into();
    assert!(matches!(
        emit_program_qir(program, &options),
        Err(QirEmissionError::InvalidProgram(_))
    ));
}

#[test]
fn malformed_arena_and_register_references_are_rejected_before_llvm() {
    let (base, options) = common::feedback();
    let mut program = base.clone();
    program.entry.tasks = Box::new([u32::MAX]);
    reject_before_llvm(&program, &options);
    for field in 0..4 {
        let mut program = base.clone();
        let mut task = program.tasks[0].clone();
        match field {
            0 => task.dependencies = Box::new([u32::MAX]),
            1 => task.activation = Some(program.bit_count),
            2 => task.output = Some(program.bit_count),
            _ => task.qubits = Box::new([program.qubit_count]),
        }
        program.tasks.push(task); // deliberately unreachable
        reject_before_llvm(&program, &options);
    }
    let mut bad_options = options.clone();
    bad_options.output_records.push(base.record_count);
    reject_before_llvm(&base, &bad_options);
    let mut bad_options = options;
    bad_options.output_bits.push(base.bit_count);
    reject_before_llvm(&base, &bad_options);
}

#[test]
fn rus_references_and_preparation_cannot_escape_isolation() {
    for case in 0..5 {
        let (mut program, options) = common::retry();
        let extra_qubit = program.qubit_count;
        program.qubit_count += 1;
        let Instruction::Rus {
            body,
            owned_qubits,
            retry_prepare,
            cultivation_exits,
            ..
        } = &mut program.tasks[0].instruction
        else {
            panic!("RUS fixture")
        };
        match case {
            0 => body.tasks = Box::new([u32::MAX]),
            1 => owned_qubits.clone_from(&Box::<[u32]>::from([0, 0])),
            2 => {
                retry_prepare.moments = Box::new([Moment {
                    operations: Box::new([QuantumOp::Reset {
                        basis: Pauli::Z,
                        qubit: extra_qubit,
                    }]),
                    ..Moment::default()
                }])
            }
            3 => cultivation_exits.clone_from(&Box::<[u32]>::from([0])),
            _ => body.tasks = Box::new([0]), // recursive RUS
        }
        reject_before_llvm(&program, &options);
    }
}

#[test]
fn invalid_quantum_operands_and_decoder_syndromes_are_rejected_before_llvm() {
    let (base, options) = common::feedback();
    for operation in [
        QuantumOp::Reset {
            basis: Pauli::Z,
            qubit: base.qubit_count,
        },
        QuantumOp::Gate2 {
            control_basis: Pauli::Z,
            target_basis: Pauli::X,
            control: 0,
            target: 0,
        },
        QuantumOp::Measure {
            observable: PauliProduct::default(),
            records: Box::new([]),
            flip_probability: 0.0,
        },
        QuantumOp::ConditionalPauli {
            basis: Pauli::X,
            qubit: 0,
            control: base.record_count,
        },
    ] {
        let mut program = base.clone();
        let mut task = program.tasks[0].clone();
        task.instruction = Instruction::MemoryRounds {
            rounds: 1,
            stream: QuantumStream {
                moments: Box::new([Moment {
                    operations: Box::new([operation]),
                    ..Moment::default()
                }]),
            },
            detectors: Box::new([]),
        };
        program.tasks.push(task);
        reject_before_llvm(&program, &options);
    }
    let (program, mut options) = common::repetition(1);
    options.decoders[0].syndrome[0].records = vec![program.record_count].into();
    reject_before_llvm(&program, &options);
}

#[test]
fn rus_checks_instruction_support_even_without_task_qubit_metadata() {
    let (mut program, options) = common::retry();
    let Instruction::Rus { body, .. } = &program.tasks[0].instruction else {
        panic!("RUS fixture")
    };
    let member = body.tasks[0] as usize;
    let outside = program.qubit_count;
    program.qubit_count += 1;
    program.tasks[member].qubits = Box::new([]);
    program.tasks[member].instruction = common::quantum(vec![QuantumOp::Reset {
        basis: Pauli::Z,
        qubit: outside,
    }]);
    reject_before_llvm(&program, &options);
}

#[test]
fn zero_probability_noise_remains_a_valid_noop() {
    let (mut program, options) = common::feedback();
    program.tasks[0].instruction = common::quantum(vec![QuantumOp::Depolarize1 {
        probability: 0.0,
        qubits: Box::new([0]),
    }]);
    emit_program_qir(&program, &options).expect("zero-probability noise is ignored");
}
