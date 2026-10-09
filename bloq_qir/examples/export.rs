//! Export VM workloads for the compiler-to-simulator integration test.
mod common;

use bloq_qir::{DecoderBinding, QirOptions, emit_program_qir};
use bloq_vm::instruction::*;
use serde_json::json;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: export OUTPUT_DIRECTORY")?,
    );
    std::fs::create_dir_all(&directory)?;
    let mut cases = vec![
        ("feedback".to_owned(), common::feedback(), "stim"),
        ("phase".to_owned(), common::phase(), "aer"),
        ("retry".to_owned(), common::retry(), "stim"),
        ("early-retry".to_owned(), common::early_retry(), "stim"),
    ];
    cases.push(("products".to_owned(), common::products(), "aer"));
    cases.push((
        "conditional-overwrite".to_owned(),
        common::decoder_publication(),
        "stim",
    ));
    for active in [false, true] {
        for rus in [false, true] {
            let boundary = if rus { "rus" } else { "wait" };
            cases.push((
                format!("conditional-{boundary}-{active}"),
                common::conditional_wait(active, rus),
                "stim",
            ));
        }
    }
    for bit in 0..3 {
        let (mut program, mut options) = common::repetition(1);
        program.entry.tasks = (0..6).collect();
        options.decoders[0].correction_bit = bit;
        options.output_records = vec![0, 1];
        cases.push((format!("mask-bit-{bit}"), (program, options), "stim"));
    }
    let (mut rejected, mut retry_options) = common::retry();
    if let Instruction::Rus { restart, .. } = &mut rejected.tasks[0].instruction {
        *restart = BoolOp::Const(true);
    }
    retry_options.max_attempts = 3;
    cases.push((
        "retry-exhausted".to_owned(),
        (rejected, retry_options),
        "stim",
    ));
    let (waiting, mut wait_options) = common::repetition(1);
    wait_options.max_wait_rounds = 1;
    cases.push(("wait-exhausted".to_owned(), (waiting, wait_options), "stim"));
    for error in 0..3 {
        cases.push((
            format!("repetition-{error}"),
            common::repetition(error),
            "stim",
        ));
    }
    let ir = bloq_compile::compile(&bloq_graph::GalleryItem::XMemory.build(), 3)?;
    let mut program = bloq_vm::lower(&ir, &bloq_vm::LoweringConfig::default())?;
    let observable = program
        .tasks
        .iter()
        .position(|task| matches!(task.instruction, Instruction::Observable { .. }))
        .ok_or("missing memory observable")? as u32;
    let raw = program.tasks[observable as usize]
        .output
        .ok_or("missing raw output")?;
    let corrected = program.bit_count;
    program.bit_count += 1;
    let decode = program.tasks.len() as u32;
    program.tasks.push(common::task(
        Instruction::Decode(DecodeRequest {
            output: bloq_ir::ObservableOutput::Corrected,
            observable,
            index: 0,
            raw,
            round_duration: 1.0,
            timing: DecodeTiming::Measurement,
        }),
        &[observable],
        Some(corrected),
    ));
    program.task_origins.push(TaskOrigin {
        function: TaskFunction::Classical,
        ..TaskOrigin::default()
    });
    program.entry.tasks = program
        .entry
        .tasks
        .iter()
        .copied()
        .chain([decode])
        .collect();
    let options = QirOptions {
        decoders: vec![DecoderBinding {
            observable,
            decoder: 0,
            syndrome: (0..program.record_count)
                .map(|record| RecordParity {
                    records: vec![record].into(),
                    constant: false,
                })
                .collect(),
            correction_count: 1,
            correction_bit: 0,
        }],
        output_records: (0..program.record_count).collect(),
        output_bits: vec![raw, corrected],
        ..QirOptions::default()
    };
    let noise = bloq_ir::circuit::NoiseModel::uniform_depolarizing(0.001);
    let reference =
        bloq_stim::emit_bloq_stim_with(&ir, &bloq_stim::BloqStimOptions::new().with_noise(&noise))?;
    std::fs::write(directory.join("memory.reference.stim"), reference)?;
    cases.push(("memory".to_owned(), (program, options), "stim"));
    let mut manifest = Vec::new();
    for (name, (program, options), backend) in cases {
        let artifact = emit_program_qir(&program, &options)?;
        std::fs::write(directory.join(format!("{name}.ll")), &artifact.llvm_ir)?;
        std::fs::write(directory.join(format!("{name}.bc")), &artifact.bitcode)?;
        std::fs::write(
            directory.join(format!("{name}.vm.json")),
            program.to_json_pretty()?,
        )?;
        // The VM reference uses its stochastic policy only for a diagnostic run.
        // Real decoder correction masks are independently checked by PyMatching.
        if options.decoders.is_empty() {
            let result = program.run(bloq_vm::RuntimeConfig {
                seed: 7,
                ..Default::default()
            })?;
            std::fs::write(
                directory.join(format!("{name}.vm-trace.json")),
                result.artifact.to_json_pretty()?,
            )?;
        }
        manifest.push(json!({"name": name, "backend": backend, "qubits": artifact.qubit_count, "results": artifact.result_count}));
    }
    std::fs::write(
        directory.join("workloads.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(())
}
