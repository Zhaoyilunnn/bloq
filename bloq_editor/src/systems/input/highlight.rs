//! Hover/selection highlight overlay syncing.
use super::*;

pub(crate) fn sync_interaction_highlight_system(
    editor_state: Res<EditorState>,
    circuit_viewer: Res<BloqViewerState>,
    graph_state: Res<GraphState>,
    render_state: Res<GraphRenderState>,
    mut previous: Local<InteractionHighlightCache>,
    children_query: Query<&Children>,
    mut material_query: Query<(&mut MeshMaterial3d<StandardMaterial>, &OriginalMaterial)>,
) {
    let _span = info_span!("editor.sync_interaction_highlight_system").entered();
    let previous = &mut *previous;
    previous.current_hovered.clear();
    previous
        .current_hovered
        .extend(editor_state.blog_hovered_element_set().iter().copied());
    previous
        .current_hovered
        .extend(circuit_viewer.hovered_source_elements_iter());
    previous
        .current_hovered
        .extend(editor_state.zx_hovered_element_set().iter().copied());
    previous
        .current_hovered
        .extend(editor_state.action_hovered_elements.iter().copied());
    if let Some(element) = editor_state.hovered_element_for_highlight() {
        previous.current_hovered.insert(element);
    }
    previous.current_selected.clear();
    previous
        .current_selected
        .extend(editor_state.selected_element_set().iter().copied());
    if editor_state.is_pipe_tool_active()
        && let Some(start) = editor_state.pipe_start
        && let Some(block) = graph_state.graph.get_endpoint_block(start)
    {
        previous
            .current_selected
            .insert(GraphElement::Block(block.pos()));
    }
    previous.current_candidates.clear();
    previous
        .current_candidates
        .extend(editor_state.action_candidate_elements.iter().copied());
    let graph_revision = graph_state.revision;
    if !previous.needs_refresh(graph_revision) {
        return;
    }

    let mut changed = std::mem::take(&mut previous.changed);
    changed.clear();
    collect_changed_interaction_elements(
        &previous.hovered,
        &previous.selected,
        &previous.current_hovered,
        &previous.current_selected,
        &mut changed,
    );
    changed.extend(
        previous
            .candidates
            .symmetric_difference(&previous.current_candidates)
            .copied(),
    );
    if previous.graph_revision != graph_revision {
        changed.extend(previous.current_hovered.iter().copied());
        changed.extend(previous.current_selected.iter().copied());
        changed.extend(previous.current_candidates.iter().copied());
    }

    for element in &changed {
        let current_is_hovered = previous.current_hovered.contains(element);
        let current_is_selected = previous.current_selected.contains(element);
        // Candidates sit below the two established tiers: an armed pick still
        // wants to show what is hovered and what was already selected.
        let material = if current_is_selected {
            Some(editor_state.selection_material.clone())
        } else if current_is_hovered {
            Some(editor_state.highlight_material.clone())
        } else if previous.current_candidates.contains(element) {
            Some(editor_state.action_candidate_material.clone())
        } else {
            None
        };
        if let Some(rendered) = render_state.rendered_for(*element) {
            update_rendered_highlight(rendered, material, &children_query, &mut material_query);
        }
    }
    previous.changed = changed;

    std::mem::swap(&mut previous.hovered, &mut previous.current_hovered);
    std::mem::swap(&mut previous.selected, &mut previous.current_selected);
    std::mem::swap(&mut previous.candidates, &mut previous.current_candidates);
    previous.graph_revision = graph_revision;
}

pub(super) fn collect_changed_interaction_elements(
    previous_hovered: &HashSet<GraphElement>,
    previous_selected: &HashSet<GraphElement>,
    current_hovered: &HashSet<GraphElement>,
    current_selected: &HashSet<GraphElement>,
    changed: &mut HashSet<GraphElement>,
) {
    changed.extend(
        previous_hovered
            .symmetric_difference(current_hovered)
            .copied(),
    );
    changed.extend(
        previous_selected
            .symmetric_difference(current_selected)
            .copied(),
    );
}

pub(crate) fn pipe_mode_selected_elements(
    editor_state: &EditorState,
    graph: &BlockGraph,
) -> HashSet<GraphElement> {
    let mut selected = editor_state.selected_element_set().clone();
    if editor_state.is_pipe_tool_active()
        && let Some(start) = editor_state.pipe_start
        && let Some(block) = graph.get_endpoint_block(start)
    {
        selected.insert(GraphElement::Block(block.pos()));
    }
    selected
}

#[derive(Default)]
pub(crate) struct InteractionHighlightCache {
    hovered: HashSet<GraphElement>,
    selected: HashSet<GraphElement>,
    candidates: HashSet<GraphElement>,
    current_hovered: HashSet<GraphElement>,
    current_selected: HashSet<GraphElement>,
    current_candidates: HashSet<GraphElement>,
    changed: HashSet<GraphElement>,
    graph_revision: u64,
}

impl InteractionHighlightCache {
    /// Whether anything the highlight depends on moved since the last sync.
    fn needs_refresh(&self, graph_revision: u64) -> bool {
        self.graph_revision != graph_revision
            || self.hovered != self.current_hovered
            || self.selected != self.current_selected
            || self.candidates != self.current_candidates
    }
}

/// Retints every mesh under `target`, or restores each to its original material
/// when `material` is `None`.
fn update_highlight(
    target: Entity,
    material: Option<Handle<StandardMaterial>>,
    children_query: &Query<&Children>,
    material_query: &mut Query<(&mut MeshMaterial3d<StandardMaterial>, &OriginalMaterial)>,
) {
    for child in children_query.iter_descendants(target) {
        if let Ok((mut mat, original)) = material_query.get_mut(child) {
            mat.0 = material.clone().unwrap_or_else(|| original.0.clone());
        }
    }
}

/// Retints a rendered element, preferring its tracked mesh parts and falling
/// back to a descendant walk.
fn update_rendered_highlight(
    rendered: &RenderedElement,
    material: Option<Handle<StandardMaterial>>,
    children_query: &Query<&Children>,
    material_query: &mut Query<(&mut MeshMaterial3d<StandardMaterial>, &OriginalMaterial)>,
) {
    if update_highlight_parts(&rendered.mesh_parts, material.clone(), material_query) {
        return;
    }
    update_highlight(rendered.entity, material, children_query, material_query);
}

fn update_highlight_parts(
    mesh_parts: &RenderedMeshParts,
    material: Option<Handle<StandardMaterial>>,
    material_query: &mut Query<(&mut MeshMaterial3d<StandardMaterial>, &OriginalMaterial)>,
) -> bool {
    if mesh_parts.triangles.is_empty() && mesh_parts.lines.is_empty() {
        return false;
    }
    let mut updated = false;
    for child in mesh_parts.triangles.iter().chain(&mesh_parts.lines) {
        if let Ok((mut mat, original)) = material_query.get_mut(*child) {
            mat.0 = material.clone().unwrap_or_else(|| original.0.clone());
            updated = true;
        }
    }
    updated
}

/// True when the layer filter hides `element` in the current View layer.
pub(super) fn element_hidden_by_layer_filter(
    element: GraphElement,
    graph: &BlockGraph,
    editor_state: &EditorState,
) -> bool {
    editor_state.mode == EditorMode::View
        && editor_state.view_current_layer_only
        && match element {
            GraphElement::Block(pos) => graph
                .get_block(pos)
                .is_none_or(|block| !block.occupies_layer(editor_state.plane_height)),
            GraphElement::Pipe(u, v) => !is_pipe_visible(u, v, editor_state.plane_height),
        }
}
