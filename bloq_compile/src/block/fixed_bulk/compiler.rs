use bloq_graph::{BlockKind, Direction};

use crate::CompileError;
use crate::block::CompiledTemplate;
use crate::signature::BlockSignature;

use crate::block::walk::compile_walking;

use super::compile_cube;
use super::compile_measurement;
use super::port::compile_port;
use super::rotation::compile_patch_rotation;
use super::ybasis::compile_y;

pub(crate) fn validate_fixed_bulk(signature: BlockSignature) -> Result<(), CompileError> {
    // Every `BlockKind` is now compilable under the fixed-bulk convention
    // (T landed last, U17e); the per-kind checks carry the real constraints.
    // Exhaustive on purpose, mirroring `compile_fixed_bulk` below: a new kind must
    // state whether it needs a signature check rather than silently skipping one.
    match signature.kind {
        BlockKind::Y => validate_y_signature(signature),
        BlockKind::Measurement(_) => validate_measurement_signature(signature),
        // A temporal port and a selective block both resolve against their
        // single temporal neighbour's patch, so they take the leaf rules
        // unchanged.
        BlockKind::Port | BlockKind::Selective(_) => validate_leaf_signature(signature),
        BlockKind::T => validate_t_signature(signature),
        BlockKind::PatchRotation(_) => validate_patch_rotation_signature(signature),
        // The signature carries no extra kind-dependent payload to check.
        BlockKind::Cube(_) | BlockKind::Walking(_) => Ok(()),
    }
}

pub(crate) fn compile_fixed_bulk(
    signature: BlockSignature,
    distance: u32,
) -> Result<CompiledTemplate, CompileError> {
    match signature.kind {
        BlockKind::Cube(kind) => compile_cube(
            kind,
            signature.connectivity,
            distance,
            signature
                .rounds
                .expect("cube signatures carry a resolved round count"),
            signature
                .layer_schedule
                .expect("cube signatures include a layer schedule"),
        ),
        BlockKind::Y => {
            let boundary_basis = signature
                .boundary_basis
                .expect("fixed-bulk validation requires a Y boundary basis");
            compile_y(boundary_basis, signature.connectivity, distance)
        }
        BlockKind::Measurement(basis) => {
            let boundary_basis = signature
                .boundary_basis
                .expect("fixed-bulk validation requires a measurement boundary basis");
            compile_measurement(basis, signature.connectivity, distance, boundary_basis)
        }
        BlockKind::Walking(kind) => compile_walking(kind, signature.connectivity, distance),
        BlockKind::PatchRotation(kind) => {
            compile_patch_rotation(kind, signature.connectivity, distance)
        }
        BlockKind::Port => {
            let boundary_basis = signature
                .boundary_basis
                .expect("fixed-bulk validation requires a port boundary basis");
            compile_port(boundary_basis, signature.connectivity, distance)
        }
        // `validate_fixed_bulk` admits `Selective`, but it is compiled to its two
        // per-basis arms by `compile_selective` (dispatched in `compile_signature`),
        // never through this single-template method.
        BlockKind::Selective(_) => unreachable!(
            "selective blocks are compiled via compile_selective, not compile_fixed_bulk"
        ),
        // `validate_fixed_bulk` admits `T`, but it is compiled to its cultivation +
        // escape template pair in `compile_signature` (the Selective
        // dispatch pattern), never through this single-template method.
        BlockKind::T => unreachable!(
            "T blocks are compiled via compile_signature's T arm, not compile_fixed_bulk"
        ),
    }
}

/// True when the connectivity has exactly one temporal pipe (`Z+` xor `Z-`) and
/// no spatial pipes — the shape both Y and temporal-port blocks require.
fn single_temporal_no_spatial(conn: crate::signature::Connectivity) -> bool {
    let has_one_temporal_pipe = conn.has_pipe(Direction::ZPLUS) ^ conn.has_pipe(Direction::ZMINUS);
    let has_spatial_pipe = conn.spatial_pipes().next().is_some();
    has_one_temporal_pipe && !has_spatial_pipe
}

/// The shared leaf-block signature check — exactly one temporal pipe, no spatial
/// pipe, and a known patch boundary basis. Connectivity is checked before the
/// boundary basis; direction-specific callers add their own past/future check.
fn validate_leaf_signature(signature: BlockSignature) -> Result<(), CompileError> {
    if !single_temporal_no_spatial(signature.connectivity) {
        return Err(CompileError::InvalidConnectivity {
            kind: signature.kind,
            connectivity: signature.connectivity,
        });
    }
    if signature.boundary_basis.is_none() {
        return Err(CompileError::MissingBoundaryBasis {
            kind: signature.kind,
        });
    }
    Ok(())
}

fn validate_y_signature(signature: BlockSignature) -> Result<(), CompileError> {
    if signature.boundary_basis.is_none() {
        return Err(CompileError::MissingBoundaryBasis {
            kind: signature.kind,
        });
    }

    if single_temporal_no_spatial(signature.connectivity) {
        Ok(())
    } else {
        Err(CompileError::InvalidConnectivity {
            kind: signature.kind,
            connectivity: signature.connectivity,
        })
    }
}

fn validate_measurement_signature(signature: BlockSignature) -> Result<(), CompileError> {
    if !signature.connectivity.has_pipe(Direction::ZMINUS) {
        return Err(CompileError::InvalidConnectivity {
            kind: signature.kind,
            connectivity: signature.connectivity,
        });
    }
    validate_leaf_signature(signature)
}

/// A T block escapes onto a patch whose orientation is inherited from its single
/// temporal neighbour, so — like Y and Port — it needs exactly one
/// temporal pipe, no spatial pipe, and a known patch boundary basis. The pipe
/// must be `+Z`: a T block is future-directed (the graph validator also rejects
/// a past pipe, but this signature check keeps the compiler self-contained).
/// The spill side (`surgery_side`) is picked by `t_surgery_sides` before this
/// runs, so a missing side never reaches here.
fn validate_t_signature(signature: BlockSignature) -> Result<(), CompileError> {
    // T is future-directed: the temporal pipe must be `+Z`. This extra
    // constraint is checked before the shared leaf checks.
    if !signature.connectivity.has_pipe(Direction::ZPLUS) {
        return Err(CompileError::InvalidConnectivity {
            kind: signature.kind,
            connectivity: signature.connectivity,
        });
    }
    validate_leaf_signature(signature)
}

/// A patch rotation connects through temporal pipes only. Temporal Hadamard
/// flags need no check here: `template_connectivity` strips them before any
/// signature is built (the realignment pipe node owns the X↔Z flip), and
/// gateway keys are Hadamard-free either way.
fn validate_patch_rotation_signature(signature: BlockSignature) -> Result<(), CompileError> {
    let connectivity = signature.connectivity;
    let unsupported = connectivity
        .pipe_dirs()
        .any(|dir| !matches!(dir, Direction::ZMINUS | Direction::ZPLUS));
    if unsupported {
        Err(CompileError::InvalidConnectivity {
            kind: signature.kind,
            connectivity,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bloq_graph::Basis;

    use super::*;
    use crate::signature::Connectivity;

    fn t_signature(connectivity: Connectivity, boundary_basis: Option<Basis>) -> BlockSignature {
        BlockSignature {
            kind: BlockKind::T,
            rounds: Some(3),
            connectivity,
            boundary_basis,
            layer_schedule: None,
            surgery_side: Some(Direction::YMINUS),
        }
    }

    #[test]
    fn t_signature_requires_single_future_pipe_and_boundary_basis() {
        let zplus = Connectivity::ISOLATED.with_pipe(Direction::ZPLUS);
        validate_fixed_bulk(t_signature(zplus, Some(Basis::X))).unwrap();

        // A past pipe, an extra spatial pipe, or a missing basis are each rejected.
        let zminus = Connectivity::ISOLATED.with_pipe(Direction::ZMINUS);
        assert!(matches!(
            validate_fixed_bulk(t_signature(zminus, Some(Basis::X))),
            Err(CompileError::InvalidConnectivity {
                kind: BlockKind::T,
                connectivity,
            }) if connectivity == zminus
        ));
        assert!(matches!(
            validate_fixed_bulk(t_signature(
                zplus.with_pipe(Direction::XPLUS),
                Some(Basis::X),
            )),
            Err(CompileError::InvalidConnectivity {
                kind: BlockKind::T,
                ..
            })
        ));
        assert!(matches!(
            validate_fixed_bulk(t_signature(zplus, None)),
            Err(CompileError::MissingBoundaryBasis { kind: BlockKind::T })
        ));
    }

    #[test]
    fn fixed_measurement_requires_one_past_pipe_and_compiles_as_fixed() {
        let signature = |connectivity, boundary_basis| BlockSignature {
            kind: BlockKind::Measurement(Basis::X),
            rounds: None,
            connectivity,
            boundary_basis,
            layer_schedule: None,
            surgery_side: None,
        };
        let zminus = Connectivity::ISOLATED.with_pipe(Direction::ZMINUS);
        let valid = signature(zminus, Some(Basis::Z));

        validate_fixed_bulk(valid).expect("past-facing fixed measurement validates");
        compile_fixed_bulk(valid, 3).expect("fixed measurement compiles through the ordinary path");

        let zplus = Connectivity::ISOLATED.with_pipe(Direction::ZPLUS);
        assert!(matches!(
            validate_fixed_bulk(signature(zplus, Some(Basis::Z))),
            Err(CompileError::InvalidConnectivity { .. })
        ));
        assert!(matches!(
            validate_fixed_bulk(signature(zminus, None)),
            Err(CompileError::MissingBoundaryBasis {
                kind: BlockKind::Measurement(Basis::X),
            })
        ));
    }
}
