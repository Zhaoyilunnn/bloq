use bloq::prelude::*;

fn main() -> Result {
    let graph = BlockGraph::load("t-gate.blog")?;
    let program = compile(&graph, 11)?;
    std::fs::write("t-gate.bloqir", program.to_text())?;
    println!("{}", program.stats()?);
    Ok(())
}
