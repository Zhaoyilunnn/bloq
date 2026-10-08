use std::sync::{Arc, OnceLock};

use bloq_circuit::{DetectorCoords, DetectorParity, DetectorTerm};
use glam::IVec2;

use super::{
    Bloq, DetectorBundleId, InstanceMeasurement, NodeDetector, NodeDetectorParity, QuantumNode,
    TemplateId, TemplateInstanceId,
};

/// A measurement in one bundle-local owner slot.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct BundleMeasurement {
    /// Index into [`DetectorBundle::owner_templates`].
    pub owner: u32,
    /// Measurement id local to that owner's template.
    pub measurement: u32,
}

/// One row stored in a reusable detector bundle.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BundleDetector {
    /// Parity over bundle-local owner slots.
    pub parity: DetectorParity<BundleMeasurement>,
    /// Local decoder coordinates.
    pub coords: Option<DetectorCoords>,
}

/// Shared detector rows with an ordered template signature.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DetectorBundle {
    owner_templates: Vec<TemplateId>,
    detectors: Vec<BundleDetector>,
    #[serde(skip)]
    used_owners: OnceLock<Result<Vec<u32>, DetectorBundleError>>,
}

impl DetectorBundle {
    /// Build a bundle. Its local owner slots are checked when read or used.
    pub fn new(owner_templates: Vec<TemplateId>, detectors: Vec<BundleDetector>) -> Self {
        Self {
            owner_templates,
            detectors,
            used_owners: OnceLock::new(),
        }
    }

    /// Ordered template signature for the bundle's owner slots.
    pub fn owner_templates(&self) -> &[TemplateId] {
        &self.owner_templates
    }

    /// Detector rows in bundle-local order.
    pub fn detectors(&self) -> &[BundleDetector] {
        &self.detectors
    }

    /// Distinct owner slots referenced by measurement terms, in slot order.
    ///
    /// # Errors
    ///
    /// Returns [`DetectorBundleError::InvalidLocalOwner`] for an out-of-range slot,
    /// or [`DetectorBundleError::CountOverflow`] when owner slots exceed `u32`.
    pub fn used_owners(&self) -> Result<&[u32], DetectorBundleError> {
        self.used_owners
            .get_or_init(|| {
                if self.owner_templates.len() > u32::MAX as usize {
                    return Err(DetectorBundleError::CountOverflow);
                }
                let mut used = vec![false; self.owner_templates.len()];
                for detector in &self.detectors {
                    for term in detector.parity.terms() {
                        if let DetectorTerm::Measurement(measurement) = term {
                            if measurement.owner as usize >= self.owner_templates.len() {
                                return Err(DetectorBundleError::InvalidLocalOwner {
                                    owner: measurement.owner,
                                    owners: self.owner_templates.len(),
                                });
                            }
                            used[measurement.owner as usize] = true;
                        }
                    }
                }
                Ok(used
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, present)| present.then_some(index as u32))
                    .collect())
            })
            .as_ref()
            .map(Vec::as_slice)
            .map_err(Clone::clone)
    }
}

/// One node-local binding of a reusable detector bundle.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DetectorBundleUse {
    /// Shared bundle id.
    pub bundle: DetectorBundleId,
    /// Instance bound to each ordered owner slot.
    pub instances: Vec<TemplateInstanceId>,
    /// Translation of the first two detector coordinates.
    pub offset: IVec2,
}

/// Append-only, clone-cheap bundle storage.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DetectorBundlePool {
    #[serde(deserialize_with = "super::id::deserialize_pool")]
    bundles: Vec<Arc<DetectorBundle>>,
}

fn check_table_count(count: usize) -> Result<(), DetectorBundleError> {
    u32::try_from(count)
        .map(|_| ())
        .map_err(|_| DetectorBundleError::CountOverflow)
}

impl QuantumNode {
    pub(crate) fn check_side_table_counts(&self) -> Result<(), DetectorBundleError> {
        for count in [
            self.detectors.len(),
            self.detector_bundles.len(),
            self.restarts.len(),
        ] {
            check_table_count(count)?;
        }
        Ok(())
    }
}

impl DetectorBundlePool {
    /// Create an empty pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert an owned bundle and return its append-only id.
    ///
    /// # Panics
    ///
    /// Panics before insertion if the next id exceeds `u32`.
    pub fn insert(&mut self, bundle: DetectorBundle) -> DetectorBundleId {
        self.insert_shared(Arc::new(bundle))
    }

    /// Insert an immutable shared bundle, preserving its cached analysis.
    ///
    /// # Panics
    ///
    /// Panics before insertion if the next id exceeds `u32`.
    pub fn insert_shared(&mut self, bundle: Arc<DetectorBundle>) -> DetectorBundleId {
        let id = DetectorBundleId(super::id::pool_index(self.bundles.len()));
        self.bundles.push(bundle);
        id
    }

    /// Look up a bundle by id.
    pub fn get(&self, id: DetectorBundleId) -> Option<&DetectorBundle> {
        self.bundles.get(id.0 as usize).map(Arc::as_ref)
    }

    /// Iterate over bundles in id order.
    pub fn iter(&self) -> impl Iterator<Item = (DetectorBundleId, &DetectorBundle)> {
        self.iter_shared().map(|(id, bundle)| (id, bundle.as_ref()))
    }

    /// Borrow shared handles in id order.
    pub fn iter_shared(&self) -> impl Iterator<Item = (DetectorBundleId, &Arc<DetectorBundle>)> {
        self.bundles
            .iter()
            .enumerate()
            .map(|(index, bundle)| (DetectorBundleId(super::id::pool_index(index)), bundle))
    }

    /// Number of bundles.
    pub fn len(&self) -> usize {
        self.bundles.len()
    }

    /// Whether the pool has no bundles.
    pub fn is_empty(&self) -> bool {
        self.bundles.is_empty()
    }
}

/// Local structure errors found before any detector rows are expanded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DetectorBundleError {
    /// A node use refers to a bundle absent from the program pool.
    #[error("unknown detector bundle {0:?}")]
    UnknownBundle(DetectorBundleId),
    /// A node use binds a different number of instances than the bundle signature.
    #[error("detector bundle {bundle:?} binds {actual} instances; expected {expected}")]
    BindingArity {
        /// Referenced bundle.
        bundle: DetectorBundleId,
        /// Number of owner slots.
        expected: usize,
        /// Number of bound instances.
        actual: usize,
    },
    /// A bundle row references a local owner slot outside its signature.
    #[error("detector bundle owner slot {owner} exceeds {owners} owners")]
    InvalidLocalOwner {
        /// Referenced owner slot.
        owner: u32,
        /// Number of declared owner slots.
        owners: usize,
    },
    /// A bound instance id has no known template placement.
    #[error("detector bundle {bundle:?} owner {owner} binds unknown instance {instance:?}")]
    UnknownBoundInstance {
        /// Referenced bundle.
        bundle: DetectorBundleId,
        /// Bundle-local owner slot.
        owner: u32,
        /// Unknown instance id.
        instance: TemplateInstanceId,
    },
    /// A bound instance uses a different template than its owner slot declares.
    #[error(
        "detector bundle {bundle:?} owner {owner} expects {expected:?}, but {instance:?} uses {actual:?}"
    )]
    OwnerTemplateMismatch {
        /// Referenced bundle.
        bundle: DetectorBundleId,
        /// Bundle-local owner slot.
        owner: u32,
        /// Bound instance id.
        instance: TemplateInstanceId,
        /// Template declared by the bundle.
        expected: TemplateId,
        /// Actual instance template.
        actual: TemplateId,
    },
    /// A count or address exceeds its integer representation.
    #[error("detector count or address overflows")]
    CountOverflow,
}

/// Stable address of a node detector row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeDetectorAddress {
    /// Index into the node's inline detector table.
    Inline(u32),
    /// Index of a bundle use and a row within its definition.
    Bundle {
        /// Index into `QuantumNode::detector_bundles`.
        use_index: u32,
        /// Index into `DetectorBundle::detectors`.
        row: u32,
    },
}

/// A borrowed detector row, mapped into instance space on demand.
#[derive(Debug, Clone, Copy)]
pub struct NodeDetectorView<'a> {
    address: NodeDetectorAddress,
    source: NodeDetectorSource<'a>,
    offset: IVec2,
}

#[derive(Debug, Clone, Copy)]
enum NodeDetectorSource<'a> {
    Inline(&'a NodeDetector),
    Bundle(&'a BundleDetector, &'a [TemplateInstanceId]),
}

impl NodeDetectorView<'_> {
    /// Stable address of this row within the node.
    pub fn address(&self) -> NodeDetectorAddress {
        self.address
    }

    /// Terms mapped to bound instances, in source term order.
    pub fn terms(&self) -> impl Iterator<Item = DetectorTerm<InstanceMeasurement>> + '_ {
        let (inline, bundle) = match self.source {
            NodeDetectorSource::Inline(detector) => (Some(detector), None),
            NodeDetectorSource::Bundle(detector, instances) => (None, Some((detector, instances))),
        };
        inline
            .map(|detector| detector.parity.terms().iter().copied())
            .into_iter()
            .flatten()
            .chain(
                bundle
                    .map(|(detector, instances)| {
                        detector.parity.terms().iter().map(move |term| match *term {
                            DetectorTerm::Measurement(measurement) => {
                                DetectorTerm::Measurement(InstanceMeasurement {
                                    instance: instances[measurement.owner as usize],
                                    measurement: measurement.measurement,
                                })
                            }
                            DetectorTerm::LoopState(state) => DetectorTerm::LoopState(state),
                        })
                    })
                    .into_iter()
                    .flatten(),
            )
    }

    /// Measurement terms only, in source term order.
    pub fn measurements(&self) -> impl Iterator<Item = InstanceMeasurement> + '_ {
        self.terms().filter_map(|term| match term {
            DetectorTerm::Measurement(measurement) => Some(measurement),
            DetectorTerm::LoopState(_) => None,
        })
    }

    /// Constant XOR term.
    pub fn sign(&self) -> bool {
        match self.source {
            NodeDetectorSource::Inline(detector) => detector.parity.sign(),
            NodeDetectorSource::Bundle(detector, _) => detector.parity.sign(),
        }
    }

    /// Coordinates translated by the bundle use's spatial offset.
    pub fn coords(&self) -> Option<impl Iterator<Item = f64> + '_> {
        let coords = match self.source {
            NodeDetectorSource::Inline(detector) => detector.coords.as_ref(),
            NodeDetectorSource::Bundle(detector, _) => detector.coords.as_ref(),
        }?;
        Some(coords.iter().enumerate().map(move |(index, &coord)| {
            if matches!(self.source, NodeDetectorSource::Inline(_)) {
                return coord;
            }
            match index {
                0 => coord + self.offset.x as f64,
                1 => coord + self.offset.y as f64,
                _ => coord,
            }
        }))
    }

    /// Materialize and canonicalize the bound parity, including alias cancellation.
    pub fn parity(&self) -> NodeDetectorParity {
        NodeDetectorParity::from_terms(self.terms()).with_sign(self.sign())
    }

    /// Materialize the bound detector and its translated coordinates.
    pub fn to_owned(&self) -> NodeDetector {
        NodeDetector {
            parity: self.parity(),
            coords: self.coords().map(Iterator::collect),
        }
    }
}

/// Inline rows followed by bundle uses in node order, then row order.
#[derive(Debug)]
pub struct NodeDetectors<'a> {
    node: &'a QuantumNode,
    pool: &'a DetectorBundlePool,
    inline_index: usize,
    use_index: usize,
    row_index: usize,
}

impl<'a> Iterator for NodeDetectors<'a> {
    type Item = NodeDetectorView<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(detector) = self.node.detectors.get(self.inline_index) {
            let address = NodeDetectorAddress::Inline(self.inline_index as u32);
            self.inline_index += 1;
            return Some(NodeDetectorView {
                address,
                source: NodeDetectorSource::Inline(detector),
                offset: IVec2::ZERO,
            });
        }
        loop {
            let bundle_use = self.node.detector_bundles.get(self.use_index)?;
            let bundle = self
                .pool
                .get(bundle_use.bundle)
                .expect("preflight checked bundle");
            if let Some(detector) = bundle.detectors.get(self.row_index) {
                let address = NodeDetectorAddress::Bundle {
                    use_index: self.use_index as u32,
                    row: self.row_index as u32,
                };
                self.row_index += 1;
                return Some(NodeDetectorView {
                    address,
                    source: NodeDetectorSource::Bundle(detector, &bundle_use.instances),
                    offset: bundle_use.offset,
                });
            }
            self.use_index += 1;
            self.row_index = 0;
        }
    }
}

impl Bloq {
    /// Count inline and bound detector rows after checking local bundle structure.
    ///
    /// # Errors
    ///
    /// Returns a [`DetectorBundleError`] for a missing bundle, invalid binding,
    /// invalid local owner slot, or a count/address overflow.
    pub fn node_detector_count(&self, node: &QuantumNode) -> Result<usize, DetectorBundleError> {
        node.check_side_table_counts()?;
        let mut count = node.detectors.len();
        for bundle_use in &node.detector_bundles {
            let bundle = self
                .detector_bundles()
                .get(bundle_use.bundle)
                .ok_or(DetectorBundleError::UnknownBundle(bundle_use.bundle))?;
            let expected = bundle.owner_templates.len();
            let actual = bundle_use.instances.len();
            if actual != expected {
                return Err(DetectorBundleError::BindingArity {
                    bundle: bundle_use.bundle,
                    expected,
                    actual,
                });
            }
            bundle.used_owners()?;
            if bundle.detectors.len() > u32::MAX as usize {
                return Err(DetectorBundleError::CountOverflow);
            }
            count = count
                .checked_add(bundle.detectors.len())
                .ok_or(DetectorBundleError::CountOverflow)?;
        }
        if self.detector_bundles().len() > u32::MAX as usize {
            return Err(DetectorBundleError::CountOverflow);
        }
        Ok(count)
    }

    /// Check every bundle owner binding against a caller's instance registry.
    ///
    /// # Errors
    ///
    /// Returns local structure errors from [`Self::node_detector_count`], or
    /// [`DetectorBundleError::UnknownBoundInstance`] / [`DetectorBundleError::OwnerTemplateMismatch`]
    /// when the supplied registry does not match a bundle's owner signature.
    pub fn check_detector_bundle_bindings(
        &self,
        node: &QuantumNode,
        mut lookup: impl FnMut(TemplateInstanceId) -> Option<TemplateId>,
    ) -> Result<(), DetectorBundleError> {
        self.node_detector_count(node)?;
        for bundle_use in &node.detector_bundles {
            let bundle = self
                .detector_bundles()
                .get(bundle_use.bundle)
                .ok_or(DetectorBundleError::UnknownBundle(bundle_use.bundle))?;
            for (owner, (&instance, &expected)) in bundle_use
                .instances
                .iter()
                .zip(bundle.owner_templates.iter())
                .enumerate()
            {
                let actual = lookup(instance).ok_or(DetectorBundleError::UnknownBoundInstance {
                    bundle: bundle_use.bundle,
                    owner: owner as u32,
                    instance,
                })?;
                if actual != expected {
                    return Err(DetectorBundleError::OwnerTemplateMismatch {
                        bundle: bundle_use.bundle,
                        owner: owner as u32,
                        instance,
                        expected,
                        actual,
                    });
                }
            }
        }
        Ok(())
    }

    /// Borrow node detectors in inline, bundle-use, and bundle-row order.
    ///
    /// # Errors
    ///
    /// Returns the same structural errors as [`Self::node_detector_count`].
    pub fn node_detectors<'a>(
        &'a self,
        node: &'a QuantumNode,
    ) -> Result<NodeDetectors<'a>, DetectorBundleError> {
        self.node_detector_count(node)?;
        Ok(NodeDetectors {
            node,
            pool: self.detector_bundles(),
            inline_index: 0,
            use_index: 0,
            row_index: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use bloq_circuit::LoopStateId;

    use super::*;

    #[test]
    fn side_table_counts_fit_guard_index_counters() {
        assert_eq!(check_table_count(u32::MAX as usize), Ok(()));
        if let Some(exhausted) = (u32::MAX as usize).checked_add(1) {
            assert_eq!(
                check_table_count(exhausted),
                Err(DetectorBundleError::CountOverflow)
            );
        }
    }

    #[test]
    fn bound_rows_translate_and_cancel_aliased_measurements() {
        let mut bloq = Bloq::new();
        let bundle = bloq.add_detector_bundle(DetectorBundle::new(
            vec![TemplateId(0), TemplateId(0)],
            vec![BundleDetector {
                parity: DetectorParity::from_terms([
                    DetectorTerm::Measurement(BundleMeasurement {
                        owner: 0,
                        measurement: 7,
                    }),
                    DetectorTerm::Measurement(BundleMeasurement {
                        owner: 1,
                        measurement: 7,
                    }),
                    DetectorTerm::LoopState(LoopStateId(2)),
                ])
                .with_sign(true),
                coords: Some(DetectorCoords::from_slice(&[1.0, 2.0, 3.0])),
            }],
        ));
        let node = QuantumNode {
            detector_bundles: vec![DetectorBundleUse {
                bundle,
                instances: vec![TemplateInstanceId(4), TemplateInstanceId(4)],
                offset: IVec2::new(10, -2),
            }],
            ..QuantumNode::default()
        };
        assert_eq!(bloq.node_detector_count(&node), Ok(1));
        let row = bloq.node_detectors(&node).unwrap().next().unwrap();
        assert_eq!(
            row.address(),
            NodeDetectorAddress::Bundle {
                use_index: 0,
                row: 0
            }
        );
        assert_eq!(row.measurements().count(), 2);
        assert_eq!(
            row.parity().terms(),
            &[DetectorTerm::LoopState(LoopStateId(2))]
        );
        assert_eq!(row.coords().unwrap().collect::<Vec<_>>(), [11.0, 0.0, 3.0]);
    }

    #[test]
    fn preflight_rejects_bad_binding_and_local_slot() {
        let mut bloq = Bloq::new();
        let bundle = bloq.add_detector_bundle(DetectorBundle::new(
            vec![TemplateId(0)],
            vec![BundleDetector {
                parity: DetectorParity::from_measurements([BundleMeasurement {
                    owner: 1,
                    measurement: 0,
                }]),
                coords: None,
            }],
        ));
        let mut node = QuantumNode {
            detector_bundles: vec![DetectorBundleUse {
                bundle,
                instances: vec![],
                offset: IVec2::ZERO,
            }],
            ..QuantumNode::default()
        };
        assert_eq!(
            bloq.node_detector_count(&node),
            Err(DetectorBundleError::BindingArity {
                bundle,
                expected: 1,
                actual: 0
            })
        );
        node.detector_bundles[0]
            .instances
            .push(TemplateInstanceId(0));
        assert_eq!(
            bloq.node_detector_count(&node),
            Err(DetectorBundleError::InvalidLocalOwner {
                owner: 1,
                owners: 1
            })
        );
    }

    #[test]
    fn binding_checker_checks_even_unused_owner_slots() {
        let mut bloq = Bloq::new();
        let bundle = bloq.add_detector_bundle(DetectorBundle::new(vec![TemplateId(2)], vec![]));
        let instance = TemplateInstanceId(4);
        let node = QuantumNode {
            detector_bundles: vec![DetectorBundleUse {
                bundle,
                instances: vec![instance],
                offset: IVec2::ZERO,
            }],
            ..QuantumNode::default()
        };
        assert_eq!(
            bloq.check_detector_bundle_bindings(&node, |_| None),
            Err(DetectorBundleError::UnknownBoundInstance {
                bundle,
                owner: 0,
                instance,
            })
        );
        assert_eq!(
            bloq.check_detector_bundle_bindings(&node, |_| Some(TemplateId(1))),
            Err(DetectorBundleError::OwnerTemplateMismatch {
                bundle,
                owner: 0,
                instance,
                expected: TemplateId(2),
                actual: TemplateId(1),
            })
        );
        assert_eq!(
            bloq.check_detector_bundle_bindings(&node, |_| Some(TemplateId(2))),
            Ok(())
        );
    }
}
