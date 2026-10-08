//! Static reference oracle and source acceptance for continuing branches.
use bloq_compile::{CompileConfig, CompileContext};
use bloq_graph::BlockGraph;
use bloq_graph::ast::{
    ActionStmt, BranchArmStmt, DataStmt, Expr, InterfaceStmt, ResolveTargetDef, Spanned,
};
use bloq_graph::verify::{BoundaryOrder, LogicalVerifier, QuizxGraph};
use glam::IVec3;
use quizx::circuit::Circuit;

const SOURCE: &str = include_str!("../../docs/fixtures/conditional_cz_strip.blog");

fn strip(source: &str, mask: u8) -> Result<BlockGraph, Box<dyn std::error::Error>> {
    let mut ast = bloq_graph::parse_blog_program_to_ast(source)?;
    let module = &mut ast.modules[0].node;
    assert_eq!(module.action_stmts.len(), 3);
    for (bit, action) in module.action_stmts.iter().enumerate() {
        let ActionStmt::Resolve(resolve) = &action.node else {
            panic!("three selectors")
        };
        assert_eq!(
            resolve.target.node,
            ResolveTargetDef::Branch(format!("cz{bit}"))
        );
        assert_eq!(resolve.condition.node, Expr::Var(format!("enable{bit}")));
    }
    module.action_stmts.clear();
    module
        .interface_stmts
        .retain(|statement| !matches!(statement.node, InterfaceStmt::BitInput(_)));
    let mut seen = 0u8;
    for statement in std::mem::take(&mut module.data_stmts) {
        match statement.node {
            DataStmt::Branch(branch) => {
                let bit: u8 = branch
                    .name
                    .node
                    .strip_prefix("cz")
                    .ok_or("branch name must start with cz")?
                    .parse()?;
                assert!(bit < 3 && seen & (1 << bit) == 0);
                seen |= 1 << bit;
                let arm = if mask & (1 << bit) != 0 {
                    branch.on_true
                } else {
                    branch.on_false
                };
                module.data_stmts.extend(arm.into_iter().map(|statement| {
                    let node = match statement.node {
                        BranchArmStmt::Block(block) => DataStmt::Block(block),
                        BranchArmStmt::Pipe(pipe) => DataStmt::Pipe(pipe),
                    };
                    Spanned::new(node, statement.span)
                }));
            }
            node => module.data_stmts.push(Spanned::new(node, statement.span)),
        }
    }
    assert_eq!(seen, 0b111);
    Ok(bloq_graph::lower_blog_graph_ast_deferred(&ast)?
        .materialize_flat_graph()?
        .fix_shadowed_faces())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Keep the arm's outgoing adapter; put an unconditional H after its exit.
    // H on i0 does not commute with CZ(q, i0), so order matters to the oracle.
    let continued = SOURCE.replace(
        "3: Port [0, 0, 1] role=output",
        "3: Port [0, 0, 2] role=output\n  8: XZX [0, 0, 1]\n  [0, 0, 1] -H> +Z",
    );
    assert_ne!(continued, SOURCE);
    let compilers =
        [3, 5].map(|distance| (distance, CompileContext::new(CompileConfig::new(distance))));
    for (label, source, has_continuation) in [
        ("strip", SOURCE, false),
        ("H continuation", continued.as_str(), true),
    ] {
        check_masks(label, source, has_continuation, &compilers)?;
        let ast = bloq_graph::parse_blog_program_to_ast(source)?;
        let program = bloq_graph::lower_blog_graph_ast_deferred(&ast)?;
        assert_eq!(program.root().local_body().branch_regions()?.len(), 3);
        println!("Continuing source accepted: {label}");
    }
    Ok(())
}

fn check_masks(
    label: &str,
    source: &str,
    has_continuation: bool,
    compilers: &[(u32, CompileContext)],
) -> Result<(), Box<dyn std::error::Error>> {
    let inputs = std::iter::once(IVec3::new(1, -1, 0))
        .chain((0..3).map(|row| IVec3::new(0, row, -1)))
        .collect();
    let outputs = std::iter::once(IVec3::new(1, 3, 0))
        .chain((0..3).map(|row| IVec3::new(0, row, 1 + i32::from(has_continuation && row == 0))))
        .collect();
    let boundaries = BoundaryOrder::new(inputs, outputs);
    for mask in 0..8 {
        let graph = strip(source, mask)?;
        graph.validate_structure()?;
        assert_eq!(
            graph
                .pipes()
                .filter(|pipe| pipe.src().x != pipe.dst().x)
                .count(),
            mask.count_ones() as usize,
        );
        assert_eq!(
            graph.pipes().filter(|pipe| pipe.is_hadamard()).count(),
            2 * mask.count_ones() as usize + usize::from(has_continuation),
        );
        let mut expected = Circuit::new(4);
        for bit in 0..3 {
            if mask & (1 << bit) != 0 {
                expected.add_gate("cz", vec![0, bit + 1]);
            }
        }
        if has_continuation {
            expected.add_gate("h", vec![1]);
        }
        let expected: QuizxGraph = expected.to_graph();
        let report = LogicalVerifier::with_boundaries(&graph, boundaries.clone())?.verify(
            &expected,
            32,
            0xC2 + u64::from(mask),
        )?;
        assert_eq!(report.verified, 32);
        for (distance, compiler) in compilers {
            let bloq = compiler.compile(&graph)?.bloq;
            bloq.validate()?;
            assert_eq!(bloq.logical_inputs().len(), 4);
            assert_eq!(bloq.logical_outputs().len(), 4);
            println!("{label} mask {mask:03b}: d{distance} compilation passes");
        }
        println!(
            "{label} mask {mask:03b}: {} ZX branches verified",
            report.verified
        );
    }
    Ok(())
}
