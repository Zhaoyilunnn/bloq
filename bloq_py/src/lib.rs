//! Python bindings for the bloq workspace, exposed as the extension module
//! `bloq._core` (distribution: `bloq-py`, import name: `bloq`).

use pyo3::prelude::*;

mod actions;
mod circuit;
mod cli;
mod compile;
mod errors;
mod gallery;
mod graph;
mod ir;
mod plan;
mod primitives;
mod vm;
mod zx;

/// Domain failures derive from `BloqError`. Python indexing, mapping,
/// coercion, and I/O errors retain their builtins; see [`errors`].
#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    errors::register(m)?;
    cli::register(m)?;
    primitives::register(m)?;
    graph::register(m)?;
    actions::register(m)?;
    zx::register(m)?;
    circuit::register(m)?;
    ir::register(m)?;
    compile::register(m)?;
    plan::register(m)?;
    gallery::register(m)?;
    vm::register(m)?;

    m.add("__version__", env!("CARGO_PKG_VERSION"))?;

    Ok(())
}

pyo3_stub_gen::define_stub_info_gatherer!(stub_info);

pyo3_stub_gen::module_variable!("bloq._core", "__version__", String);
