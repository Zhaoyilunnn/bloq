//! Generates `.pyi` stub files from the pyo3-stub-gen metadata collected by
//! the `gen_stub_*` macros. Run via `just py-stub`; output lands next to the
//! Python sources per `bloq_py/pyproject.toml`'s `module-name`.

use pyo3_stub_gen::Result;

fn main() -> Result<()> {
    let stub = bloq_py::stub_info()?;
    stub.generate()?;
    Ok(())
}
