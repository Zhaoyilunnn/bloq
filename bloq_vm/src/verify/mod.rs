//! Physical verification: run compiled bloq circuits on the
//! [`Simulator`](crate::backend::Simulator) engine and check them.
//!
//! The execution kernel itself — the whole-program walk, the flat
//! [`CircuitExecutor`](crate::CircuitExecutor), and the gate table — lives in
//! the crate root, and [`run_bloq`](crate::run_bloq) drives a shot batch through
//! it. This module is the *verifier's vocabulary*: the report types
//! ([`VerifyReport`] and friends), the detector-constancy check, and the
//! logical expectation readouts a per-shot hook scores a non-Clifford
//! output with.
//!
//! This is the physical counterpart to the graph-level logical verifier
//! (`bloq_graph::verify`). The layers are intentionally independent: this path
//! drives [`Simulator`](crate::backend::Simulator) over the fully compiled
//! physical circuit, while the graph path contracts symbolic QuiZX maps.

mod expectation;
mod report;

pub use expectation::{
    ccz_orbit_signatures, ccz_state_signature, logical_bloch, logical_signature, signatures_match,
};
pub(crate) use report::detector_report;
pub use report::{DetectorReport, FramePairReport, ObservableReport, VerifyReport};
