//! Controlled-adder scaling fixtures tiled from the three-bit gallery.
use bloq_graph::{
    Action, BitBinding, BitOutput, BitRef, Block, BlockGraph, BranchArm, Direction, Expr,
    GalleryItem, InstancePort, ModuleInterface, Pipe, QuantumConnection,
};
use glam::IVec3;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Tile the adaptive gallery adder, retaining each erase-controlled final CZ.
///
/// # Panics
///
/// Panics for fewer than three bits, coordinate overflow, or an invalid seed.
pub fn controlled_adder(bits: usize) -> BlockGraph {
    assert!(
        bits >= 3 && bits <= (i32::MAX as usize - 10) / 3,
        "adder size must fit at least three bits in i32 coordinates"
    );
    build(&GalleryItem::ThreeBitAdder.build(), bits).expect("valid tiled adder")
}

fn bit_name(name: &str, from: usize, to: usize) -> String {
    for prefix in ["bit", "i", "t", "s", "cz_target_", "cz_q_contact_"] {
        if let Some(suffix) = name.strip_prefix(&format!("{prefix}{from}"))
            && (suffix.is_empty() || suffix.starts_with('_'))
        {
            return format!("{prefix}{to}{suffix}");
        }
    }
    name.to_owned()
}

fn build(seed: &BlockGraph, bits: usize) -> Result<BlockGraph> {
    let mut definitions = seed
        .modules()
        .map(BlockGraph::clone_local_definition)
        .collect::<Vec<_>>();
    let mut and = GalleryItem::CCZInjectedAnd.build().clone_local_definition();
    and.name = "InjectedAnd".into();
    *definitions
        .iter_mut()
        .find(|module| module.name == "InjectedAnd")
        .expect("three-bit adder defines InjectedAnd") = and;
    let root = definitions
        .iter_mut()
        .find(|module| module.name == "main")
        .expect("three-bit adder defines main");
    let source = root.clone();
    assert_eq!(
        source.bit_bindings.len(),
        2,
        "three-bit adder has two carry bindings"
    );
    assert_eq!(
        source.branch_definitions().len(),
        3,
        "three-bit adder has one correction branch per bit"
    );
    *root = BlockGraph::definition(
        root.name.clone(),
        BlockGraph::new(),
        ModuleInterface::default(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut regions = Vec::new();
    let mut shared_pipes = Vec::new();
    for bit in 0..bits {
        let from = if bit == 0 {
            0
        } else if bit + 1 == bits {
            2
        } else {
            1
        };
        let first = if from == 0 { -1 } else { 3 * from as i32 };
        let end = if from == 2 { 10 } else { 3 * (from + 1) as i32 };
        let inside = |position: IVec3| (first..end).contains(&position.y);
        let offset = IVec3::new(0, 3 * (bit as i32 - from as i32), 0);
        let rename = |name: &str| bit_name(name, from, bit);
        let endpoint = |port: &InstancePort| InstancePort {
            instance: rename(&port.instance),
            port: port.port.clone(),
        };
        let local = |port: &InstancePort| port.instance.starts_with(&format!("bit{from}_"));
        let branch = source
            .branch_definitions()
            .iter()
            .find(|region| region.name == format!("cz{from}"))
            .expect("each seed bit declares its final CZ correction");
        let shift_block = |block: &Block| -> Result<Block> {
            let mut block = block.try_with_shift(offset)?;
            if let Some(tag) = block.tag().map(rename) {
                block = block.with_tag(tag)?;
            }
            Ok(block)
        };
        for block in source
            .blocks()
            .filter(|block| inside(block.pos()) && !branch.shown_arm().contains_block(block.pos()))
        {
            root.try_add_block(shift_block(block)?)?;
        }
        for pipe in source.pipes().filter(|pipe| {
            inside(pipe.src())
                && inside(pipe.dst())
                && !branch.shown_arm().pipes().any(|owned| owned == *pipe)
        }) {
            shared_pipes.push(pipe.try_with_shift(offset)?);
        }
        let shift_arm = |arm: &BranchArm| -> Result<BranchArm> {
            Ok(BranchArm::new(
                arm.blocks().map(shift_block).collect::<Result<_>>()?,
                arm.pipes()
                    .map(|pipe| pipe.try_with_shift(offset))
                    .collect::<std::result::Result<_, _>>()?,
            ))
        };
        regions.push((
            format!("cz{bit}"),
            shift_arm(branch.on_false())?,
            shift_arm(branch.on_true())?,
        ));
        for port in source
            .interface
            .quantum_ports
            .iter()
            .filter(|port| inside(port.position))
        {
            let mut port = port.clone();
            port.position += offset;
            port.name = rename(&port.name);
            root.interface.quantum_ports.push(port);
        }
        for instance in source
            .instances
            .iter()
            .filter(|instance| instance.name.starts_with(&format!("bit{from}_")))
        {
            let mut instance = instance.clone();
            instance.name = rename(&instance.name);
            instance.translation += offset;
            root.instances.push(instance);
        }
        for connection in &source.quantum_connections {
            let connection = match connection {
                QuantumConnection::Input {
                    block,
                    input,
                    hadamard,
                } if local(input) => {
                    assert!(inside(*block), "input connection belongs to this bit");
                    QuantumConnection::Input {
                        block: *block + offset,
                        input: endpoint(input),
                        hadamard: *hadamard,
                    }
                }
                QuantumConnection::Output {
                    output,
                    block,
                    hadamard,
                } if local(output) => {
                    assert!(inside(*block), "output connection belongs to this bit");
                    QuantumConnection::Output {
                        output: endpoint(output),
                        block: *block + offset,
                        hadamard: *hadamard,
                    }
                }
                QuantumConnection::Pipe {
                    output,
                    input,
                    hadamard,
                } if local(output) && local(input) => QuantumConnection::Pipe {
                    output: endpoint(output),
                    input: endpoint(input),
                    hadamard: *hadamard,
                },
                _ => continue,
            };
            root.quantum_connections.push(connection);
        }
        root.interface.bit_outputs.push(BitOutput {
            name: format!("raw_erase_{bit}"),
            expr: Expr::Var(if bit + 1 == bits {
                format!("bit{bit}_tail.m_ikprime")
            } else {
                format!("bit{bit}_uma.erase")
            }),
        });
        if bit + 1 < bits {
            let tail = bit + 2 == bits;
            root.bit_bindings.push(BitBinding {
                source: BitRef {
                    instance: Some(format!(
                        "bit{}_{}",
                        bit + 1,
                        if tail { "tail" } else { "uma" }
                    )),
                    bit: if tail { "m_ikprime" } else { "carry_z" }.into(),
                },
                target_instance: format!("bit{bit}_uma"),
                target_bit: "incoming_z".into(),
            });
        }
        if bit > 0 {
            for z in [0, 12] {
                shared_pipes.push(Pipe::new(
                    IVec3::new(11, 3 * bit as i32 - 1, z),
                    Direction::YPLUS,
                ));
            }
            root.quantum_connections.push(QuantumConnection::Pipe {
                output: InstancePort {
                    instance: format!("bit{}_maj", bit - 1),
                    port: "c_k_plus_1".into(),
                },
                input: InstancePort {
                    instance: if bit + 1 == bits {
                        format!("bit{bit}_tail")
                    } else {
                        format!("bit{bit}_maj")
                    },
                    port: "c_k".into(),
                },
                hadamard: false,
            });
        }
    }
    let targets = root.try_add_branch_regions(regions, shared_pipes)?;
    let mut inputs = Vec::new();
    let actions = targets
        .into_iter()
        .enumerate()
        .map(|(bit, target)| {
            let condition = root.interface.bit_outputs[bit].expr.clone();
            let Expr::Var(name) = &condition else {
                panic!("fixture exports the erase readout");
            };
            inputs.push(name.clone());
            Action::Branch { target, condition }
        })
        .collect();
    root.set_actions_with_inputs(actions, inputs)?;
    Ok(BlockGraph::from_definitions(definitions)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiled_adders_match_gallery_geometry() {
        for (bits, item) in [
            (3, GalleryItem::ThreeBitAdder),
            (10, GalleryItem::TenBitAdder),
        ] {
            let generated = controlled_adder(bits).flatten().unwrap();
            let expected = item.build().flatten().unwrap();
            assert_eq!(
                generated.blocks().collect::<std::collections::HashSet<_>>(),
                expected.blocks().collect()
            );
            assert_eq!(
                generated.pipes().collect::<std::collections::HashSet<_>>(),
                expected.pipes().collect()
            );
            assert_eq!(generated.actions(), expected.actions());
            let branches = |graph: &BlockGraph| {
                graph
                    .branch_definitions()
                    .iter()
                    .map(|region| {
                        (
                            region.name.clone(),
                            region.on_false().clone(),
                            region.on_true().clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(branches(&generated), branches(&expected));
            assert_eq!(generated.branch_definitions().len(), bits);
        }
        let larger = controlled_adder(20).flatten().unwrap();
        larger.validate_source().unwrap();
        assert_eq!(larger.branch_definitions().len(), 20);
    }
}
