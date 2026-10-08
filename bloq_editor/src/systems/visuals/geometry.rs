//! Geometry template cache and shared GPU mesh/material asset interning.

use super::*;

pub(super) fn cached_block_geometry<'a>(
    cache: &'a mut RenderGeometryCache,
    block: &Block,
    context: &RenderSignatureContext,
    graph: &BlockGraph,
    pipe_length: f32,
    signature: u64,
) -> &'a CachedRenderGeometry {
    if let Some(cached) = cache.blocks.get(&block.pos())
        && cached.geometry_hash == signature
    {
        return cache
            .block_templates
            .get(&cached.template_key)
            .expect("cached block template entry exists");
    }

    let template_key = block_geometry_template_key(block, context, pipe_length);
    let template = cache
        .block_templates
        .entry(template_key.clone())
        .or_insert_with(|| cached_block_geometry_template(block, graph, pipe_length));
    cache.blocks.insert(
        block.pos(),
        CachedBlockGeometry {
            template_key,
            geometry_hash: signature,
        },
    );
    template
}

fn cached_block_geometry_template(
    block: &Block,
    graph: &BlockGraph,
    pipe_length: f32,
) -> CachedRenderGeometry {
    let data = block_as_gltf_data_with_pipe_length(block, graph, pipe_length)
        .map_points(|p| graph_to_world(p, 0.0));
    let geometry = analyze_gltf_geometry(&data);
    CachedRenderGeometry {
        data,
        mesh_keys: geometry.mesh_keys,
        pick_proxy_min: geometry.pick_proxy.min,
        pick_proxy_max: geometry.pick_proxy.max,
    }
}

fn block_geometry_template_key(
    block: &Block,
    context: &RenderSignatureContext,
    pipe_length: f32,
) -> BlockGeometryTemplateKey {
    BlockGeometryTemplateKey {
        kind: block.kind(),
        height_cells: block.height_cells(),
        port_color: block.port_color(),
        pipe_length_bits: pipe_length.to_bits(),
        endpoint_pipe_masks: block
            .connectable_offsets()
            .into_iter()
            .map(|offset| {
                let endpoint = block.pos() + offset;
                ADJACENT_DIRECTIONS
                    .into_iter()
                    .enumerate()
                    .fold(0u8, |mask, (index, direction)| {
                        if endpoint
                            .checked_add(direction)
                            .is_some_and(|neighbor| context.has_pipe_between(endpoint, neighbor))
                        {
                            mask | (1 << index)
                        } else {
                            mask
                        }
                    })
            })
            .collect(),
    }
}

pub(super) fn cached_pipe_geometry<'a>(
    cache: &'a mut RenderGeometryCache,
    key: PipeKey,
    pipe: &Pipe,
    context: &RenderSignatureContext,
    graph: &BlockGraph,
    pipe_length: f32,
    signature: u64,
) -> &'a CachedRenderGeometry {
    if let Some(cached) = cache.pipes.get(&key)
        && cached.geometry_hash == signature
    {
        return cache
            .pipe_templates
            .get(&cached.template_key)
            .expect("cached pipe template entry exists");
    }

    let template_key =
        pipe_geometry_template_key(pipe.src(), pipe.dst(), pipe, context, pipe_length);
    let template = cache.pipe_templates.entry(template_key).or_insert_with(|| {
        cached_pipe_geometry_template(pipe.src(), pipe.dst(), pipe, graph, pipe_length)
    });
    cache.pipes.insert(
        key,
        CachedPipeGeometry {
            template_key,
            geometry_hash: signature,
        },
    );
    template
}

fn cached_pipe_geometry_template(
    u_pos: IVec3,
    v_pos: IVec3,
    pipe: &Pipe,
    graph: &BlockGraph,
    pipe_length: f32,
) -> CachedRenderGeometry {
    let data = pipe_between_positions_as_gltf_data(u_pos, v_pos, pipe, graph, pipe_length)
        .map_points(|p| graph_to_world(p, 0.0));
    let geometry = analyze_gltf_geometry(&data);
    CachedRenderGeometry {
        data,
        mesh_keys: geometry.mesh_keys,
        pick_proxy_min: geometry.pick_proxy.min,
        pick_proxy_max: geometry.pick_proxy.max,
    }
}

pub(super) fn pipe_geometry_template_key(
    u_pos: IVec3,
    v_pos: IVec3,
    pipe: &Pipe,
    context: &RenderSignatureContext,
    pipe_length: f32,
) -> PipeGeometryTemplateKey {
    let pipe_height = pipe_geometry_height(u_pos, v_pos, pipe, context, pipe_length);
    PipeGeometryTemplateKey {
        dir: v_pos - u_pos,
        hadamard: pipe.is_hadamard(),
        inferred_basis_bits: pipe_geometry_inferred_basis_bits(pipe, context),
        pipe_length_bits: pipe_length.to_bits(),
        pipe_height_bits: pipe_height.to_bits(),
    }
}

fn pipe_geometry_height(
    u_pos: IVec3,
    v_pos: IVec3,
    pipe: &Pipe,
    context: &RenderSignatureContext,
    pipe_length: f32,
) -> f32 {
    if !pipe.dir().is_spatial() {
        return 1.0;
    }
    let Some(u) = context.endpoint_block(u_pos) else {
        return 1.0;
    };
    let Some(v) = context.endpoint_block(v_pos) else {
        return 1.0;
    };
    if !u.kind.is_cube() || !v.kind.is_cube() || u.height_cells != v.height_cells {
        return 1.0;
    }
    if u.height_cells > 1 {
        (u.height_cells - 1) as f32 * (pipe_length + 1.0) + 1.0
    } else {
        1.0
    }
}

fn pipe_geometry_inferred_basis_bits(pipe: &Pipe, context: &RenderSignatureContext) -> u8 {
    let mut bits = 0u8;
    for (index, basis) in infer_pipe_basis_from_render_context(pipe, context)
        .into_iter()
        .enumerate()
    {
        let encoded = match basis {
            None => 0,
            Some(Basis::X) => 1,
            Some(Basis::Z) => 2,
        };
        bits |= encoded << (index * 2);
    }
    bits
}

pub(super) fn infer_pipe_basis_from_render_context(
    pipe: &Pipe,
    context: &RenderSignatureContext,
) -> [Option<Basis>; 3] {
    let mut bases = [None; 3];
    let Some(src) = context.endpoint_block(pipe.src()) else {
        return bases;
    };
    let Some(dst) = context.endpoint_block(pipe.dst()) else {
        return bases;
    };

    for (index, dir) in bloq_graph::UDirection::iter().enumerate() {
        if dir == pipe.dir().as_udirection() {
            continue;
        }
        if let Some(kind_bases) = src.kind.pipe_face_bases_at_endpoint(src.pos, pipe.src())
            && pipe_endpoint_is_unshadowed(pipe.src(), dir, context)
        {
            bases[index] = Some(kind_bases[index]);
        } else if let Some(kind_bases) = dst.kind.pipe_face_bases_at_endpoint(dst.pos, pipe.dst())
            && pipe_endpoint_is_unshadowed(pipe.dst(), dir, context)
        {
            let basis = kind_bases[index];
            bases[index] = Some(if pipe.is_hadamard() {
                basis.flip()
            } else {
                basis
            });
        }
    }

    let pipe_dir = pipe.dir().as_udirection().index();
    let none_count = bases.iter().filter(|basis| basis.is_none()).count();
    if none_count == 3 {
        bases[(pipe_dir + 1) % 3] = Some(Basis::X);
        bases[(pipe_dir + 2) % 3] = Some(Basis::Z);
    } else if none_count == 2
        && let Some(resolved_id) = bases.iter().position(Option::is_some)
    {
        let other_id = 3 - pipe_dir - resolved_id;
        if let Some(resolved) = bases[resolved_id] {
            bases[other_id] = Some(resolved.flip());
        }
    }
    bases
}

fn pipe_endpoint_is_unshadowed(
    endpoint: IVec3,
    dir: bloq_graph::UDirection,
    context: &RenderSignatureContext,
) -> bool {
    ![dir.to_ivec3(), -dir.to_ivec3()].into_iter().all(|offset| {
        endpoint
            .checked_add(offset)
            .is_some_and(|neighbor| context.has_pipe_between(endpoint, neighbor))
    })
}

pub(super) fn get_or_insert_triangle_mesh(
    render_cache: &mut RenderAssetCache,
    meshes: &mut Assets<Mesh>,
    positions: &[[f32; 3]],
) -> Handle<Mesh> {
    let key = hash_positions(positions, PrimitiveTopology::TriangleList);
    if let Some(existing) = render_cache.triangle_meshes.get(&key) {
        return existing.clone();
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.to_vec());
    mesh.compute_flat_normals();
    let handle = meshes.add(mesh);
    render_cache.triangle_meshes.insert(key, handle.clone());
    handle
}

pub(super) fn get_or_insert_triangle_mesh_from_triangles(
    render_cache: &mut RenderAssetCache,
    meshes: &mut Assets<Mesh>,
    triangles: &[[Vec3; 3]],
    precomputed_key: Option<u64>,
) -> Handle<Mesh> {
    let key = precomputed_key.unwrap_or_else(|| hash_triangle_positions(triangles));
    if let Some(existing) = render_cache.triangle_meshes.get(&key) {
        return existing.clone();
    }

    let mut positions = Vec::with_capacity(triangles.len() * 3);
    for triangle in triangles {
        for point in triangle {
            positions.push(point.to_array());
        }
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.compute_flat_normals();
    let handle = meshes.add(mesh);
    render_cache.triangle_meshes.insert(key, handle.clone());
    handle
}

pub(super) fn get_or_insert_line_mesh_from_lines(
    render_cache: &mut RenderAssetCache,
    meshes: &mut Assets<Mesh>,
    lines: &[[Vec3; 2]],
    precomputed_key: Option<u64>,
) -> Handle<Mesh> {
    let key = precomputed_key.unwrap_or_else(|| hash_line_positions(lines));
    if let Some(existing) = render_cache.line_meshes.get(&key) {
        return existing.clone();
    }

    let mut positions = Vec::with_capacity(lines.len() * 2);
    for line in lines {
        for point in line {
            positions.push(point.to_array());
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    let handle = meshes.add(mesh);
    render_cache.line_meshes.insert(key, handle.clone());
    handle
}

pub(super) fn get_or_insert_material(
    render_cache: &mut RenderAssetCache,
    materials: &mut Assets<StandardMaterial>,
    color: [f32; 4],
    style: MaterialStyle,
) -> Handle<StandardMaterial> {
    let key = MaterialCacheKey {
        rgba_bits: [
            color[0].to_bits(),
            color[1].to_bits(),
            color[2].to_bits(),
            color[3].to_bits(),
        ],
        unlit: style.unlit,
        double_sided: style.double_sided,
        cull_none: style.cull_none,
    };
    if let Some(existing) = render_cache.materials.get(&key) {
        return existing.clone();
    }

    let material = materials.add(StandardMaterial {
        // Palette channels are sRGB, so a fully lit face lands close to the
        // literal hex and shaded faces fall off from there.
        base_color: Color::Srgba(Srgba::from_f32_array(color)),
        // Purely diffuse, so a face's shade depends only on how squarely it
        // faces the light and never picks up a specular highlight that would
        // break the "color is the basis" read.
        metallic: 0.0,
        perceptual_roughness: 1.0,
        reflectance: 0.0,
        double_sided: style.double_sided,
        cull_mode: if style.cull_none {
            None
        } else {
            Some(Face::Back)
        },
        unlit: style.unlit,
        alpha_mode: if color[3] < 0.999 {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        },
        ..default()
    });

    render_cache.materials.insert(key, material.clone());
    material
}

pub(super) fn get_or_insert_text_material(
    render_cache: &mut RenderAssetCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(existing) = &render_cache.text_material {
        return existing.clone();
    }
    let handle = materials.add(StandardMaterial {
        base_color_texture: Some(TextAtlas::DEFAULT_IMAGE.clone()),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        cull_mode: None,
        ..Default::default()
    });
    render_cache.text_material = Some(handle.clone());
    handle
}

pub(super) fn hash_positions(positions: &[[f32; 3]], topology: PrimitiveTopology) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_topology(topology, &mut hasher);
    for p in positions {
        hash_position_array(p, &mut hasher);
    }
    hasher.finish()
}

pub(super) fn hash_triangle_positions(triangles: &[[Vec3; 3]]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_topology(PrimitiveTopology::TriangleList, &mut hasher);
    for triangle in triangles {
        for point in triangle {
            hash_vec3_position(*point, &mut hasher);
        }
    }
    hasher.finish()
}

pub(super) fn hash_line_positions(lines: &[[Vec3; 2]]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_topology(PrimitiveTopology::LineList, &mut hasher);
    for line in lines {
        for point in line {
            hash_vec3_position(*point, &mut hasher);
        }
    }
    hasher.finish()
}

pub(super) fn hash_topology(topology: PrimitiveTopology, hasher: &mut impl Hasher) {
    match topology {
        PrimitiveTopology::TriangleList => 1u8.hash(hasher),
        PrimitiveTopology::LineList => 2u8.hash(hasher),
        _ => 0u8.hash(hasher),
    }
}

fn hash_position_array(position: &[f32; 3], hasher: &mut impl Hasher) {
    position[0].to_bits().hash(hasher);
    position[1].to_bits().hash(hasher);
    position[2].to_bits().hash(hasher);
}

pub(super) fn hash_vec3_position(position: Vec3, hasher: &mut impl Hasher) {
    hash_position_array(&position.to_array(), hasher);
}

pub(super) fn hash_gltf_data(data: &GltfData) -> u64 {
    let mut hasher = DefaultHasher::new();

    for (rgba, triangles) in &data.triangles {
        for c in rgba.to_f32_array() {
            c.to_bits().hash(&mut hasher);
        }
        for tri in triangles {
            for p in tri {
                hash_vec3_position(*p, &mut hasher);
            }
        }
    }

    for (rgba, lines) in &data.lines {
        for c in rgba.to_f32_array() {
            c.to_bits().hash(&mut hasher);
        }
        for line in lines {
            for p in line {
                hash_vec3_position(*p, &mut hasher);
            }
        }
    }

    for label in &data.texts {
        let (pos, text, rgba) = (&label.position, &label.text, &label.color);
        hash_vec3_position(*pos, &mut hasher);
        text.hash(&mut hasher);
        for c in rgba.to_f32_array() {
            c.to_bits().hash(&mut hasher);
        }
    }

    hasher.finish()
}

/// Deduplication key for cached `StandardMaterial` handles (colours stored as
/// raw float bits so the key is `Hash`/`Eq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MaterialCacheKey {
    pub(crate) rgba_bits: [u32; 4],
    pub(crate) unlit: bool,
    pub(crate) double_sided: bool,
    pub(crate) cull_none: bool,
}

/// Deduplication key for cached pick-proxy box meshes, by AABB corner bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PickProxyCacheKey {
    pub(crate) min_bits: [u32; 3],
    pub(crate) max_bits: [u32; 3],
}

/// Interns reusable GPU assets (meshes, materials) keyed by content, so
/// identical geometry and colours share one handle.
#[derive(Resource, Default)]
pub(crate) struct RenderAssetCache {
    pub(crate) triangle_meshes: FxHashMap<u64, Handle<Mesh>>,
    pub(crate) line_meshes: FxHashMap<u64, Handle<Mesh>>,
    pub(crate) pick_proxy_meshes: FxHashMap<PickProxyCacheKey, Handle<Mesh>>,
    pub(crate) materials: FxHashMap<MaterialCacheKey, Handle<StandardMaterial>>,
    pub(crate) text_material: Option<Handle<StandardMaterial>>,
}

/// A geometry template's built glTF data, the mesh-cache keys it references,
/// and its pick-proxy bounds, reused by every element sharing the template.
#[derive(Clone)]
pub(crate) struct CachedRenderGeometry {
    pub(crate) data: GltfData,
    pub(crate) mesh_keys: CachedMeshKeys,
    pub(crate) pick_proxy_min: Vec3,
    pub(crate) pick_proxy_max: Vec3,
}

/// The attributes that make two pipes share one geometry template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PipeGeometryTemplateKey {
    pub(crate) dir: IVec3,
    pub(crate) hadamard: bool,
    pub(crate) inferred_basis_bits: u8,
    pub(crate) pipe_length_bits: u32,
    pub(crate) pipe_height_bits: u32,
}

/// A pipe's resolved geometry template and the hash of its built geometry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CachedPipeGeometry {
    pub(crate) template_key: PipeGeometryTemplateKey,
    pub(crate) geometry_hash: u64,
}

/// The attributes that make two blocks share one geometry template.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BlockGeometryTemplateKey {
    pub(crate) kind: BlockKind,
    pub(crate) height_cells: u32,
    pub(crate) port_color: Option<RGBA>,
    pub(crate) pipe_length_bits: u32,
    pub(crate) endpoint_pipe_masks: Vec<u8>,
}

/// A block's resolved geometry template and the hash of its built geometry.
#[derive(Debug, Clone)]
pub(crate) struct CachedBlockGeometry {
    pub(crate) template_key: BlockGeometryTemplateKey,
    pub(crate) geometry_hash: u64,
}

/// The triangle- and line-mesh cache keys that make up one element's geometry.
#[derive(Clone, Default)]
pub(crate) struct CachedMeshKeys {
    pub(crate) triangles: Vec<u64>,
    pub(crate) lines: Vec<u64>,
}

/// Two-level geometry cache: per-element resolved templates and built template
/// geometry shared by matching elements.
#[derive(Default)]
pub(crate) struct RenderGeometryCache {
    /// Pipe length the cached generation was built for. Changing it invalidates
    /// every geometry-derived key, so we sweep stale generations on change
    /// instead of letting their meshes/templates leak (see F6b eviction).
    pub(super) pipe_length_generation: Option<u32>,
    pub(super) blocks: FxHashMap<IVec3, CachedBlockGeometry>,
    pub(super) block_templates: FxHashMap<BlockGeometryTemplateKey, CachedRenderGeometry>,
    pub(super) pipes: FxHashMap<PipeKey, CachedPipeGeometry>,
    pub(super) pipe_templates: FxHashMap<PipeGeometryTemplateKey, CachedRenderGeometry>,
}
