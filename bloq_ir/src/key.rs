//! Compile-stable node identity.
//!
//! A [`BloqNodeId`] identifies a node inside one program, and nothing more:
//! two programs compiled from the same block graph — the ordinary compile and
//! a Clifford proxy of it, say — number their nodes independently, and an edit
//! may recycle a vacant slot (SEM-ID). A consumer that has to line the two up
//! needs an identity derived from what a node *is*, not where it landed.
//!
//! [`NodeKey`] is that identity: a node's [`NodeProvenance`] in canonical
//! form, plus an ordinal that separates the nodes sharing one provenance. Both
//! halves are compile-stable — provenance names source constructs rather than
//! compiled artifacts, and the ordinal comes from
//! [`SubGraph::deterministic_emit_order`], which is fixed by graph shape.
//!
//! What has no key is a node with no provenance: a region node, or a
//! hand-built synthetic node. Without source provenance these nodes have no
//! cross-program identity.

use std::collections::BTreeMap;
use std::fmt;

use crate::{
    Basis, Bloq, BloqNode, BloqNodeId, CycleDetected, NodeProvenance, PortRole, SubGraph,
    TemporalPipeRef,
};

/// A node identity that survives recompilation and renumbering.
///
/// Ordered and hashable so it can key a lookup table; the ordering is an
/// implementation detail of the provenance encoding and carries no meaning.
/// [`Display`](fmt::Display) renders it the way the `.bloqir` text format
/// spells provenance, suffixed with `#ordinal`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeKey {
    provenance: KeyProvenance,
    ordinal: u32,
}

impl NodeKey {
    /// This node's index among the nodes of its level that share its
    /// provenance, in [`SubGraph::deterministic_emit_order`].
    ///
    /// Always `0` on a key from [`BloqNode::stable_key`], which sees one node
    /// and so cannot count its peers; [`SubGraph::stable_keys`] assigns the
    /// real ordinals.
    #[must_use]
    pub fn ordinal(&self) -> u32 {
        self.ordinal
    }
}

/// Canonical provenance: the payload of [`NodeProvenance`] reduced to plain
/// ordered data, so a key can derive `Ord`/`Hash`.
///
/// Block members are sorted because a node's identity is the *set* of source
/// blocks it fused, not the order lowering happened to list them in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum KeyProvenance {
    Blocks(Vec<[i32; 3]>),
    Pipe {
        src: [i32; 3],
        dst: [i32; 3],
        hadamard: bool,
    },
    SpatialPort {
        source: [i32; 3],
        role: PortRole,
    },
    Padding {
        src: [i32; 3],
        dst: [i32; 3],
        rounds: u32,
    },
    Generator(u32),
    Action(u32),
    BranchSelector(String),
    OutputFrame {
        port: [i32; 3],
        basis: Basis,
    },
}

/// Mirrors [`NodeProvenance`]'s `Display`, so a key and the program view name
/// the same node alike.
impl fmt::Display for NodeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let coord = |f: &mut fmt::Formatter<'_>, [x, y, z]: [i32; 3]| write!(f, "({x},{y},{z})");
        match &self.provenance {
            KeyProvenance::Blocks(blocks) => {
                write!(f, "blocks")?;
                for &block in blocks {
                    write!(f, " ")?;
                    coord(f, block)?;
                }
                Ok(())
            }
            KeyProvenance::Pipe { src, dst, hadamard } => {
                write!(f, "pipe ")?;
                coord(f, *src)?;
                write!(f, ">")?;
                coord(f, *dst)?;
                if *hadamard {
                    write!(f, " H")?;
                }
                Ok(())
            }
            KeyProvenance::SpatialPort { source, role } => {
                write!(f, "spatial-port {} ", role.as_str())?;
                coord(f, *source)
            }
            KeyProvenance::Padding { src, dst, rounds } => {
                write!(f, "padding ")?;
                coord(f, *src)?;
                write!(f, ">")?;
                coord(f, *dst)?;
                write!(f, " rounds {rounds}")
            }
            KeyProvenance::Generator(ordinal) => write!(f, "generator {ordinal}"),
            KeyProvenance::Action(ordinal) => write!(f, "action {ordinal}"),
            KeyProvenance::BranchSelector(name) => write!(f, "selector {name}"),
            KeyProvenance::OutputFrame { port, basis } => {
                let basis = match basis {
                    Basis::X => "x",
                    Basis::Z => "z",
                };
                write!(f, "frame {basis} ")?;
                coord(f, *port)
            }
        }?;
        write!(f, "#{}", self.ordinal)
    }
}

fn pipe_key(pipe: &TemporalPipeRef) -> ([i32; 3], [i32; 3]) {
    (pipe.src.to_array(), pipe.dst.to_array())
}

impl BloqNode {
    /// This node's provenance-derived key, at ordinal `0`.
    ///
    /// `None` for a node with no provenance (a region node, or a hand-built
    /// synthetic one), which has no cross-program identity to report.
    ///
    /// Use [`SubGraph::stable_keys`] when nodes may share a provenance: the
    /// ordinal that separates them is a property of the level, not of any one
    /// node. Memory padding is the case that needs it — several padding nodes
    /// can idle on one pipe for the same number of rounds.
    #[must_use]
    pub fn stable_key(&self) -> Option<NodeKey> {
        let provenance = match &self.provenance {
            NodeProvenance::BlockComponent { members } => {
                let mut blocks: Vec<_> =
                    members.iter().map(|member| member.pos.to_array()).collect();
                blocks.sort_unstable();
                KeyProvenance::Blocks(blocks)
            }
            NodeProvenance::TemporalPipe { pipe } => {
                let (src, dst) = pipe_key(pipe);
                KeyProvenance::Pipe {
                    src,
                    dst,
                    hadamard: pipe.hadamard,
                }
            }
            NodeProvenance::SpatialPortSubstitution { source, role } => {
                KeyProvenance::SpatialPort {
                    source: source.to_array(),
                    role: *role,
                }
            }
            NodeProvenance::MemoryPadding { pipe, rounds } => {
                let (src, dst) = pipe_key(pipe);
                KeyProvenance::Padding {
                    src,
                    dst,
                    rounds: *rounds,
                }
            }
            NodeProvenance::Generator { ordinal } => KeyProvenance::Generator(*ordinal),
            NodeProvenance::Action { ordinal } => KeyProvenance::Action(*ordinal),
            NodeProvenance::BranchSelector { name } => KeyProvenance::BranchSelector(name.clone()),
            NodeProvenance::OutputFrame { port, basis } => KeyProvenance::OutputFrame {
                port: port.to_array(),
                basis: *basis,
            },
            NodeProvenance::None => return None,
        };
        Some(NodeKey {
            provenance,
            ordinal: 0,
        })
    }
}

impl SubGraph {
    /// This level's provenance-bearing nodes with their stable keys, in
    /// [`SubGraph::deterministic_emit_order`].
    ///
    /// Nodes sharing a provenance are numbered in that order, which is what
    /// makes the keys line up across two programs: the schedule follows graph
    /// shape, so corresponding nodes are reached in corresponding positions.
    /// Nodes with no provenance are skipped, not keyed.
    ///
    /// # Errors
    ///
    /// Returns [`CycleDetected`] when the level has no emit order to number in.
    pub fn stable_keys(&self) -> Result<Vec<(NodeKey, BloqNodeId)>, CycleDetected> {
        let mut ordinals: BTreeMap<KeyProvenance, u32> = BTreeMap::new();
        let mut keys = Vec::new();
        for id in self.deterministic_emit_order()? {
            let Some(key) = self[id].stable_key() else {
                continue;
            };
            let next = ordinals.entry(key.provenance.clone()).or_default();
            let ordinal = *next;
            *next += 1;
            keys.push((
                NodeKey {
                    provenance: key.provenance,
                    ordinal,
                },
                id,
            ));
        }
        Ok(keys)
    }
}

impl Bloq {
    /// The top level's [`stable keys`](SubGraph::stable_keys) as a lookup
    /// table, for resolving a key obtained from another program into a node id
    /// in this one.
    ///
    /// # Errors
    ///
    /// Returns [`CycleDetected`] when the program has no emit order.
    pub fn stable_key_map(&self) -> Result<BTreeMap<NodeKey, BloqNodeId>, CycleDetected> {
        Ok(self.top().stable_keys()?.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block, a temporal pipe, two padding nodes idling the *same* pipe for
    /// the same number of rounds, and a terminal block — the shape that made
    /// block membership alone an unusable identity, since padding nodes are
    /// spliced into a pipe and carry no blocks at all.
    ///
    /// `base` shifts every node id without touching any provenance: the
    /// stand-in for "the same graph, compiled a second time".
    fn program(base: u32) -> Bloq {
        let (a, b, c, d, e) = (base, base + 1, base + 2, base + 3, base + 4);
        Bloq::from_text(&format!(
            "\
BLOQIR 1

graph {{
  n{a} quantum {{
    from blocks (1,0,0) (0,0,0)
  }}
  n{b} quantum {{
    from pipe (0,0,0)>(0,0,1)
  }}
  n{c} quantum {{
    from padding (0,0,1)>(0,0,2) rounds 4
  }}
  n{d} quantum {{
    from padding (0,0,1)>(0,0,2) rounds 4
  }}
  n{e} quantum {{
    from blocks (0,0,2)
  }}
  n{a} -> n{b} order
  n{b} -> n{c} order
  n{c} -> n{d} order
  n{d} -> n{e} order
}}
"
        ))
        .expect("valid .bloqir text")
    }

    #[test]
    fn keys_name_provenance_and_disambiguate_with_an_ordinal() {
        let program = program(90);

        let rendered: Vec<_> = program
            .top()
            .stable_keys()
            .expect("acyclic program")
            .into_iter()
            .map(|(key, id)| (key.to_string(), id))
            .collect();

        assert_eq!(
            rendered,
            [
                ("blocks (0,0,0) (1,0,0)#0".to_owned(), BloqNodeId(90)),
                ("pipe (0,0,0)>(0,0,1)#0".to_owned(), BloqNodeId(91)),
                (
                    "padding (0,0,1)>(0,0,2) rounds 4#0".to_owned(),
                    BloqNodeId(92)
                ),
                (
                    "padding (0,0,1)>(0,0,2) rounds 4#1".to_owned(),
                    BloqNodeId(93)
                ),
                ("blocks (0,0,2)#0".to_owned(), BloqNodeId(94)),
            ],
            "members sort, and the two padding nodes separate by ordinal"
        );
    }

    /// The point of the whole module: renumbering must not change a key.
    #[test]
    fn keys_survive_renumbering() {
        let plain = program(0).stable_key_map().expect("keyed");
        let shifted = program(90).stable_key_map().expect("keyed");

        let keys: Vec<_> = plain.keys().collect();
        assert_eq!(
            keys,
            shifted.keys().collect::<Vec<_>>(),
            "the same source constructs key the same way in both programs"
        );
        // The ids behind them did move, which is what makes the keys earn
        // their keep.
        assert_eq!(plain[keys[0]], BloqNodeId(0));
        assert_eq!(shifted[keys[0]], BloqNodeId(90));
    }

    #[test]
    fn key_map_rejects_a_cyclic_program() {
        let mut program = program(0);
        program.add_edge(BloqNodeId(4), BloqNodeId(0), crate::BloqEdge::Order);

        assert_eq!(program.stable_key_map(), Err(CycleDetected));
    }

    #[test]
    fn provenance_free_nodes_have_no_key() {
        let program = Bloq::from_text(
            "\
BLOQIR 1

graph {
  n0 rus in0 {
    body {
    }
  }
  n1 quantum {
    from blocks (0,0,0)
  }
}
",
        )
        .expect("valid .bloqir text");

        assert_eq!(program[BloqNodeId(0)].stable_key(), None);
        assert_eq!(
            program
                .stable_key_map()
                .expect("keyed")
                .into_values()
                .collect::<Vec<_>>(),
            [BloqNodeId(1)],
            "the region has no key"
        );
    }
}
