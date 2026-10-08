use bloq::{
    graph::GalleryItem,
    vm::{self, LoweringConfig, SourceTiming},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // [example-start]
    let ir = bloq::compile::compile(&GalleryItem::THTH.build(), 3)?;
    let options = LoweringConfig {
        gate_duration: 1.0,
        decoder_latency_rounds: 10,
        source_timing: SourceTiming {
            factory: 0.0,
            input: 23.0,
            clifford: 23.0,
        },
        ..Default::default()
    };
    let program = vm::lower(&ir, &options)?;
    let mut runtime = options.runtime_config(17);
    runtime.decoder.acceptance_probability = 1.0;
    runtime.decoder.accepted_accuracy = 1.0;
    runtime.decoder.rejected_accuracy = 1.0;
    let result = program.run(runtime)?;
    assert!(!result.artifact.discarded);
    let actual = result.logical_bloch(&program.outputs[0])?;
    let expected = (0.5_f64.sqrt(), 0.5, 0.5);
    assert!((actual.0 - expected.0).abs() < 1e-9);
    assert!((actual.1 - expected.1).abs() < 1e-9);
    assert!((actual.2 - expected.2).abs() < 1e-9);
    std::fs::write("thth.instructions.json", program.to_json_pretty()?)?;
    std::fs::write("thth.trace.json", result.artifact.to_json_pretty()?)?;
    println!("logical Bloch vector: {actual:?}");
    println!("finished at: {}", result.artifact.finished_at);
    // [example-end]
    Ok(())
}
