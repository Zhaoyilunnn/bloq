//! Cold classical-storage probe: `profile_classical_storage WIDTH|yoke:WIDTH|GALLERY [TEXT_PATH]`.
//! Compilation, codec and VM preparation are timed separately; no shots execute.

use std::collections::HashSet;
use std::time::Instant;

use bloq_compile::{CompileConfig, CompileContext};
use bloq_ir::ClassicalNode;

fn main() {
    let case = std::env::args()
        .nth(1)
        .expect("width, yoke:width or gallery slug");
    let graph = if let Some(width) = case.strip_prefix("yoke:") {
        bloq_test::benchmark::yoked_memory(width.parse().expect("yoked memory width"))
    } else if let Ok(bits) = case.parse::<usize>() {
        assert!(bits >= 3);
        bloq_test::benchmark::controlled_adder(bits)
    } else {
        case.parse::<bloq_graph::GalleryItem>()
            .expect("gallery slug")
            .build()
    };
    let start = Instant::now();
    let compiled = CompileContext::new(CompileConfig::new(3))
        .compile(&graph)
        .expect("compile");
    let compile_ms = start.elapsed().as_secs_f64() * 1000.0;
    eprintln!("compiled {case} in {compile_ms} ms");
    let mut definitions = HashSet::new();
    let mut stored_terms = 0;
    for (_, level) in compiled.bloq.levels() {
        for (_, node) in level.nodes() {
            if let Some(data) = node.try_classical()
                && definitions.insert(std::ptr::from_ref::<ClassicalNode>(data))
            {
                stored_terms += data.measurements().len() + data.operators().len();
            }
        }
    }
    let start = Instant::now();
    let bytes = compiled.bloq.to_binary();
    let encode_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    let restored = bloq_ir::Bloq::from_binary(&bytes).expect("decode");
    let decode_ms = start.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(restored.to_binary(), bytes);
    drop(restored);
    if let Some(path) = std::env::args().nth(2) {
        std::fs::write(path, compiled.bloq.to_text()).expect("write exact IR text");
    }
    eprintln!("codec {case}: {} bytes", bytes.len());
    let start = Instant::now();
    let (prepare_ms, vm_tasks) =
        match bloq_vm::lower(&compiled.bloq, &bloq_vm::LoweringConfig::default()) {
            Ok(vm) => (
                (start.elapsed().as_secs_f64() * 1000.0).to_string(),
                vm.tasks.len().to_string(),
            ),
            Err(error) => {
                eprintln!("VM preparation unsupported for this case: {error}");
                ("null".to_owned(), "null".to_owned())
            }
        };
    println!(
        "{{\"case\":\"{case}\",\"compile_ms\":{compile_ms},\"encode_ms\":{encode_ms},\"decode_ms\":{decode_ms},\"prepare_ms\":{prepare_ms},\"binary_bytes\":{},\"classical_definitions\":{},\"stored_readout_terms\":{stored_terms},\"vm_tasks\":{vm_tasks}}}",
        bytes.len(),
        definitions.len()
    );
}
