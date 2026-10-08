//! Render-signature incremental-diff engine: per-element identity/adjacency
//! tracking and the signature/geometry hashing that decides what changed.

use super::*;

pub(super) struct RenderSignatureContext<'a> {
    adjacency: &'a GraphAdjacencySnapshot,
    block_signatures: &'a FxHashMap<IVec3, u64>,
    pipe_signatures: &'a FxHashMap<PipeKey, u64>,
}

impl<'a> RenderSignatureContext<'a> {
    pub(super) fn cached(
        cache: &'a mut RenderSignatureContextCache,
        tabs: &EditorTabs,
        graph_state: &GraphState,
        pipe_length: f32,
    ) -> Self {
        let sync_state = RenderSignatureContextSyncState {
            tab_id: tabs.active,
            graph_revision: graph_state.revision,
            pipe_length_bits: pipe_length.to_bits(),
        };
        if cache.applied != Some(sync_state) {
            sync_render_signature_context_cache(
                cache,
                sync_state,
                &graph_state.graph,
                pipe_length,
                Some(&graph_state.edit_delta),
            );
            cache.applied = Some(sync_state);
        }
        Self {
            adjacency: &cache.adjacency,
            block_signatures: &cache.block_signatures,
            pipe_signatures: &cache.pipe_signatures,
        }
    }

    pub(super) fn from_cache(cache: &'a RenderSignatureContextCache) -> Self {
        Self {
            adjacency: &cache.adjacency,
            block_signatures: &cache.block_signatures,
            pipe_signatures: &cache.pipe_signatures,
        }
    }

    pub(super) fn has_pipe_between(&self, u: IVec3, v: IVec3) -> bool {
        self.adjacency.has_pipe_between(u, v)
    }

    pub(super) fn endpoint_block(&self, endpoint: IVec3) -> Option<crate::utils::SnapshotBlock> {
        self.adjacency.endpoint_block(endpoint)
    }

    pub(super) fn block_geometry_signature(&self, block: &Block, pipe_length: f32) -> u64 {
        self.block_signatures
            .get(&block.pos())
            .copied()
            .unwrap_or_else(|| block_render_geometry_signature(block, self, pipe_length))
    }

    pub(super) fn pipe_geometry_signature(
        &self,
        u_pos: IVec3,
        v_pos: IVec3,
        pipe: &Pipe,
        pipe_length: f32,
    ) -> u64 {
        self.pipe_signatures
            .get(&PipeKey::new(u_pos, v_pos))
            .copied()
            .unwrap_or_else(|| {
                pipe_render_geometry_signature(u_pos, v_pos, pipe, self, pipe_length)
            })
    }
}

pub(super) fn rebuild_render_signature_context_cache(
    cache: &mut RenderSignatureContextCache,
    graph: &BlockGraph,
    pipe_length: f32,
) {
    let block_identities = collect_render_signature_block_identities(graph);
    let pipe_identities = collect_render_signature_pipe_identities(graph);
    cache.adjacency =
        GraphAdjacencySnapshot::from_render_identities(&block_identities, &pipe_identities);
    let context = RenderSignatureContext::from_cache(cache);
    let block_signatures = graph
        .blocks()
        .map(|block| {
            (
                block.pos(),
                block_render_geometry_signature(block, &context, pipe_length),
            )
        })
        .collect();
    let pipe_signatures = graph
        .pipe_endpoints_with_blocks()
        .map(|(u, v, _, _, pipe)| {
            (
                PipeKey::new(u, v),
                pipe_render_geometry_signature(u, v, pipe, &context, pipe_length),
            )
        })
        .collect();
    cache.block_signatures = block_signatures;
    cache.pipe_signatures = pipe_signatures;
    cache.block_identities = block_identities;
    cache.pipe_identities = pipe_identities;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenderSignatureContextCacheSyncMode {
    Full,
    Incremental,
}

pub(super) fn sync_render_signature_context_cache(
    cache: &mut RenderSignatureContextCache,
    sync_state: RenderSignatureContextSyncState,
    graph: &BlockGraph,
    pipe_length: f32,
    edit_delta: Option<&crate::resources::GraphEditDelta>,
) -> RenderSignatureContextCacheSyncMode {
    let Some(previous) = cache.applied else {
        rebuild_render_signature_context_cache(cache, graph, pipe_length);
        return RenderSignatureContextCacheSyncMode::Full;
    };
    if !can_incrementally_sync_render_signature_context(previous, sync_state) {
        rebuild_render_signature_context_cache(cache, graph, pipe_length);
        return RenderSignatureContextCacheSyncMode::Full;
    }

    rebuild_render_signature_context_cache_incremental_with_delta(
        cache,
        graph,
        pipe_length,
        edit_delta.and_then(RenderSignatureEditDelta::from_graph_edit_delta),
    );
    RenderSignatureContextCacheSyncMode::Incremental
}

fn can_incrementally_sync_render_signature_context(
    previous: RenderSignatureContextSyncState,
    sync_state: RenderSignatureContextSyncState,
) -> bool {
    previous.tab_id == sync_state.tab_id
        && previous.pipe_length_bits == sync_state.pipe_length_bits
        && previous.graph_revision != sync_state.graph_revision
}

fn rebuild_render_signature_context_cache_incremental_with_delta(
    cache: &mut RenderSignatureContextCache,
    graph: &BlockGraph,
    pipe_length: f32,
    edit_delta: Option<RenderSignatureEditDelta>,
) {
    let previous_blocks = std::mem::take(&mut cache.block_identities);
    let previous_pipes = std::mem::take(&mut cache.pipe_identities);
    let (next_blocks, next_pipes, diff) =
        collect_render_signature_identities(graph, previous_blocks, previous_pipes, edit_delta);

    sync_incremental_adjacency_snapshot(cache, &next_blocks, &next_pipes, &diff, graph);
    let mut affected_blocks = FxHashSet::default();
    let mut affected_pipes = FxHashSet::default();
    collect_affected_render_signature_elements(
        &cache.adjacency,
        &diff,
        &mut affected_blocks,
        &mut affected_pipes,
    );

    update_affected_render_signatures(cache, graph, pipe_length, affected_blocks, affected_pipes);
    cache.block_identities = next_blocks;
    cache.pipe_identities = next_pipes;
}

#[derive(Clone, Copy)]
enum RenderSignatureEditDelta {
    PipeAdded(RenderSignaturePipeIdentity),
    PipeRemoved(RenderSignaturePipeIdentity),
}

impl RenderSignatureEditDelta {
    fn from_graph_edit_delta(delta: &crate::resources::GraphEditDelta) -> Option<Self> {
        use crate::resources::GraphEditDelta;
        match delta {
            GraphEditDelta::PipeAdded { src, dst, hadamard } => {
                Some(Self::PipeAdded(RenderSignaturePipeIdentity {
                    src: *src,
                    dir: *dst - *src,
                    hadamard: *hadamard,
                }))
            }
            GraphEditDelta::PipeRemoved { src, dst, hadamard } => {
                Some(Self::PipeRemoved(RenderSignaturePipeIdentity {
                    src: *src,
                    dir: *dst - *src,
                    hadamard: *hadamard,
                }))
            }
            GraphEditDelta::Unknown => None,
        }
    }
}

#[derive(Default)]
struct RenderSignatureIdentityDiff {
    previous_blocks: Vec<(IVec3, RenderSignatureBlockIdentity)>,
    next_blocks: Vec<(IVec3, RenderSignatureBlockIdentity)>,
    previous_pipes: Vec<(PipeKey, RenderSignaturePipeIdentity)>,
    next_pipes: Vec<(PipeKey, RenderSignaturePipeIdentity)>,
}

impl RenderSignatureIdentityDiff {
    fn has_block_changes(&self) -> bool {
        !self.previous_blocks.is_empty() || !self.next_blocks.is_empty()
    }
}

fn collect_render_signature_identities(
    graph: &BlockGraph,
    previous_blocks: FxHashMap<IVec3, RenderSignatureBlockIdentity>,
    previous_pipes: FxHashMap<PipeKey, RenderSignaturePipeIdentity>,
    edit_delta: Option<RenderSignatureEditDelta>,
) -> (
    FxHashMap<IVec3, RenderSignatureBlockIdentity>,
    FxHashMap<PipeKey, RenderSignaturePipeIdentity>,
    RenderSignatureIdentityDiff,
) {
    if let Some(edit_delta) = edit_delta {
        collect_render_signature_identities_from_delta(
            graph,
            previous_blocks,
            previous_pipes,
            edit_delta,
        )
    } else {
        let mut diff = RenderSignatureIdentityDiff::default();
        let next_blocks =
            collect_render_signature_block_identities_with_diff(graph, &previous_blocks, &mut diff);
        let next_pipes =
            collect_render_signature_pipe_identities_with_diff(graph, &previous_pipes, &mut diff);
        (next_blocks, next_pipes, diff)
    }
}

fn collect_render_signature_identities_from_delta(
    graph: &BlockGraph,
    previous_blocks: FxHashMap<IVec3, RenderSignatureBlockIdentity>,
    previous_pipes: FxHashMap<PipeKey, RenderSignaturePipeIdentity>,
    edit_delta: RenderSignatureEditDelta,
) -> (
    FxHashMap<IVec3, RenderSignatureBlockIdentity>,
    FxHashMap<PipeKey, RenderSignaturePipeIdentity>,
    RenderSignatureIdentityDiff,
) {
    debug_assert_eq!(previous_blocks.len(), graph.block_count());
    let mut next_pipes = previous_pipes;
    let mut diff = RenderSignatureIdentityDiff::default();
    match edit_delta {
        RenderSignatureEditDelta::PipeAdded(identity) => {
            let key = PipeKey::new(identity.src, identity.src + identity.dir);
            match next_pipes.insert(key, identity) {
                Some(previous) if previous != identity => {
                    diff.previous_pipes.push((key, previous));
                    diff.next_pipes.push((key, identity));
                }
                Some(_) => {}
                None => diff.next_pipes.push((key, identity)),
            }
        }
        RenderSignatureEditDelta::PipeRemoved(identity) => {
            let key = PipeKey::new(identity.src, identity.src + identity.dir);
            if let Some(previous) = next_pipes.remove(&key) {
                diff.previous_pipes.push((key, previous));
            } else {
                debug_assert!(
                    !graph.has_pipe_between(key.a, key.b),
                    "pipe removal delta missed existing pipe"
                );
            }
        }
    }
    debug_assert_eq!(next_pipes.len(), graph.pipe_count());
    (previous_blocks, next_pipes, diff)
}

fn sync_incremental_adjacency_snapshot(
    cache: &mut RenderSignatureContextCache,
    next_blocks: &FxHashMap<IVec3, RenderSignatureBlockIdentity>,
    next_pipes: &FxHashMap<PipeKey, RenderSignaturePipeIdentity>,
    diff: &RenderSignatureIdentityDiff,
    graph: &BlockGraph,
) {
    if diff.has_block_changes() {
        cache.adjacency = GraphAdjacencySnapshot::from_render_identities(next_blocks, next_pipes);
        debug_assert_eq!(next_blocks.len(), graph.block_count());
    } else {
        update_pipe_only_adjacency_snapshot(&mut cache.adjacency, diff);
    }
}

fn update_pipe_only_adjacency_snapshot(
    snapshot: &mut GraphAdjacencySnapshot,
    diff: &RenderSignatureIdentityDiff,
) {
    for (_, identity) in &diff.previous_pipes {
        snapshot.remove_identity_pipe_neighbor(identity);
    }
    for (_, identity) in &diff.next_pipes {
        snapshot.insert_identity_pipe_neighbor(identity);
    }
}

fn collect_affected_render_signature_elements(
    snapshot: &GraphAdjacencySnapshot,
    diff: &RenderSignatureIdentityDiff,
    affected_blocks: &mut FxHashSet<IVec3>,
    affected_pipes: &mut FxHashSet<PipeKey>,
) {
    for (pos, identity) in &diff.previous_blocks {
        add_affected_block_signature_elements(
            *pos,
            identity,
            snapshot,
            affected_blocks,
            affected_pipes,
        );
    }
    for (pos, identity) in &diff.next_blocks {
        add_affected_block_signature_elements(
            *pos,
            identity,
            snapshot,
            affected_blocks,
            affected_pipes,
        );
    }
    for (key, _) in &diff.previous_pipes {
        affected_pipes.insert(*key);
        add_affected_pipe_signature_elements(*key, snapshot, affected_blocks, affected_pipes);
    }
    for (key, _) in &diff.next_pipes {
        affected_pipes.insert(*key);
        add_affected_pipe_signature_elements(*key, snapshot, affected_blocks, affected_pipes);
    }
}

fn update_affected_render_signatures(
    cache: &mut RenderSignatureContextCache,
    graph: &BlockGraph,
    pipe_length: f32,
    affected_blocks: FxHashSet<IVec3>,
    affected_pipes: FxHashSet<PipeKey>,
) {
    let (block_updates, pipe_updates) = {
        let context = RenderSignatureContext::from_cache(cache);
        let block_updates = affected_blocks
            .into_iter()
            .map(|pos| {
                let signature = graph
                    .get_block(pos)
                    .map(|block| block_render_geometry_signature(block, &context, pipe_length));
                (pos, signature)
            })
            .collect::<Vec<_>>();
        let pipe_updates = affected_pipes
            .into_iter()
            .map(|key| {
                let signature = graph.get_pipe(key.a, key.b).map(|pipe| {
                    pipe_render_geometry_signature(key.a, key.b, pipe, &context, pipe_length)
                });
                (key, signature)
            })
            .collect::<Vec<_>>();
        (block_updates, pipe_updates)
    };
    for (pos, signature) in block_updates {
        if let Some(signature) = signature {
            cache.block_signatures.insert(pos, signature);
        } else {
            cache.block_signatures.remove(&pos);
        }
    }
    for (key, signature) in pipe_updates {
        if let Some(signature) = signature {
            cache.pipe_signatures.insert(key, signature);
        } else {
            cache.pipe_signatures.remove(&key);
        }
    }
}

fn collect_render_signature_block_identities(
    graph: &BlockGraph,
) -> FxHashMap<IVec3, RenderSignatureBlockIdentity> {
    graph
        .blocks()
        .map(|block| {
            (
                block.pos(),
                RenderSignatureBlockIdentity {
                    kind: block.kind(),
                    height_cells: block.height_cells(),
                    port_color: block.port_color(),
                    connectable_offsets: compact_connectable_offsets(block),
                },
            )
        })
        .collect()
}

fn collect_render_signature_block_identities_with_diff(
    graph: &BlockGraph,
    previous_blocks: &FxHashMap<IVec3, RenderSignatureBlockIdentity>,
    diff: &mut RenderSignatureIdentityDiff,
) -> FxHashMap<IVec3, RenderSignatureBlockIdentity> {
    let mut next_blocks =
        FxHashMap::with_capacity_and_hasher(graph.block_count(), Default::default());
    graph
        .blocks()
        .map(|block| {
            let identity = RenderSignatureBlockIdentity {
                kind: block.kind(),
                height_cells: block.height_cells(),
                port_color: block.port_color(),
                connectable_offsets: compact_connectable_offsets(block),
            };
            (block.pos(), identity)
        })
        .for_each(|(pos, identity)| {
            if previous_blocks.get(&pos) != Some(&identity) {
                diff.next_blocks.push((pos, identity));
            }
            next_blocks.insert(pos, identity);
        });
    for (pos, identity) in previous_blocks {
        if next_blocks.get(pos) != Some(identity) {
            diff.previous_blocks.push((*pos, *identity));
        }
    }
    next_blocks
}

fn collect_render_signature_pipe_identities(
    graph: &BlockGraph,
) -> FxHashMap<PipeKey, RenderSignaturePipeIdentity> {
    graph
        .pipes()
        .map(|pipe| {
            let identity = render_signature_pipe_identity(pipe);
            (
                PipeKey::new(identity.src, identity.src + identity.dir),
                identity,
            )
        })
        .collect()
}

fn collect_render_signature_pipe_identities_with_diff(
    graph: &BlockGraph,
    previous_pipes: &FxHashMap<PipeKey, RenderSignaturePipeIdentity>,
    diff: &mut RenderSignatureIdentityDiff,
) -> FxHashMap<PipeKey, RenderSignaturePipeIdentity> {
    let mut next_pipes =
        FxHashMap::with_capacity_and_hasher(graph.pipe_count(), Default::default());
    graph.pipes().for_each(|pipe| {
        let identity = render_signature_pipe_identity(pipe);
        let key = PipeKey::new(identity.src, identity.src + identity.dir);
        if previous_pipes.get(&key) != Some(&identity) {
            diff.next_pipes.push((key, identity));
        }
        next_pipes.insert(key, identity);
    });
    for (key, identity) in previous_pipes {
        if next_pipes.get(key) != Some(identity) {
            diff.previous_pipes.push((*key, *identity));
        }
    }
    next_pipes
}

fn render_signature_pipe_identity(pipe: &Pipe) -> RenderSignaturePipeIdentity {
    let (src, dst) = pipe.endpoints();
    RenderSignaturePipeIdentity {
        src,
        dir: dst - src,
        hadamard: pipe.is_hadamard(),
    }
}

fn add_affected_block_signature_elements(
    pos: IVec3,
    identity: &RenderSignatureBlockIdentity,
    snapshot: &GraphAdjacencySnapshot,
    affected_blocks: &mut FxHashSet<IVec3>,
    affected_pipes: &mut FxHashSet<PipeKey>,
) {
    affected_blocks.insert(pos);
    for offset in identity.connectable_offsets.iter() {
        let endpoint = pos + offset;
        add_affected_endpoint_signature_elements(
            endpoint,
            snapshot,
            affected_blocks,
            affected_pipes,
        );
    }
}

fn add_affected_pipe_signature_elements(
    key: PipeKey,
    snapshot: &GraphAdjacencySnapshot,
    affected_blocks: &mut FxHashSet<IVec3>,
    affected_pipes: &mut FxHashSet<PipeKey>,
) {
    add_affected_endpoint_signature_elements(key.a, snapshot, affected_blocks, affected_pipes);
    add_affected_endpoint_signature_elements(key.b, snapshot, affected_blocks, affected_pipes);
}

fn add_affected_endpoint_signature_elements(
    endpoint: IVec3,
    snapshot: &GraphAdjacencySnapshot,
    affected_blocks: &mut FxHashSet<IVec3>,
    affected_pipes: &mut FxHashSet<PipeKey>,
) {
    if let Some(block) = snapshot.endpoint_block(endpoint) {
        affected_blocks.insert(block.pos);
    }
    for direction in ADJACENT_DIRECTIONS {
        let Some(neighbor) = endpoint.checked_add(direction) else {
            continue;
        };
        if snapshot.has_pipe_between(endpoint, neighbor) {
            affected_pipes.insert(PipeKey::new(endpoint, neighbor));
        }
        if let Some(block) = snapshot.endpoint_block(neighbor) {
            affected_blocks.insert(block.pos);
        }
    }
}

pub(super) fn block_render_geometry_signature(
    block: &Block,
    context: &RenderSignatureContext,
    pipe_length: f32,
) -> u64 {
    // SipHash (not FxHasher): FxHasher's output width follows the pointer width,
    // so on wasm32 `finish()` truncates to 32 bits. That collapses these
    // geometry version tags into a 2^32 space on the web build only, where a
    // collision between an element's old and new geometry silently skips its
    // mesh rebuild (stale/wrong mesh). DefaultHasher is a stable 64 bits on
    // every target, matching combine_signature here.
    let mut hasher = DefaultHasher::new();
    "block_geometry".hash(&mut hasher);
    block.pos().hash(&mut hasher);
    block.kind().hash(&mut hasher);
    block.height_cells().hash(&mut hasher);
    block.port_color().hash(&mut hasher);
    pipe_length.to_bits().hash(&mut hasher);
    for offset in block.connectable_offsets() {
        let endpoint = block.pos() + offset;
        endpoint.hash(&mut hasher);
        for direction in ADJACENT_DIRECTIONS {
            direction.hash(&mut hasher);
            endpoint
                .checked_add(direction)
                .is_some_and(|neighbor| context.has_pipe_between(endpoint, neighbor))
                .hash(&mut hasher);
        }
    }
    hasher.finish()
}

pub(super) fn pipe_render_geometry_signature(
    u_pos: IVec3,
    v_pos: IVec3,
    pipe: &Pipe,
    context: &RenderSignatureContext,
    pipe_length: f32,
) -> u64 {
    // SipHash for the same pointer-width portability reason as
    // block_render_geometry_signature above.
    let mut hasher = DefaultHasher::new();
    "pipe_geometry".hash(&mut hasher);
    PipeKey::new(u_pos, v_pos).hash(&mut hasher);
    pipe.src().hash(&mut hasher);
    pipe.dir().hash(&mut hasher);
    pipe.is_hadamard().hash(&mut hasher);
    pipe_length.to_bits().hash(&mut hasher);
    let key = PipeKey::new(u_pos, v_pos);
    for endpoint in [key.a, key.b] {
        endpoint.hash(&mut hasher);
        if let Some(block) = context.endpoint_block(endpoint) {
            block.pos.hash(&mut hasher);
            block.kind.hash(&mut hasher);
            block.height_cells.hash(&mut hasher);
        } else {
            0xffu8.hash(&mut hasher);
        }
        for direction in ADJACENT_DIRECTIONS {
            direction.hash(&mut hasher);
            endpoint
                .checked_add(direction)
                .is_some_and(|neighbor| context.has_pipe_between(endpoint, neighbor))
                .hash(&mut hasher);
        }
    }
    hasher.finish()
}

pub(super) fn combine_signature(base: u64, alpha_bits: u32, label: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    label.hash(&mut hasher);
    base.hash(&mut hasher);
    alpha_bits.hash(&mut hasher);
    hasher.finish()
}

/// Inputs the per-element render signatures depend on; a change triggers a
/// full signature recomputation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderSignatureContextSyncState {
    pub(crate) tab_id: EditorTabId,
    pub(crate) graph_revision: u64,
    pub(crate) pipe_length_bits: u32,
}

/// The block attributes that feed into its render signature and adjacency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderSignatureBlockIdentity {
    pub(crate) kind: BlockKind,
    pub(crate) height_cells: u32,
    pub(crate) port_color: Option<RGBA>,
    pub(crate) connectable_offsets: RenderSignatureConnectableOffsets,
}

/// A block's connectable endpoint offsets, stored inline (`len` of the two-slot
/// array is used) to avoid an allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderSignatureConnectableOffsets {
    pub(crate) offsets: [IVec3; 2],
    pub(crate) len: u8,
}

impl RenderSignatureConnectableOffsets {
    pub(crate) fn iter(self) -> impl Iterator<Item = IVec3> {
        self.offsets.into_iter().take(usize::from(self.len))
    }
}

/// The pipe attributes that feed into its render signature and adjacency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderSignaturePipeIdentity {
    pub(crate) src: IVec3,
    pub(crate) dir: IVec3,
    pub(crate) hadamard: bool,
}

/// Caches per-element render signatures and identities plus the adjacency they
/// were derived from, letting the visual system detect exactly which elements
/// changed shape between frames.
#[derive(Default)]
pub(crate) struct RenderSignatureContextCache {
    pub(super) applied: Option<RenderSignatureContextSyncState>,
    pub(super) adjacency: GraphAdjacencySnapshot,
    pub(super) block_signatures: FxHashMap<IVec3, u64>,
    pub(super) pipe_signatures: FxHashMap<PipeKey, u64>,
    pub(super) block_identities: FxHashMap<IVec3, RenderSignatureBlockIdentity>,
    pub(super) pipe_identities: FxHashMap<PipeKey, RenderSignaturePipeIdentity>,
}
