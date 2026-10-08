use bloq::prelude::*;

fn main() -> Result {
    // [cnot-load-start]
    let cnot = GalleryItem::CNOT.build();
    println!("{}", cnot.to_blog_text());
    // [cnot-load-end]

    // [cnot-compile-start]
    let cnot_program = compile(&cnot, 3)?;
    std::fs::write("cnot.bloqir", cnot_program.to_text())?;
    // [cnot-compile-end]

    std::fs::write("cnot.svg", cnot_program.to_svg(true))?;

    // [cnot-emit-start]
    let stim_text = emit_bloq_stim(&cnot_program)?;
    std::fs::write("cnot.stim", stim_text)?;
    // [cnot-emit-end]

    // [t-load-start]
    let t_gate = GalleryItem::T.build();
    println!("{}", t_gate.to_blog_text());
    // [t-load-end]

    // [t-compile-start]
    let t_program = compile(&t_gate, 3)?;
    std::fs::write("t-gate.bloqir", t_program.to_text())?;
    // [t-compile-end]

    std::fs::write("t-gate.svg", t_program.to_svg(true))?;

    std::fs::write("quickstart-cnot.blog", cnot.to_blog_text())?;
    std::fs::write("quickstart-t.blog", t_gate.to_blog_text())?;
    for program in [&cnot_program, &t_program] {
        let restored = Bloq::from_text(&program.to_text())?;
        restored.validate()?;
        assert_eq!(restored.stats()?, program.stats()?);
    }
    assert!(cnot_program.stats()?.is_static);
    assert!(!t_program.stats()?.is_static);
    assert!(emit_bloq_stim(&t_program).is_err());
    Ok(())
}
