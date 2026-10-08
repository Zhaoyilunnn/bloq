//! Binary definitions are separate from their graph invocations. Content keys
//! give deterministic output even when a hand-built graph has not been interned.

use std::sync::Arc;

use serde::{Deserialize, Serialize, de::Error as _, ser::SerializeSeq, ser::SerializeStruct};

use crate::{
    BloqNode, BloqNodeKind, ClassicalNode, NodeProvenance, QuantumNode, RegionNode, SubGraph,
};

#[derive(Serialize)]
enum KindRef<'a> {
    Quantum(&'a Arc<QuantumNode>),
    Classical(u32),
    Region(&'a RegionNode),
}

#[derive(Serialize)]
struct NodeRef<'a> {
    kind: KindRef<'a>,
    provenance: &'a NodeProvenance,
    activation: Option<u32>,
}

#[derive(Deserialize)]
enum Kind {
    Quantum(#[serde(deserialize_with = "crate::ir::node::deserialize_quantum")] Arc<QuantumNode>),
    Classical(u32),
    Region(RegionNode),
}

#[derive(Deserialize)]
struct Node {
    kind: Kind,
    provenance: NodeProvenance,
    activation: Option<u32>,
}

struct Nodes<'a> {
    graph: &'a SubGraph,
    ids: crate::FxMap<*const ClassicalNode, u32>,
}

impl Serialize for Nodes<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.graph.node_count()))?;
        for (id, node) in self.graph.nodes() {
            let kind = match &node.kind {
                BloqNodeKind::Quantum(data) => KindRef::Quantum(data),
                BloqNodeKind::Classical(data) => KindRef::Classical(self.ids[&Arc::as_ptr(data)]),
                BloqNodeKind::Region(data) => KindRef::Region(data),
            };
            sequence.serialize_element(&(
                id.0,
                NodeRef {
                    kind,
                    provenance: &node.provenance,
                    activation: node.activation,
                },
            ))?;
        }
        sequence.end()
    }
}

struct Edges<'a>(&'a SubGraph);

impl Serialize for Edges<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        super::serialize_edges(self.0, serializer)
    }
}

pub(super) fn serialize<S: serde::Serializer>(
    graph: &SubGraph,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut definitions = Vec::new();
    let mut ids = crate::FxMap::default();
    let mut contents = crate::FxMap::default();
    for (_, node) in graph.nodes() {
        if let BloqNodeKind::Classical(data) = &node.kind
            && let std::collections::hash_map::Entry::Vacant(entry) = ids.entry(Arc::as_ptr(data))
        {
            let id = if let Some(&id) = contents.get(data.as_ref()) {
                id
            } else {
                let id = u32::try_from(definitions.len()).map_err(serde::ser::Error::custom)?;
                contents.insert(data.as_ref(), id);
                definitions.push(data.as_ref());
                id
            };
            entry.insert(id);
        }
    }
    let mut wire = serializer.serialize_struct("SubGraphWire", 5)?;
    wire.serialize_field("classical", &definitions)?;
    wire.serialize_field("nodes", &Nodes { graph, ids })?;
    wire.serialize_field("edges", &Edges(graph))?;
    wire.serialize_field("value_output", &graph.value_output())?;
    wire.serialize_field("boundary_outputs", &graph.boundary_outputs())?;
    wire.end()
}

pub(super) fn read_nodes<'de, A: serde::de::SeqAccess<'de>>(
    sequence: &mut A,
) -> Result<Vec<(u32, BloqNode)>, A::Error> {
    let definitions: Vec<Arc<ClassicalNode>> = sequence
        .next_element()?
        .ok_or_else(|| A::Error::missing_field("classical"))?;
    let nodes: Vec<(u32, Node)> = sequence
        .next_element()?
        .ok_or_else(|| A::Error::missing_field("nodes"))?;
    nodes
        .into_iter()
        .map(|(id, node)| {
            let kind = match node.kind {
                Kind::Quantum(data) => BloqNodeKind::Quantum(data),
                Kind::Region(data) => BloqNodeKind::Region(data),
                Kind::Classical(index) => BloqNodeKind::Classical(Arc::clone(
                    definitions.get(index as usize).ok_or_else(|| {
                        A::Error::custom(format!("classical definition {index} does not exist"))
                    })?,
                )),
            };
            Ok((
                id,
                BloqNode {
                    kind,
                    provenance: node.provenance,
                    activation: node.activation,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BloqEdge, BloqNodeId, ClassicalExpr, ValueRef};

    #[test]
    fn codecs_restore_shared_definitions_and_keep_invocations_independent() {
        let mut graph = SubGraph::new();
        let zero = graph.add_node(BloqNode::classical(ClassicalNode::Compute {
            expr: ClassicalExpr::Const(false),
        }));
        let one = graph.add_node(BloqNode::classical(ClassicalNode::Compute {
            expr: ClassicalExpr::Const(true),
        }));
        let definition = ClassicalNode::Compute {
            expr: ClassicalExpr::parity([3, 3, 1], true),
        };
        let mut first = BloqNode::classical(definition.clone())
            .with_provenance(NodeProvenance::Action { ordinal: 7 });
        first.activation = Some(9);
        let first = graph.add_node(first);
        let mut second = BloqNode::classical(definition.clone())
            .with_provenance(NodeProvenance::Generator { ordinal: 11 });
        second.activation = Some(10);
        let second = graph.add_node(second);
        for (source, target, slot) in [
            (zero, first, 3),
            (one, first, 1),
            (one, first, 9),
            (one, second, 3),
            (zero, second, 1),
            (zero, second, 10),
        ] {
            graph.add_edge(source, target, BloqEdge::value(slot));
        }
        graph.set_value_output(Some(second.into()));
        let shares_definition = |level: &SubGraph| match (&level[first].kind, &level[second].kind) {
            (BloqNodeKind::Classical(left), BloqNodeKind::Classical(right)) => {
                Arc::ptr_eq(left, right)
            }
            _ => unreachable!("both invocations are classical"),
        };
        assert!(!shares_definition(&graph));

        let bytes = postcard::to_extend(&graph, Vec::new()).unwrap();
        let json = serde_json::to_value(&graph).unwrap();
        assert!(json.get("classical").is_none());
        assert_eq!(
            json["nodes"][first.0 as usize][1]["kind"]["Classical"],
            serde_json::to_value(&definition).unwrap()
        );
        for mut restored in [
            postcard::from_bytes::<SubGraph>(&bytes).unwrap(),
            serde_json::from_value::<SubGraph>(json.clone()).unwrap(),
        ] {
            assert!(shares_definition(&restored));
            assert_eq!(postcard::to_extend(&restored, Vec::new()).unwrap(), bytes);
            assert_eq!(serde_json::to_value(&restored).unwrap(), json);
            *restored
                .node_mut(first)
                .unwrap()
                .try_classical_mut()
                .unwrap() = ClassicalNode::Compute {
                expr: ClassicalExpr::Const(false),
            };
            assert!(!shares_definition(&restored));
            assert_eq!(restored[second].try_classical(), Some(&definition));
            assert_ne!(restored[first].try_classical(), Some(&definition));
            assert_eq!(graph[first].try_classical(), Some(&definition));
        }
    }

    #[test]
    fn binary_graph_rejects_out_of_range_classical_definition() {
        let definitions = vec![ClassicalNode::Compute {
            expr: ClassicalExpr::Const(false),
        }];
        for index in [0, 1, u32::MAX] {
            let nodes = vec![(
                0u32,
                NodeRef {
                    kind: KindRef::Classical(index),
                    provenance: &NodeProvenance::None,
                    activation: None,
                },
            )];
            let wire = (
                &definitions,
                &nodes,
                Vec::<()>::new(),
                None::<ValueRef>,
                Vec::<BloqNodeId>::new(),
            );
            let bytes = postcard::to_extend(&wire, Vec::new()).unwrap();
            let restored = postcard::from_bytes::<SubGraph>(&bytes);
            if index == 0 {
                assert_eq!(
                    restored.unwrap()[BloqNodeId(0)].try_classical(),
                    Some(&definitions[0])
                );
            } else {
                restored.unwrap_err();
            }
        }
    }
}
