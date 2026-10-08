//! Built-in block-graph gallery, as flat `gallery_*` functions over id
//! strings.
//!
//! These are plumbing for `bloq.GalleryItem`, the pure-Python enum that is the
//! public surface. They stay reachable as `bloq._core.gallery_*` for the enum
//! itself and its drift test, but are deliberately not re-exported from
//! `bloq`.

use std::collections::HashMap;
use std::str::FromStr;

use pyo3::IntoPyObjectExt;
use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::errors;
use crate::graph::PyBlockGraph;

fn lookup(id: &str) -> PyResult<bloq_graph::GalleryItem> {
    bloq_graph::GalleryItem::from_str(id).map_err(|_| {
        let known = bloq_graph::GalleryItem::iter()
            .map(bloq_graph::GalleryItem::id)
            .collect::<Vec<_>>()
            .join(", ");
        errors::InvalidArgumentError::new_err(format!(
            "unknown gallery id {id:?}; known ids: {known}"
        ))
    })
}

/// Builds the gallery graph with the given id. Public spelling:
/// `bloq.GalleryItem.BELL_STATE.load()`.
///
/// Args:
///     id: A gallery id.
///
/// Returns:
///     BlockGraph: A complete, valid graph for the entry.
///
/// Raises:
///     InvalidArgumentError: On an unknown id.
#[gen_stub_pyfunction(module = "bloq._core")]
#[pyfunction]
fn gallery_load(id: &str) -> PyResult<PyBlockGraph> {
    Ok(lookup(id)?.build().into())
}

/// The `.blog` source text of a gallery entry. Public spelling:
/// `bloq.GalleryItem.CNOT.source()`.
///
/// Args:
///     id: A gallery id.
///
/// Raises:
///     InvalidArgumentError: On an unknown id.
#[gen_stub_pyfunction(module = "bloq._core")]
#[pyfunction]
fn gallery_source(id: &str) -> PyResult<String> {
    Ok(lookup(id)?.entry().blog().to_owned())
}

/// All gallery entries as `{"id", "description", "categories"}` dicts — the
/// table `bloq.GalleryItem.description` / `.categories` read from.
///
/// Returns:
///     list[dict]: One dict per entry, each with `id` (str), `description`
///         (str), and `categories` (list[str]) keys.
#[gen_stub_pyfunction(module = "bloq._core")]
#[pyfunction]
fn gallery_entries(py: Python<'_>) -> PyResult<Vec<HashMap<String, Py<PyAny>>>> {
    bloq_graph::GalleryItem::iter()
        .map(|g| {
            let categories: Vec<String> = g.categories().iter().map(ToString::to_string).collect();
            Ok(HashMap::from([
                ("id".to_owned(), g.id().into_py_any(py)?),
                ("description".to_owned(), g.description().into_py_any(py)?),
                ("categories".to_owned(), categories.into_py_any(py)?),
            ]))
        })
        .collect()
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(gallery_load, m)?)?;
    m.add_function(wrap_pyfunction!(gallery_source, m)?)?;
    m.add_function(wrap_pyfunction!(gallery_entries, m)?)?;
    Ok(())
}
