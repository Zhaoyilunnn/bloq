// [build-start]
use bloq::graph::ModuleJoinOptions;
use bloq::prelude::*;

fn build_graph() -> Result<BlockGraph> {
    let mut definitions = Vec::new();
    for (name, item) in [
        ("AND", GalleryItem::CCZInjectedAnd),
        ("MAJ", GalleryItem::CCZInjectedMaj),
        ("UMA", GalleryItem::UMA),
    ] {
        let mut definition = item.build();
        definition.name = name.into();
        definitions.push(definition);
    }
    definitions.push(BlockGraph::new());
    let mut graph = BlockGraph::from_definitions(definitions)?;
    for (index, name) in ["AND", "MAJ", "UMA"].into_iter().enumerate() {
        graph.place_module(&format!("child{index}"), name)?;
    }
    graph.connect_modules_with(
        "child0.qi_k",
        "child1.i_prime_k",
        ModuleJoinOptions {
            compact: false,
            ..Default::default()
        },
    )?;
    graph.connect_modules("child1.c_k_out", "child2.c_k")?;
    Ok(graph)
}
// [build-end]

fn main() -> Result {
    let graph = build_graph()?;
    graph.validate_source()?;
    graph.to_file("module-controlled-adder.blog")?;
    #[cfg(feature = "gltf")]
    graph.write_module_html_viewer(2.0, "module-controlled-adder.html", &[])?;
    Ok(())
}
