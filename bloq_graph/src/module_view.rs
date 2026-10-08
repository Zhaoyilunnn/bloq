//! Definition ownership and colors shared by module viewers.

use std::collections::HashMap;

use bloq_utils::RGBA;
use glam::IVec3;

use crate::{BlockGraph, ModuleOrientation};

/// One visible definition in a module view.
#[derive(Debug, Clone)]
pub struct ModuleViewModule {
    /// Authored definition name.
    pub name: String,
    /// Number of child instances of this definition.
    pub instance_count: usize,
    /// Number of visible blocks owned by these instances.
    pub block_count: usize,
}

/// Definition ownership for a flat rendering projection of an authored graph.
///
/// Reused child definitions share one color. Root blocks and pipes between
/// different definitions retain their ordinary graph colors.
#[derive(Debug, Clone)]
pub struct ModuleView {
    modules: Vec<ModuleViewModule>,
    block_modules: HashMap<IVec3, usize>,
}

impl ModuleView {
    /// Matches authored definition ownership against a rendering projection.
    ///
    /// The root and definitions without visible blocks are omitted. The authored hierarchy
    /// and its projection are not modified or validated by this operation.
    ///
    /// # Panics
    ///
    /// Panics if an instance references a missing definition. Pass a validated
    /// source and its matching flat projection.
    pub fn from_graph(program: &BlockGraph, graph: &BlockGraph) -> Option<Self> {
        let mut modules = Vec::new();
        let mut module_indices = HashMap::new();
        let mut block_modules = HashMap::new();
        collect_module_blocks(
            program,
            program.root(),
            ModuleOrientation::IDENTITY,
            IVec3::ZERO,
            true,
            graph,
            &mut modules,
            &mut module_indices,
            &mut block_modules,
        );

        let mut block_counts = vec![0; modules.len()];
        for index in block_modules.values() {
            block_counts[*index] += 1;
        }
        let mut remap = vec![None; modules.len()];
        let mut visible = Vec::new();
        for (old_index, mut module) in modules.into_iter().enumerate() {
            let block_count = block_counts[old_index];
            if block_count == 0 {
                continue;
            }
            module.block_count = block_count;
            remap[old_index] = Some(visible.len());
            visible.push(module);
        }
        block_modules.retain(|_, index| {
            let Some(next) = remap[*index] else {
                return false;
            };
            *index = next;
            true
        });

        (!visible.is_empty()).then_some(Self {
            modules: visible,
            block_modules,
        })
    }

    /// Visible definitions in their authored traversal order.
    pub fn modules(&self) -> &[ModuleViewModule] {
        &self.modules
    }

    /// Returns the owning definition's index for a block anchor.
    pub fn module_for_block(&self, position: IVec3) -> Option<usize> {
        self.block_modules.get(&position).copied()
    }

    /// Returns a pipe's owner only when both endpoint blocks share a definition.
    ///
    /// Resolves endpoint positions through `graph`, including extended blocks.
    pub fn module_for_pipe(&self, graph: &BlockGraph, a: IVec3, b: IVec3) -> Option<usize> {
        let a = self.module_for_block(graph.get_endpoint_block(a)?.pos())?;
        let b = self.module_for_block(graph.get_endpoint_block(b)?.pos())?;
        (a == b).then_some(a)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "shared editor traversal preserves ownership ordering"
)]
fn collect_module_blocks(
    program: &BlockGraph,
    module: &BlockGraph,
    orientation: ModuleOrientation,
    translation: IVec3,
    is_root: bool,
    graph: &BlockGraph,
    modules: &mut Vec<ModuleViewModule>,
    module_indices: &mut HashMap<String, usize>,
    block_modules: &mut HashMap<IVec3, usize>,
) {
    if !is_root {
        let index = *module_indices
            .entry(module.name.clone())
            .or_insert_with(|| {
                modules.push(ModuleViewModule {
                    name: module.name.clone(),
                    instance_count: 0,
                    block_count: 0,
                });
                modules.len() - 1
            });
        modules[index].instance_count += 1;

        for block in module.local_body().blocks() {
            if module
                .interface
                .quantum_ports
                .iter()
                .any(|port| port.position == block.pos())
            {
                continue;
            }
            let Some(position) = orientation
                .try_transform_position(block.pos(), translation)
                .ok()
            else {
                continue;
            };
            if graph.has_block_at(position) {
                block_modules.insert(position, index);
            }
        }
    }

    for instance in &module.instances {
        let Some(translation) = orientation
            .try_transform_position(instance.translation, translation)
            .ok()
        else {
            continue;
        };
        collect_module_blocks(
            program,
            program
                .module(&instance.definition)
                .expect("validated instance names a module"),
            orientation.then(instance.rotation),
            translation,
            false,
            graph,
            modules,
            module_indices,
            block_modules,
        );
    }
}

/// Stable ownership colors used by Bloq Editor and exported module viewers.
///
/// The first ten are a qualitative palette. Further definitions continue around
/// the hue wheel instead of reusing a swatch.
pub fn module_color(index: usize) -> RGBA {
    const COLORS: [RGBA; 10] = [
        RGBA::from_hex(0x4477AA, 255),
        RGBA::from_hex(0xEE6677, 255),
        RGBA::from_hex(0x228833, 255),
        RGBA::from_hex(0xCCBB44, 255),
        RGBA::from_hex(0x66CCEE, 255),
        RGBA::from_hex(0xAA3377, 255),
        RGBA::from_hex(0xEE7733, 255),
        RGBA::from_hex(0x332288, 255),
        RGBA::from_hex(0x44AA99, 255),
        RGBA::from_hex(0x999999, 255),
    ];
    if let Some(color) = COLORS.get(index) {
        return *color;
    }
    // Keep the editor's linear-HSV palette and its sRGB conversion.
    let hue = ((index as f32 * 0.618_034).fract() + 1.0).fract() * 6.0;
    let fraction = hue - hue.floor();
    let value = 0.82;
    let saturation = 0.72;
    let low = value * (1.0 - saturation);
    let falling = value * (1.0 - fraction * saturation);
    let rising = value * (1.0 - (1.0 - fraction) * saturation);
    let rgb = match hue.floor() as u8 {
        0 => [value, rising, low],
        1 => [falling, value, low],
        2 => [low, value, rising],
        3 => [low, falling, value],
        4 => [rising, low, value],
        _ => [value, low, falling],
    };
    let [r, g, b] = rgb.map(|linear: f32| {
        let gamma = if linear <= 0.003_130_8 {
            3294.6 * linear
        } else {
            269.025 * linear.powf(1.0 / 2.4) - 14.025
        };
        (gamma + 0.5) as u8
    });
    RGBA { r, g, b, a: 255 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GalleryItem;
    use std::collections::HashSet;

    #[test]
    fn reused_definitions_keep_one_ownership_group_and_seams_uncolored() {
        let program = GalleryItem::ThreeBitAdder.build();
        let graph = program.flatten().unwrap();
        let view = ModuleView::from_graph(&program, &graph).unwrap();
        assert_eq!(
            view.modules()
                .iter()
                .map(|module| (module.name.as_str(), module.instance_count))
                .collect::<Vec<_>>(),
            [
                ("InjectedAnd", 3),
                ("HeadMaj", 1),
                ("HeadUmaParked", 1),
                ("BulkMaj", 1),
                ("BulkUmaParked", 1),
                ("TailParity", 1)
            ]
        );
        let mut counts = vec![0; view.modules().len()];
        for block in graph.blocks() {
            if let Some(index) = view.module_for_block(block.pos()) {
                counts[index] += 1;
            } else {
                assert!(program.root().local_body().has_block_at(block.pos()));
            }
        }
        assert_eq!(
            counts,
            view.modules()
                .iter()
                .map(|module| module.block_count)
                .collect::<Vec<_>>()
        );
        let mut seams = 0;
        for (a, b, lhs, rhs, _) in graph.pipe_endpoints_with_blocks() {
            let left = view.module_for_block(lhs.pos());
            let right = view.module_for_block(rhs.pos());
            let expected = left.filter(|_| left == right);
            assert_eq!(view.module_for_pipe(&graph, a, b), expected);
            assert_eq!(view.module_for_pipe(&graph, b, a), expected);
            seams += usize::from(expected.is_none());
        }
        assert!(seams > 0);
        for block in program.root().local_body().blocks() {
            assert_eq!(view.module_for_block(block.pos()), None);
        }
    }

    #[test]
    fn root_only_and_empty_graphs_have_no_module_highlights() {
        let program = GalleryItem::OneDYoked.build();
        let graph = program.flatten().unwrap();
        assert!(ModuleView::from_graph(&program, &graph).is_none());
        assert!(ModuleView::from_graph(&BlockGraph::new(), &BlockGraph::new()).is_none());
    }

    #[test]
    fn first_thirty_two_module_colors_are_distinct() {
        assert_eq!((0..32).map(module_color).collect::<HashSet<_>>().len(), 32);
    }
}
