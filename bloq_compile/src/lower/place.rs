use bloq_ir::lowering::TemplateInstanceId;
use glam::IVec3;
use petgraph::graph::NodeIndex;

use crate::{BlockLayout, CompileError};

use super::classify::NodeLowering;
use super::{ChunkSiteSource, LoweredTemplateInstance, PlacedInstance, TemplateInstanceAllocator};

pub(super) struct PlacedInstances {
    /// Lowered instances per plan node, indexed by `NodeIndex::index()`.
    pub(super) node_instances: Vec<Vec<LoweredTemplateInstance>>,
    /// T block position → its cultivation instance id. The cultivation instance
    /// lives inside the RUS body (built at node materialization), but its id is
    /// allocated here — before the escape's — so instance ids follow emission
    /// order, and the intra-body seam pass can label its flows.
    pub(super) t_cultivation_instances: crate::FxMap<IVec3, TemplateInstanceId>,
}

/// Allocate and place one `TemplateInstance` per node member (or one per
/// temporal-pipe node) in emission order. **No circuit is materialized.**
pub(super) fn place_instances(
    classified: &[NodeLowering],
    lower_order: &[NodeIndex],
    layout: BlockLayout,
    instance_allocator: &mut TemplateInstanceAllocator,
) -> Result<PlacedInstances, CompileError> {
    let mut node_instances = vec![Vec::new(); classified.len()];
    let mut t_cultivation_instances = crate::FxMap::default();
    for &plan_index in lower_order {
        let instances = &mut node_instances[plan_index.index()];
        match &classified[plan_index.index()] {
            // Selective arms are allocated together at node materialization.
            NodeLowering::Selective { .. } => {}
            // A T block lowers to a `RepeatUntilSuccess` region. Both stage
            // instances live inside its body, but the escape instance doubles as
            // the block's temporal face (U8's source-only residual): registering
            // it in `node_instances` lets the top-level detector pass close the
            // +Z neighbour's seam against it and the observable pass resolve the
            // site.
            NodeLowering::TRegion { pos, escape, .. } => {
                t_cultivation_instances.insert(*pos, instance_allocator.allocate());
                instances.push(LoweredTemplateInstance {
                    site_source: ChunkSiteSource::Block(*pos),
                    instance: PlacedInstance {
                        id: instance_allocator.allocate(),
                        template: *escape,
                        offset: layout.offset(*pos)?,
                    },
                });
            }
            NodeLowering::TemporalPipe {
                pipe,
                template,
                origin,
            } => {
                instances.push(LoweredTemplateInstance {
                    site_source: ChunkSiteSource::TemporalPipe(*pipe),
                    instance: PlacedInstance {
                        id: instance_allocator.allocate(),
                        template: *template,
                        offset: layout.offset(*origin)?,
                    },
                });
            }
            NodeLowering::SpatialPort { source, template } => {
                instances.push(LoweredTemplateInstance {
                    site_source: ChunkSiteSource::SpatialPort(*source),
                    instance: PlacedInstance {
                        id: instance_allocator.allocate(),
                        template: *template,
                        offset: layout.offset(*source)?,
                    },
                });
            }
            NodeLowering::Fixed { members, walls } => {
                for &(member, template) in members {
                    instances.push(LoweredTemplateInstance {
                        site_source: ChunkSiteSource::Block(member.pos),
                        instance: PlacedInstance {
                            id: instance_allocator.allocate(),
                            template,
                            offset: layout.offset(member.pos)?,
                        },
                    });
                }
                // Walls come after the members so a shared seam qubit's physical
                // reset/readout is owned by its cube; the wall's duplicate
                // aliases onto it when the node's instances merge.
                for wall in walls {
                    instances.push(LoweredTemplateInstance {
                        site_source: ChunkSiteSource::SpatialPipe(wall.pipe),
                        instance: PlacedInstance {
                            id: instance_allocator.allocate(),
                            template: wall.template,
                            offset: layout.offset(wall.pipe.src)?,
                        },
                    });
                }
            }
        }
    }
    Ok(PlacedInstances {
        node_instances,
        t_cultivation_instances,
    })
}
