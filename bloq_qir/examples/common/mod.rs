use bloq_ir::ObservableOutput;
use bloq_qir::{DecoderBinding, QirOptions};
use bloq_vm::instruction::*;

pub(crate) fn task(
    instruction: Instruction,
    dependencies: &[TaskId],
    output: Option<BitId>,
) -> Task {
    Task {
        label: "example".into(),
        release: 0.0,
        source: None,
        dependencies: dependencies.into(),
        activation: None,
        output,
        qubits: Box::new([]),
        duration: 0.0,
        instruction,
    }
}

pub(crate) fn stream(operations: Vec<QuantumOp>) -> QuantumStream {
    QuantumStream {
        moments: vec![Moment {
            kind: None,
            duration: 1.0,
            operations: operations.into_boxed_slice(),
        }]
        .into_boxed_slice(),
    }
}

pub(crate) fn quantum(operations: Vec<QuantumOp>) -> Instruction {
    Instruction::Quantum(QuantumTask {
        alternatives: vec![QuantumAlternative {
            stream: stream(operations),
            ..QuantumAlternative::default()
        }]
        .into_boxed_slice(),
    })
}

pub(crate) fn measure(qubit: u32, record: u32) -> QuantumOp {
    QuantumOp::Measure {
        observable: PauliProduct {
            negative: false,
            terms: Box::new([(qubit, Pauli::Z)]),
        },
        records: Box::new([record]),
        flip_probability: 0.0,
    }
}

fn cx(control: u32, target: u32) -> QuantumOp {
    QuantumOp::Gate2 {
        control_basis: Pauli::Z,
        target_basis: Pauli::X,
        control,
        target,
    }
}

pub(crate) fn feedback() -> (Program, QirOptions) {
    let operations = vec![
        QuantumOp::Pauli {
            basis: Pauli::X,
            qubit: 0,
        },
        measure(0, 0),
        QuantumOp::ConditionalPauli {
            basis: Pauli::X,
            qubit: 1,
            control: 0,
        },
        measure(1, 1),
    ];
    let program = Program {
        qubit_count: 2,
        record_count: 2,
        tasks: vec![task(quantum(operations), &[], None)],
        entry: Stream {
            tasks: Box::new([0]),
            ..Stream::default()
        },
        ..Program::default()
    };
    (
        program,
        QirOptions {
            output_records: vec![0, 1],
            ..QirOptions::default()
        },
    )
}

pub(crate) fn phase() -> (Program, QirOptions) {
    let mut operations = Vec::new();
    for qubit in 0..2 {
        operations.push(QuantumOp::Gate1 {
            gate: Clifford1::H,
            qubit,
        });
        for _ in 0..2 {
            operations.push(QuantumOp::T {
                basis: Pauli::Z,
                qubit,
                adjoint: qubit == 1,
            });
        }
        operations.push(QuantumOp::Gate1 {
            gate: if qubit == 0 {
                Clifford1::S_DAG
            } else {
                Clifford1::S
            },
            qubit,
        });
        operations.push(QuantumOp::Gate1 {
            gate: Clifford1::H,
            qubit,
        });
        operations.push(measure(qubit, qubit));
    }
    (
        Program {
            qubit_count: 2,
            record_count: 2,
            tasks: vec![task(quantum(operations), &[], None)],
            entry: Stream {
                tasks: Box::new([0]),
                ..Stream::default()
            },
            ..Program::default()
        },
        QirOptions {
            output_records: vec![0, 1],
            ..QirOptions::default()
        },
    )
}

pub(crate) fn repetition(error: u32) -> (Program, QirOptions) {
    let syndrome = vec![
        cx(0, 3),
        cx(1, 3),
        cx(1, 4),
        cx(2, 4),
        measure(3, 0),
        measure(4, 1),
        measure(error, 2),
    ];
    let mut preparation = vec![QuantumOp::Pauli {
        basis: Pauli::X,
        qubit: error,
    }];
    preparation.extend(syndrome);
    let memory = MemoryCycle {
        stream: stream(vec![
            QuantumOp::Reset {
                basis: Pauli::Z,
                qubit: 3,
            },
            QuantumOp::Reset {
                basis: Pauli::Z,
                qubit: 4,
            },
            cx(0, 3),
            cx(1, 3),
            cx(1, 4),
            cx(2, 4),
            measure(3, 3),
            measure(4, 4),
        ]),
        round_duration: 1.0,
        ..MemoryCycle::default()
    };
    let decode = |output| {
        Instruction::Decode(DecodeRequest {
            output,
            observable: 2,
            index: 0,
            raw: 1,
            round_duration: 1.0,
            timing: DecodeTiming::Measurement,
        })
    };
    let mut alternatives = Vec::new();
    for flip in [false, true] {
        let mut operations = Vec::new();
        if flip {
            operations.push(QuantumOp::Pauli {
                basis: Pauli::X,
                qubit: error,
            });
        }
        operations.extend([measure(0, 5), measure(1, 6), measure(2, 7)]);
        alternatives.push(QuantumAlternative {
            when: Box::new([(3, flip)]),
            stream: stream(operations),
            ..QuantumAlternative::default()
        });
    }
    let tasks = vec![
        task(quantum(preparation), &[], None),
        task(
            Instruction::Accumulate(RecordParity {
                records: vec![2].into(),
                constant: false,
            }),
            &[0],
            Some(0),
        ),
        task(
            Instruction::Observable {
                index: 0,
                bits: Box::new([0]),
                bindings: Box::new([]),
            },
            &[1],
            Some(1),
        ),
        task(decode(ObservableOutput::Corrected), &[2], Some(2)),
        task(decode(ObservableOutput::Flip), &[2], Some(3)),
        task(
            Instruction::WaitFor {
                until: Box::new([3, 4]),
                memory: Box::new([memory]),
            },
            &[0],
            None,
        ),
        task(
            Instruction::Quantum(QuantumTask {
                alternatives: alternatives.into_boxed_slice(),
            }),
            &[3, 4, 5],
            None,
        ),
    ];
    let program = Program {
        qubit_count: 5,
        bit_count: 4,
        record_count: 8,
        tasks,
        entry: Stream {
            tasks: (0..7).collect(),
            ..Stream::default()
        },
        ..Program::default()
    };
    let options = QirOptions {
        decoders: vec![DecoderBinding {
            observable: 2,
            decoder: 0,
            syndrome: vec![
                RecordParity {
                    records: vec![0].into(),
                    constant: false,
                },
                RecordParity {
                    records: vec![1].into(),
                    constant: false,
                },
            ]
            .into_boxed_slice(),
            correction_count: 3,
            correction_bit: error,
        }],
        output_records: vec![0, 1, 2, 5, 6, 7],
        output_bits: vec![2, 3],
        ..QirOptions::default()
    };
    (program, options)
}

pub(crate) fn retry() -> (Program, QirOptions) {
    let mut body = task(
        quantum(vec![
            QuantumOp::Reset {
                basis: Pauli::X,
                qubit: 0,
            },
            measure(0, 0),
        ]),
        &[],
        None,
    );
    body.qubits = Box::new([0]);
    body.duration = 1.0;
    let tasks = vec![
        task(
            Instruction::Rus {
                body: Stream {
                    tasks: Box::new([1, 2, 3]),
                    value: Some(1),
                    bindings: Box::new([]),
                },
                restart: BoolOp::Copy(0),
                owned_qubits: Box::new([0]),
                attempt_bits: Box::new([0, 1]),
                attempt_records: Box::new([0]),
                resource: 0,
                retry_prepare: QuantumStream::default(),
                decoder_hold: Box::new([]),
                cultivation_exits: Box::new([]),
            },
            &[],
            Some(2),
        ),
        body,
        task(
            Instruction::Accumulate(RecordParity {
                records: vec![0].into(),
                constant: false,
            }),
            &[1],
            Some(0),
        ),
        task(Instruction::Eval(BoolOp::Const(true)), &[2], Some(1)),
    ];
    (
        Program {
            qubit_count: 1,
            bit_count: 3,
            record_count: 1,
            tasks,
            entry: Stream {
                tasks: Box::new([0]),
                ..Stream::default()
            },
            ..Program::default()
        },
        QirOptions {
            output_bits: vec![2],
            ..QirOptions::default()
        },
    )
}

pub(crate) fn early_retry() -> (Program, QirOptions) {
    let (mut program, mut options) = retry();
    let Instruction::Rus { restart, .. } = &mut program.tasks[0].instruction else {
        unreachable!("retry fixture")
    };
    *restart = BoolOp::Const(false);
    let Instruction::Quantum(quantum) = &mut program.tasks[1].instruction else {
        unreachable!("retry body")
    };
    // Once a restart check accepts its first available parity, overwriting
    // that record later must not evaluate the same check again.
    quantum.alternatives[0].restarts = Box::new([RecordParity {
        records: vec![0].into(),
        constant: false,
    }]);
    let mut moments = stream(vec![
        QuantumOp::Reset {
            basis: Pauli::Z,
            qubit: 0,
        },
        measure(0, 0),
    ])
    .moments
    .into_vec();
    moments.extend(
        stream(vec![
            QuantumOp::Pauli {
                basis: Pauli::X,
                qubit: 0,
            },
            measure(0, 0),
        ])
        .moments,
    );
    quantum.alternatives[0].stream.moments = moments.into_boxed_slice();
    program.tasks[1].duration = 2.0;
    options.output_records = vec![0];
    (program, options)
}

pub(crate) fn products() -> (Program, QirOptions) {
    let product = |basis, negative, records: Vec<u32>| QuantumOp::Measure {
        observable: PauliProduct {
            negative,
            terms: Box::new([(0, basis), (1, basis)]),
        },
        records: records.into_boxed_slice(),
        flip_probability: 0.0,
    };
    let operations = vec![
        QuantumOp::Gate1 {
            gate: Clifford1::H,
            qubit: 0,
        },
        cx(0, 1),
        product(Pauli::X, false, vec![0, 1]),
        product(Pauli::Y, true, vec![2]),
        product(Pauli::Z, false, vec![3]),
        measure(0, 4),
        measure(1, 5),
    ];
    (
        Program {
            qubit_count: 2,
            record_count: 6,
            tasks: vec![task(quantum(operations), &[], None)],
            entry: Stream {
                tasks: Box::new([0]),
                ..Stream::default()
            },
            ..Program::default()
        },
        QirOptions {
            output_records: (0..6).collect(),
            ..QirOptions::default()
        },
    )
}
