fn main() -> bloq::Result<()> {
    // [dag-start]
    use bloq::prelude::*;

    let source = GalleryItem::ThreeBitAdder.build();
    let dag = source.analyze_action_graph()?;

    std::fs::write("three-bit-adder-action-dag.svg", dag.to_svg()?)?;
    // [dag-end]
    Ok(())
}
