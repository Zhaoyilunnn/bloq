use std::fmt;

use bloq_circuit::DetectorParity;

pub(super) fn pool_index(index: usize) -> u32 {
    u32::try_from(index).expect("append-only pool id space exhausted")
}

pub(super) fn deserialize_pool<'de, D, T>(
    deserializer: D,
) -> Result<Vec<std::sync::Arc<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    let entries = <Vec<std::sync::Arc<T>> as serde::Deserialize>::deserialize(deserializer)?;
    if let Some(last) = entries.len().checked_sub(1) {
        u32::try_from(last)
            .map_err(|_| serde::de::Error::custom("append-only pool id space exhausted"))?;
    }
    Ok(entries)
}

/// A node's slot index in its own graph level (a [`crate::Bloq`] top level or
/// one region [`crate::SubGraph`] body) — ids are level-local, not program-global.
///
/// Identity contract: an id is valid until its node is
/// removed; removal may recycle the id for a later `add_node`. Holders of ids
/// across mutating passes must re-resolve them — in particular,
/// The `fuse_single_use_computes` optimization removes nodes and is the
/// documented last structural pass before external ids are retained.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct BloqNodeId(pub u32);

/// A template's index into the program's shared [`crate::BloqTemplatePool`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct TemplateId(pub u32);

/// A detector bundle's append-only index in a program.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct DetectorBundleId(pub u32);

/// A program-global id for one placement of a template: ids are unique
/// across every graph level, not level-local like [`BloqNodeId`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct TemplateInstanceId(pub u32);

/// One measurement within a template instance, named by the instance and the
/// template-local measurement id
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct InstanceMeasurement {
    /// Owning template instance.
    pub instance: TemplateInstanceId,
    /// Template-local measurement id.
    pub measurement: u32,
}

impl fmt::Display for InstanceMeasurement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "i{}:m{}", self.instance.0, self.measurement)
    }
}

/// A detector parity in template-local measurement-id space.
pub type TemplateDetectorParity = DetectorParity<u32>;
/// A measurement-only detector parity in instance space ([`InstanceMeasurement`]).
/// Validation rejects the generic parity type's loop-state terms (WF-6).
pub type NodeDetectorParity = DetectorParity<InstanceMeasurement>;

#[cfg(test)]
mod tests {
    #[test]
    fn pool_indices_never_alias_after_exhaustion() {
        assert_eq!(super::pool_index(u32::MAX as usize), u32::MAX);
        if let Some(exhausted) = (u32::MAX as usize).checked_add(1) {
            std::panic::catch_unwind(|| super::pool_index(exhausted))
                .expect_err("an exhausted append-only pool must refuse insertion");
        }
    }
}
