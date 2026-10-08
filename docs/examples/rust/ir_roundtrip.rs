fn main() -> Result<(), Box<dyn std::error::Error>> {
    // [example-start]
    use bloq::{graph::GalleryItem, ir::Bloq};

    let program = bloq::compile::compile(&GalleryItem::XMemory.build(), 3)?;
    std::fs::write("memory.bloq", program.to_binary())?;
    let loaded = Bloq::from_binary(&std::fs::read("memory.bloq")?)?;
    std::fs::write("memory.bloqir", loaded.to_text())?;
    // [example-end]

    let text = program.to_text();
    let binary = program.to_binary();
    let from_text = Bloq::from_text(&text)?;
    let from_binary = Bloq::from_binary(&binary)?;
    let from_file = Bloq::from_text(&std::fs::read_to_string("memory.bloqir")?)?;
    for restored in [loaded, from_text, from_binary, from_file] {
        restored.validate()?;
        assert_eq!(restored.to_binary(), binary);
        assert_eq!(restored.to_text(), text);
    }
    Ok(())
}
