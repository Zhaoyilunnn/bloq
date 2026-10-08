//! Internal geometry helpers for fixed-bulk surface code patch construction.
//!
//! A [`Tile`] represents one stabilizer: a measure qubit that
//! interacts with a sequence of data qubits. The data qubit slots are ordered
//! by interaction layer — the order in which CX/CZ gates are applied. Some
//! slots may be `None`, indicating that no data qubit is interacted with
//! during that layer (e.g. boundary-truncated corners in a surface code, or
//! skipped interaction steps in other codes).
//!
//! This type carries no circuit or flow information — it is purely geometric.
//! Use it for patch construction, visualization, and coordinate transforms.
//! While surface codes typically have up to 4 data qubits per stabilizer,
//! `Tile` supports arbitrary stabilizer weight (e.g. weight-6 for color codes).

use bloq_circuit::PauliMap;
use bloq_graph::{Basis, Pauli};
use glam::IVec2;
use smallvec::SmallVec;

type DataSlots = SmallVec<[Option<IVec2>; 5]>;

/// A geometry-only stabilizer tile within a code patch.
///
/// `data_slots` stores the data qubits in interaction order — the sequence in
/// which they are interacted with via entangling gates. Entries may be `None`
/// to indicate that no data qubit is interacted with during that layer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Tile {
    basis: Basis,
    measure_qubit: IVec2,
    data_slots: DataSlots,
}

impl Tile {
    /// Create a new tile with the given basis, measure qubit, and data qubit slots.
    ///
    /// `data_slots` lists data qubits in interaction order. `None` entries
    /// indicate layers where no data qubit is interacted with.
    pub(crate) fn new(
        basis: Basis,
        measure_qubit: IVec2,
        data_slots: impl IntoIterator<Item = Option<IVec2>>,
    ) -> Self {
        Self {
            basis,
            measure_qubit,
            data_slots: data_slots.into_iter().collect(),
        }
    }

    /// Stabilizer basis (X or Z).
    pub(crate) fn basis(&self) -> Basis {
        self.basis
    }

    /// Measure qubit coordinate.
    pub(crate) fn measure_qubit(&self) -> IVec2 {
        self.measure_qubit
    }

    /// Data qubit slots in interaction order. `None` entries indicate layers
    /// where no data qubit is interacted with.
    pub(crate) fn data_slots(&self) -> &[Option<IVec2>] {
        &self.data_slots
    }

    /// Active (non-None) data qubit coordinates.
    ///
    /// Uses inline O(n^2) dedup rather than a HashMap because stabilizer
    /// tiles typically have only 4-8 data slots.
    pub(crate) fn active_data_qubits(&self) -> Vec<IVec2> {
        let mut result = Vec::with_capacity(self.data_slots.len());
        for &slot in &self.data_slots {
            if let Some(coord) = slot
                && !result.contains(&coord)
            {
                result.push(coord);
            }
        }
        result
    }

    /// Build a `PauliMap` representing this stabilizer's Pauli product on data qubits.
    pub(crate) fn pauli_map(&self) -> PauliMap {
        let pauli = Pauli::from(self.basis);
        PauliMap::from_unique_entries(self.active_data_qubits().into_iter().map(|dq| (dq, pauli)))
    }
}

impl PartialOrd for Tile {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Tile {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.measure_qubit.y, self.measure_qubit.x, self.basis as u8)
            .cmp(&(
                other.measure_qubit.y,
                other.measure_qubit.x,
                other.basis as u8,
            ))
            .then_with(|| {
                self.data_slots
                    .iter()
                    .map(|slot| slot.map(|v| v.to_array()))
                    .cmp(
                        other
                            .data_slots
                            .iter()
                            .map(|slot| slot.map(|v| v.to_array())),
                    )
            })
    }
}

/// A surface code patch: a sorted collection of stabilizer tiles.
///
/// Tiles are sorted on construction by `(measure_qubit.y, measure_qubit.x, basis)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Patch {
    tiles: Vec<Tile>,
}

impl Patch {
    /// Create a new patch from an unsorted collection of tiles.
    #[cfg(test)]
    pub(super) fn new(mut tiles: Vec<Tile>) -> Self {
        tiles.sort();
        Self { tiles }
    }

    /// Create a patch from tiles that are already sorted by [`Tile::cmp`].
    pub(super) fn from_sorted_tiles(tiles: Vec<Tile>) -> Self {
        debug_assert!(
            tiles.windows(2).all(|pair| pair[0] <= pair[1]),
            "Patch::from_sorted_tiles requires sorted tiles"
        );
        Self { tiles }
    }

    /// The sorted tiles in this patch.
    pub(super) fn tiles(&self) -> &[Tile] {
        &self.tiles
    }

    /// Number of tiles.
    pub(super) fn len(&self) -> usize {
        self.tiles.len()
    }

    /// Set of all data qubit coordinates.
    pub(super) fn data_set(&self) -> crate::FxSet<IVec2> {
        let mut set = crate::FxSet::default();
        for tile in &self.tiles {
            for &slot in tile.data_slots() {
                if let Some(coord) = slot {
                    set.insert(coord);
                }
            }
        }
        set
    }

    /// Set of all qubit coordinates (data + measure).
    ///
    /// Iterates tile fields directly into one crate::FxSet instead of creating
    /// intermediate per-tile HashSets, avoiding hundreds of temporary
    /// allocations for large patches.
    pub(super) fn used_set(&self) -> crate::FxSet<IVec2> {
        let mut set = crate::FxSet::default();
        for tile in &self.tiles {
            set.insert(tile.measure_qubit());
            for &slot in tile.data_slots() {
                if let Some(coord) = slot {
                    set.insert(coord);
                }
            }
        }
        set
    }

    /// X-basis tiles.
    #[cfg(test)]
    pub(super) fn x_tiles(&self) -> impl Iterator<Item = &Tile> {
        self.tiles.iter().filter(|t| t.basis() == Basis::X)
    }

    /// Z-basis tiles.
    pub(super) fn z_tiles(&self) -> impl Iterator<Item = &Tile> {
        self.tiles.iter().filter(|t| t.basis() == Basis::Z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tile_accessors_and_active_data_are_deduplicated() {
        let tile = Tile::new(
            Basis::Z,
            IVec2::new(2, 2),
            vec![
                Some(IVec2::new(1, 3)),
                Some(IVec2::new(3, 3)),
                None,
                Some(IVec2::new(1, 3)),
                Some(IVec2::new(3, 1)),
            ],
        );
        assert_eq!(tile.basis(), Basis::Z);
        assert_eq!(tile.measure_qubit(), IVec2::new(2, 2));
        assert_eq!(tile.active_data_qubits().len(), 3);
    }

    #[test]
    fn test_tile_pauli_map() {
        let tile = Tile::new(
            Basis::X,
            IVec2::new(0, 0),
            vec![Some(IVec2::new(1, 1)), Some(IVec2::new(-1, -1))],
        );
        let pm = tile.pauli_map();
        assert_eq!(pm.len(), 2);
        assert_eq!(pm.get(&IVec2::new(1, 1)), Some(&Pauli::X));
        assert_eq!(pm.get(&IVec2::new(-1, -1)), Some(&Pauli::X));
    }

    #[test]
    fn test_tile_ordering() {
        let t1 = Tile::new(Basis::X, IVec2::new(0, 0), vec![]);
        let t2 = Tile::new(Basis::Z, IVec2::new(0, 0), vec![]);
        let t3 = Tile::new(Basis::X, IVec2::new(2, 0), vec![]);
        let t4 = Tile::new(Basis::X, IVec2::new(0, 2), vec![]);
        // Sort order: (y, x, basis_as_u8)
        let mut tiles = vec![t4.clone(), t3.clone(), t2.clone(), t1.clone()];
        tiles.sort();
        assert_eq!(tiles, vec![t1, t2, t3, t4]);
    }

    #[test]
    fn test_single_tile_patch() {
        let tile = Tile::new(
            Basis::Z,
            IVec2::new(2, 2),
            vec![
                Some(IVec2::new(1, 3)),
                Some(IVec2::new(3, 3)),
                Some(IVec2::new(1, 1)),
                Some(IVec2::new(3, 1)),
            ],
        );
        let patch = Patch::new(vec![tile]);
        assert_eq!(patch.len(), 1);
        assert_eq!(patch.data_set().len(), 4);
        assert_eq!(patch.used_set().len(), 5);
    }

    #[test]
    fn test_patch_sorted_on_construction() {
        let t1 = Tile::new(Basis::X, IVec2::new(4, 2), vec![]);
        let t2 = Tile::new(Basis::Z, IVec2::new(2, 2), vec![]);
        let patch = Patch::new(vec![t1.clone(), t2.clone()]);
        assert_eq!(patch.tiles()[0], t2); // y=2,x=2 before y=2,x=4
        assert_eq!(patch.tiles()[1], t1);
    }

    #[test]
    fn test_x_z_tile_filters() {
        let tx = Tile::new(Basis::X, IVec2::new(2, 2), vec![]);
        let tz = Tile::new(Basis::Z, IVec2::new(4, 2), vec![]);
        let patch = Patch::new(vec![tx, tz]);
        assert_eq!(patch.x_tiles().count(), 1);
        assert_eq!(patch.z_tiles().count(), 1);
    }
}
