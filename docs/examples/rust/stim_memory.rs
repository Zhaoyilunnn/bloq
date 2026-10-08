use bloq::{
    circuit::NoiseModel,
    graph::GalleryItem,
    stim::{BloqStimOptions, emit_bloq_stim, emit_bloq_stim_with},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // [example-start]
    let program = bloq::compile::compile(&GalleryItem::XMemory.build(), 3)?;
    let clean = emit_bloq_stim(&program)?;
    assert!(clean.contains("DETECTOR"));
    std::fs::write("memory.clean.stim", clean)?;
    let noise = NoiseModel::uniform_depolarizing(0.001);
    let options = BloqStimOptions::new().with_noise(&noise);
    let text = emit_bloq_stim_with(&program, &options)?;
    assert!(text.contains("DEPOLARIZE1"));
    std::fs::write("memory.stim", text)?;
    // [example-end]
    Ok(())
}
