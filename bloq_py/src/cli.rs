//! Console-script binding to the shared Rust CLI.

use std::ffi::OsString;

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

/// Runs the Rust CLI with argv including the executable name and returns its
/// exit status. Used by the package's console script.
#[gen_stub_pyfunction(module = "bloq._core")]
#[pyfunction]
fn _run_cli(py: Python<'_>, args: Vec<OsString>) -> u8 {
    py.detach(move || bloq_cli::run(args))
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(_run_cli, m)?)?;
    Ok(())
}
