use std::collections::BTreeMap;

use bloq_circuit::{Basis, PauliMap};
use glam::IVec3;

use super::{Bloq, BloqNodeId, NodeProvenance, TemplateInstanceId};

/// Logical Pauli-frame sign nodes for one terminal output.
///
/// These are the `Compute` nodes carrying the X- and Z-frame corrections a frame consumer
/// applies at the output port. Produced by the symbolic GF(2) solve of the
/// joint output-correction system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePair {
    /// The source output port this frame corrects.
    pub port: IVec3,
    /// The `Compute` node whose bit is the X-frame sign.
    pub x: BloqNodeId,
    /// The `Compute` node whose bit is the Z-frame sign.
    pub z: BloqNodeId,
}

/// Terminal logical Pauli operators for one program output.
///
/// The compiler records these in layout-global coordinates so an executor can
/// inspect or teleport an output using the saved IR alone. The owning instance
/// also identifies the output cut gated by shot-scoped discards.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogicalOutput {
    /// The source output port.
    pub port: IVec3,
    /// Instance owning this output cut; coordinates may be reused by later instances.
    /// Its membership and enclosing region selections must be unconditional (WF-7).
    pub instance: TemplateInstanceId,
    /// Logical X operator in layout-global coordinates.
    pub x: PauliMap,
    /// Logical Z operator in layout-global coordinates.
    pub z: PauliMap,
}

/// Input-port metadata for physical preparation hooks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogicalInput {
    /// The source input port.
    pub port: IVec3,
    /// Port instance whose template owns the patch stabilizers.
    /// Its membership and enclosing region selections must be unconditional (WF-7).
    pub instance: TemplateInstanceId,
    /// Logical X operator in layout-global coordinates.
    pub x: PauliMap,
    /// Logical Z operator in layout-global coordinates.
    pub z: PauliMap,
}

impl Bloq {
    /// Replace the logical-input metadata.
    pub fn set_logical_inputs(&mut self, inputs: Vec<LogicalInput>) {
        self.logical_inputs = inputs;
    }

    /// Logical input patches in deterministic source-port order.
    pub fn logical_inputs(&self) -> &[LogicalInput] {
        &self.logical_inputs
    }

    /// The per-output terminal Pauli-frame signs: one [`FramePair`] per
    /// source output port, in `(x, y, z)` order. Derived from the
    /// [`NodeProvenance::OutputFrame`] stamps on frame-sign `Compute` nodes —
    /// the stamps are the single source of truth, so this table cannot drift
    /// from the graph. Validation guarantees every stamped output carries
    /// exactly one X and one Z stamp; on an unvalidated program an incomplete
    /// pair is skipped.
    // Spec rule WF-15.
    pub fn output_frames(&self) -> Vec<FramePair> {
        let mut stamps = BTreeMap::new();
        for (id, node) in self.nodes() {
            if let NodeProvenance::OutputFrame { port, basis } = &node.provenance {
                let entry: &mut (Option<BloqNodeId>, Option<BloqNodeId>) =
                    stamps.entry((port.x, port.y, port.z)).or_default();
                match basis {
                    Basis::X => entry.0 = Some(id),
                    Basis::Z => entry.1 = Some(id),
                }
            }
        }
        stamps
            .into_iter()
            .filter_map(|((x, y, z), (frame_x, frame_z))| {
                Some(FramePair {
                    port: IVec3::new(x, y, z),
                    x: frame_x?,
                    z: frame_z?,
                })
            })
            .collect()
    }

    /// Replace the terminal logical-output metadata.
    pub fn set_logical_outputs(&mut self, outputs: Vec<LogicalOutput>) {
        self.logical_outputs = outputs;
    }

    /// Terminal logical operators in deterministic output order.
    pub fn logical_outputs(&self) -> &[LogicalOutput] {
        &self.logical_outputs
    }
}
