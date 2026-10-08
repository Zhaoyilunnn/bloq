use bloq::{
    graph::GalleryItem,
    vm::{self, LoweringConfig},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ir = bloq::compile::compile(&GalleryItem::CNOT.build(), 3)?;
    let options = LoweringConfig::default();
    let program = vm::lower(&ir, &options)?;
    for seed in 0..3 {
        let result = program.run(options.runtime_config(seed))?;
        assert!(!result.artifact.discarded);
        println!(
            "{seed}: discarded={}, finished_at={}",
            result.artifact.discarded, result.artifact.finished_at
        );
        std::fs::write("cnot.trace.json", result.artifact.to_json_pretty()?)?;
    }
    std::fs::write("cnot.instructions.json", program.to_json_pretty()?)?;
    Ok(())
}
