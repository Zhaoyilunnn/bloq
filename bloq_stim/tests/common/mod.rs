//! Shared noise model and distance oracle for Stim integration tests.

use stim::{Circuit, noise::UniformDepolarizing};

/// The physical error rate every distance check in this suite assumes.
pub(crate) fn uniform_depolarizing() -> UniformDepolarizing {
    UniformDepolarizing::new(1e-3).expect("1e-3 is a valid probability")
}

/// The same graphlike search as `Circuit::shortest_graphlike_error`, without
/// reconstructing physical error locations that distance checks never inspect.
pub(crate) fn graphlike_distance(circuit: &Circuit) -> stim::Result<usize> {
    // Match the circuit helper's DEM options, including disjoint-error handling.
    circuit
        .detector_error_model_with_options(false, false, false, 1.0, false, false)?
        .shortest_graphlike_error(true)
        .map(|error| error.len())
}
