//! Read-only views of one ZX time layer.

use super::ZXGraph;

/// A precomputed snapshot of one time (`z`) layer of a [`ZXGraph`]: its nodes,
/// in-layer edges, and the nodes with edges into the past or future layers.
#[derive(Debug, Clone)]
pub struct ZXLayerView {
    z: i32,
    node_ids: Vec<usize>,
    spacelike_edges: Vec<(usize, usize)>,
    past_timelike_nodes: Vec<usize>,
    future_timelike_nodes: Vec<usize>,
}

impl ZXLayerView {
    pub(crate) fn new(zx: &ZXGraph, z: i32) -> Self {
        let mut node_ids = zx
            .nodes
            .iter()
            .filter(|node| node.pos.z == z)
            .map(|node| node.id)
            .collect::<Vec<_>>();
        node_ids.sort_unstable();

        let mut spacelike_edges = zx
            .edges
            .iter()
            .filter(|edge| {
                edge.n1 < edge.n2 && zx.nodes[edge.n1].pos.z == z && zx.nodes[edge.n2].pos.z == z
            })
            .map(|edge| (edge.n1, edge.n2))
            .collect::<Vec<_>>();
        spacelike_edges.sort_unstable();

        let mut past_timelike_nodes = Vec::new();
        let mut future_timelike_nodes = Vec::new();
        for &node_id in &node_ids {
            let neighbors = zx.neighbor_ids(node_id);
            if neighbors
                .iter()
                .any(|&neighbor| zx.nodes[neighbor].pos.z < z)
            {
                past_timelike_nodes.push(node_id);
            }
            if neighbors
                .iter()
                .any(|&neighbor| zx.nodes[neighbor].pos.z > z)
            {
                future_timelike_nodes.push(node_id);
            }
        }

        Self {
            z,
            node_ids,
            spacelike_edges,
            past_timelike_nodes,
            future_timelike_nodes,
        }
    }

    /// Returns the time layer this view covers.
    pub fn z(&self) -> i32 {
        self.z
    }

    /// Returns the ids of nodes in this layer.
    pub fn node_ids(&self) -> &[usize] {
        &self.node_ids
    }

    /// Returns the in-layer edges as sorted `(n1, n2)` pairs.
    pub fn spacelike_edges(&self) -> &[(usize, usize)] {
        &self.spacelike_edges
    }

    /// Returns the layer nodes that connect to an earlier layer.
    pub fn past_timelike_nodes(&self) -> &[usize] {
        &self.past_timelike_nodes
    }

    /// Returns the layer nodes that connect to a later layer.
    pub fn future_timelike_nodes(&self) -> &[usize] {
        &self.future_timelike_nodes
    }
}

#[cfg(test)]
mod tests {
    use glam::ivec3;
    use rustc_hash::FxHashMap;

    use crate::zx::graph::{CsrAdjacency, ZXEdge, ZXNode};
    use crate::zx::{NodeKind, ZXGraph};

    #[test]
    fn zx_layer_view_exposes_exact_membership_and_edges() {
        let zx = hand_built_zx_graph();
        let layer = zx.layer(0);

        let past = node_id(&zx, ivec3(0, 0, 0));
        let left = node_id(&zx, ivec3(1, 0, 0));
        let right = node_id(&zx, ivec3(2, 0, 0));
        let isolated = node_id(&zx, ivec3(5, 5, 0));
        let below = node_id(&zx, ivec3(0, 0, -1));

        assert_eq!(zx.z_layers(), vec![-1, 0, 1]);
        assert_eq!(layer.z(), 0);
        assert_eq!(layer.node_ids(), &[past, left, right, isolated]);
        assert_eq!(layer.spacelike_edges(), &[(left, right)]);
        assert_eq!(layer.past_timelike_nodes(), &[past]);
        assert_eq!(layer.future_timelike_nodes(), &[right]);

        assert_eq!(
            zx.edge_between(left, right)
                .map(|edge| (edge.n1, edge.n2, edge.hadamard)),
            Some((left, right, false))
        );
        assert_eq!(
            zx.edge_between(right, left)
                .map(|edge| (edge.n1, edge.n2, edge.hadamard)),
            Some((right, left, false))
        );
        assert_eq!(
            zx.edge_between(past, below)
                .map(|edge| (edge.n1, edge.n2, edge.hadamard)),
            Some((past, below, false))
        );
        assert_eq!(
            zx.edge_between(below, past)
                .map(|edge| (edge.n1, edge.n2, edge.hadamard)),
            Some((below, past, false))
        );
    }

    #[test]
    fn zx_output_ports_ignore_isolated_ports() {
        let zx = hand_built_zx_graph();

        assert_eq!(zx.output_ports(), vec![glam::ivec3(0, 0, 0)]);
    }

    fn hand_built_zx_graph() -> crate::ZXGraph {
        ZXGraph {
            nodes: vec![
                ZXNode::new(0, ivec3(0, 0, -1), NodeKind::X),
                ZXNode::new(1, ivec3(0, 0, 0), NodeKind::Port),
                ZXNode::new(2, ivec3(1, 0, 0), NodeKind::X),
                ZXNode::new(3, ivec3(2, 0, 0), NodeKind::Z),
                ZXNode::new(4, ivec3(2, 0, 1), NodeKind::X),
                ZXNode::new(5, ivec3(5, 5, 0), NodeKind::Port),
            ],
            edges: vec![
                ZXEdge {
                    n1: 0,
                    n2: 1,
                    id: 6,
                    hadamard: false,
                },
                ZXEdge {
                    n1: 1,
                    n2: 0,
                    id: 7,
                    hadamard: false,
                },
                ZXEdge {
                    n1: 2,
                    n2: 3,
                    id: 8,
                    hadamard: false,
                },
                ZXEdge {
                    n1: 3,
                    n2: 2,
                    id: 9,
                    hadamard: false,
                },
                ZXEdge {
                    n1: 3,
                    n2: 4,
                    id: 10,
                    hadamard: false,
                },
                ZXEdge {
                    n1: 4,
                    n2: 3,
                    id: 11,
                    hadamard: false,
                },
            ],
            adjacency: CsrAdjacency::from_edges(
                6,
                &[
                    (0, 1, 6),
                    (1, 0, 7),
                    (2, 3, 8),
                    (3, 2, 9),
                    (3, 4, 10),
                    (4, 3, 11),
                ],
            ),
            pos_to_node: FxHashMap::from_iter([
                (ivec3(0, 0, -1), 0),
                (ivec3(0, 0, 0), 1),
                (ivec3(1, 0, 0), 2),
                (ivec3(2, 0, 0), 3),
                (ivec3(2, 0, 1), 4),
                (ivec3(5, 5, 0), 5),
            ]),
            action_graph: crate::ActionDag::default(),
            total_ids: 12,
            cross_incident: Default::default(),
            stabilizer_phase_basis: Default::default(),
        }
    }

    fn node_id(zx: &ZXGraph, pos: glam::IVec3) -> usize {
        zx.nodes
            .iter()
            .find(|node| node.pos == pos)
            .map(|node| node.id)
            .expect("node exists at position")
    }
}
