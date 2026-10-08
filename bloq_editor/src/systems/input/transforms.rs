//! Whole-graph and selection translate/rotate transforms.
use super::*;
use bloq_graph::{Action, Expr, MeasureTarget};
use std::collections::HashMap;

const INSERTION_GAP: i32 = 2;

/// The outcome of transforming a selection: the new graph, the transformed
/// elements to reselect, and how many actions were dropped by the transform.
#[derive(Debug)]
pub(crate) struct SelectionTransformResult {
    pub(crate) graph: BlockGraph,
    pub(crate) selected_elements: Vec<GraphElement>,
    pub(crate) dropped_action_count: usize,
}

/// Copies the blocks selected in `graph`, including every pipe whose endpoint
/// blocks are both selected. Pipes crossing the selection boundary are omitted.
pub(crate) fn copy_selected_subgraph(
    graph: &BlockGraph,
    selected_elements: &HashSet<GraphElement>,
) -> eyre::Result<BlockGraph> {
    let selected_blocks = selected_block_positions(selected_elements);
    if selected_blocks.is_empty() {
        bail!("Select at least one block to copy");
    }

    let mut subgraph = BlockGraph::new();
    for block in graph
        .blocks()
        .filter(|block| selected_blocks.contains(&block.pos()))
    {
        subgraph
            .try_add_block(block.clone())
            .wrap_err("copy selected block")?;
    }
    for (_, _, u, v, pipe) in graph.pipe_endpoints_with_blocks() {
        if selected_blocks.contains(&u.pos()) && selected_blocks.contains(&v.pos()) {
            subgraph
                .try_add_pipe(pipe.clone())
                .wrap_err("copy internal selected pipe")?;
        }
    }
    Ok(subgraph)
}

/// Places `inserted` beside `graph` and returns the combined graph and insertion.
pub(crate) fn insert_graph_without_overlap(
    graph: &BlockGraph,
    inserted: &BlockGraph,
) -> eyre::Result<(BlockGraph, Vec<GraphElement>)> {
    if inserted.is_empty() {
        bail!("Cannot insert an empty graph");
    }
    if graph.is_empty() {
        return Ok((inserted.clone(), GraphElement::all_in(inserted).collect()));
    }

    let shifted = shift_beside(graph, inserted)?;
    let selected_elements = GraphElement::all_in(&shifted).collect();
    let mut next = graph.clone();

    let shown_branch_elements = shifted
        .branch_definitions()
        .iter()
        .flat_map(|region| {
            let arm = region.shown_arm();
            arm.blocks()
                .map(|block| GraphElement::Block(block.pos()))
                .chain(arm.pipes().map(|pipe| {
                    let (u, v) = pipe.endpoints();
                    GraphElement::Pipe(u, v).canonical()
                }))
        })
        .collect::<HashSet<_>>();

    for block in shifted
        .blocks()
        .filter(|block| !shown_branch_elements.contains(&GraphElement::Block(block.pos())))
    {
        next.try_add_block(block.clone())
            .wrap_err("insert block without overlap")?;
    }
    let shared_pipes = shifted
        .pipes()
        .filter(|pipe| {
            let (u, v) = pipe.endpoints();
            !shown_branch_elements.contains(&GraphElement::Pipe(u, v).canonical())
        })
        .cloned()
        .collect();

    let mut branch_names = next
        .branch_definitions()
        .iter()
        .map(|region| region.name.clone())
        .collect::<HashSet<_>>();
    let mut hidden_true = Vec::new();
    let regions = shifted
        .branch_definitions()
        .iter()
        .map(|region| {
            let name = unique_name(&region.name, &mut branch_names);
            if !region.shown_true() {
                hidden_true.push(name.clone());
            }
            (name, region.on_false().clone(), region.on_true().clone())
        })
        .collect();
    next.try_add_branch_regions(regions, shared_pipes)
        .wrap_err("insert branch regions and common pipes")?;
    for name in hidden_true {
        next.set_shown_branch_arm(&name, false)
            .wrap_err("restore shown branch arm")?;
    }

    let mut action_names = next
        .actions()
        .iter()
        .filter_map(action_definition_name)
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let inserted_actions = rename_action_definitions(shifted.actions(), &mut action_names);
    let mut actions = next.actions();
    actions.extend(inserted_actions);
    next.set_actions_lenient(actions)
        .wrap_err("combine inserted graph actions")?;

    Ok((next, selected_elements))
}

/// Translates the whole graph by `step` cells along `axis`.
///
/// # Errors
///
/// Returns an error if the step is zero or coordinates overflow.
pub(crate) fn translate_graph(
    graph: &BlockGraph,
    axis: UDirection,
    step: i32,
) -> eyre::Result<BlockGraph> {
    let delta = axis.to_ivec3() * step;
    if delta == IVec3::ZERO {
        bail!("Translation step is zero");
    }

    graph
        .shift_positions(delta)
        .wrap_err("translate graph coordinates")
}

/// Translates just the selected blocks (and their internal pipes) by `step`
/// cells along `axis`.
///
/// # Errors
///
/// Returns an error for an empty/zero move, a boundary pipe, or an overlap.
pub(crate) fn translate_selected_elements(
    graph: &BlockGraph,
    selected_elements: &HashSet<GraphElement>,
    axis: UDirection,
    step: i32,
) -> eyre::Result<SelectionTransformResult> {
    translate_selected_elements_by(graph, selected_elements, axis.to_ivec3() * step)
}

/// Mouse translation uses the same boundary, overlap, and action checks as keyboard moves.
pub(crate) fn translate_selected_elements_by(
    graph: &BlockGraph,
    selected_elements: &HashSet<GraphElement>,
    offset: IVec3,
) -> eyre::Result<SelectionTransformResult> {
    if offset == IVec3::ZERO {
        bail!("Translation step is zero");
    }
    let selected_blocks = selected_block_positions(selected_elements);
    if !graph.is_empty()
        && graph
            .blocks()
            .all(|block| selected_blocks.contains(&block.pos()))
    {
        let graph = graph.shift_positions(offset)?;
        return Ok(SelectionTransformResult {
            selected_elements: GraphElement::all_in(&graph).collect(),
            graph,
            dropped_action_count: 0,
        });
    }
    let mut actions = graph.actions();
    for action in &mut actions {
        translate_action_targets(action, &selected_blocks, offset)?;
    }
    transform_selected_subgraph(graph, selected_elements, actions, |subgraph| {
        subgraph
            .shift_positions(offset)
            .wrap_err("translate selected coordinates")
    })
}

/// Rotates the selected blocks by `quarter_turns` about `axis`, pivoting on the
/// selection's own center.
///
/// # Errors
///
/// Returns an error for an empty selection, boundary pipe, unsupported rotation,
/// or overlap.
pub(crate) fn rotate_selected_elements(
    graph: &BlockGraph,
    selected_elements: &HashSet<GraphElement>,
    axis: UDirection,
    quarter_turns: i32,
) -> eyre::Result<SelectionTransformResult> {
    transform_selected_subgraph(graph, selected_elements, Vec::new(), |subgraph| {
        let pivot = selection_rotation_pivot(&subgraph)?;
        let inverse_pivot = IVec3::new(
            pivot
                .x
                .checked_neg()
                .wrap_err("rotation pivot x is i32::MIN")?,
            pivot
                .y
                .checked_neg()
                .wrap_err("rotation pivot y is i32::MIN")?,
            pivot
                .z
                .checked_neg()
                .wrap_err("rotation pivot z is i32::MIN")?,
        );
        let rotated = subgraph
            .shift_positions(inverse_pivot)
            .wrap_err("translate selection to its rotation pivot")?
            .rotate_about_origin_lenient(axis, quarter_turns)
            .wrap_err("rotate selection about its pivot")?
            .shift_positions(pivot)
            .wrap_err("translate rotated selection from its pivot")?;
        Ok(rotated)
    })
}

fn transform_selected_subgraph(
    graph: &BlockGraph,
    selected_elements: &HashSet<GraphElement>,
    actions: Vec<Action>,
    transform: impl FnOnce(BlockGraph) -> eyre::Result<BlockGraph>,
) -> eyre::Result<SelectionTransformResult> {
    let dropped_action_count = graph.actions().len() - actions.len();
    let selected_blocks = selected_block_positions(selected_elements);

    if selected_blocks.is_empty() {
        bail!("Select at least one block to transform");
    }
    if selected_blocks
        .iter()
        .any(|position| graph.shown_branch_at(*position).is_some())
    {
        bail!("Branch arms cannot be transformed separately; translate the whole graph instead");
    }

    for element in selected_elements {
        if let GraphElement::Pipe(u, v) = element.canonical()
            && !pipe_endpoint_owners_selected(graph, u, v, &selected_blocks)
        {
            bail!("Pipe-only transforms need both endpoint blocks selected");
        }
    }

    for (_, _, u, v, _) in graph.pipe_endpoints_with_blocks() {
        let u_selected = selected_blocks.contains(&u.pos());
        let v_selected = selected_blocks.contains(&v.pos());
        if u_selected != v_selected {
            bail!(
                "Selection has pipes crossing its boundary; select the connected blocks together"
            );
        }
    }

    let subgraph = copy_selected_subgraph(graph, selected_elements)
        .wrap_err("copy selection into transform subgraph")?;
    let transformed_subgraph = transform(subgraph)?;
    let mut next_graph = graph.clone();
    for pos in &selected_blocks {
        next_graph.remove_block(*pos);
    }
    next_graph.clear_actions();

    for block in transformed_subgraph.blocks() {
        next_graph
            .try_add_block(block.clone())
            .wrap_err("Transform would overlap an existing block")?;
    }
    for (_, _, _, _, pipe) in transformed_subgraph.pipe_endpoints_with_blocks() {
        next_graph
            .try_add_pipe(pipe.clone())
            .wrap_err("Transform would create an invalid pipe")?;
    }
    next_graph
        .set_actions_lenient(actions)
        .wrap_err("Transform produced invalid actions")?;

    let selected_after = transformed_subgraph
        .blocks()
        .map(|block| GraphElement::Block(block.pos()))
        .chain(
            transformed_subgraph
                .pipe_endpoints_with_blocks()
                .map(|(u, v, _, _, _)| GraphElement::Pipe(u, v).canonical()),
        )
        .collect::<Vec<_>>();

    Ok(SelectionTransformResult {
        graph: next_graph,
        selected_elements: selected_after,
        dropped_action_count,
    })
}

fn translate_action_targets(
    action: &mut Action,
    selected_blocks: &HashSet<IVec3>,
    offset: IVec3,
) -> eyre::Result<()> {
    // Branch regions move only with the whole graph.
    let shift_if_selected = |target: &mut IVec3| -> eyre::Result<()> {
        if selected_blocks.contains(target) {
            *target = bloq_graph::checked_add_position(*target, offset)?;
        }
        Ok(())
    };
    match action {
        Action::Measure {
            target: MeasureTarget::Node(target),
            ..
        }
        | Action::Measure {
            target: MeasureTarget::Edge { src: target, .. },
            ..
        }
        | Action::Resolve { target, .. } => shift_if_selected(target)?,
        Action::Feedback { targets, .. } => {
            for target in targets {
                shift_if_selected(&mut target.target)?;
            }
        }
        Action::Let { .. } | Action::DiscardIf(_) | Action::Branch { .. } => {}
    }
    Ok(())
}

fn selected_block_positions(selected_elements: &HashSet<GraphElement>) -> HashSet<IVec3> {
    selected_elements
        .iter()
        .filter_map(|element| match element.canonical() {
            GraphElement::Block(pos) => Some(pos),
            GraphElement::Pipe(_, _) => None,
        })
        .collect()
}

fn shift_beside(graph: &BlockGraph, inserted: &BlockGraph) -> eyre::Result<BlockGraph> {
    let (graph_x, _, _) = graph.spans().wrap_err("current graph has no blocks")?;
    let (inserted_x, _, _) = inserted.spans().wrap_err("inserted graph has no blocks")?;
    let x = (*graph_x.end())
        .checked_sub(*inserted_x.start())
        .and_then(|offset| offset.checked_add(INSERTION_GAP))
        .wrap_err("no coordinate range remains to the right of the current graph")?;
    inserted
        .shift_positions(IVec3::new(x, 0, 0))
        .wrap_err("place inserted graph beside current graph")
}

fn unique_name(name: &str, used: &mut HashSet<String>) -> String {
    if used.insert(name.to_owned()) {
        return name.to_owned();
    }
    for index in 1usize.. {
        let candidate = if index == 1 {
            format!("{name}_copy")
        } else {
            format!("{name}_copy{index}")
        };
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("usize name suffixes are inexhaustible")
}

fn action_definition_name(action: &Action) -> Option<&str> {
    match action {
        Action::Let { name, .. } | Action::Measure { name, .. } => Some(name),
        _ => None,
    }
}

fn rename_action_definitions(mut actions: Vec<Action>, used: &mut HashSet<String>) -> Vec<Action> {
    let mut names = HashMap::new();
    for name in actions.iter().filter_map(action_definition_name) {
        names
            .entry(name.to_owned())
            .or_insert_with(|| unique_name(name, used));
    }
    for action in &mut actions {
        rename_action(action, &names);
    }
    actions
}

fn rename_action(action: &mut Action, names: &HashMap<String, String>) {
    match action {
        Action::Let { name, expr } => {
            rename_name(name, names);
            rename_expr(expr, names);
        }
        Action::Measure { name, .. } => rename_name(name, names),
        Action::DiscardIf(expr)
        | Action::Resolve {
            condition: expr, ..
        }
        | Action::Branch {
            condition: expr, ..
        } => rename_expr(expr, names),
        Action::Feedback { condition, .. } => {
            if let Some(expr) = condition {
                rename_expr(expr, names);
            }
        }
    }
}

fn rename_expr(expr: &mut Expr, names: &HashMap<String, String>) {
    match expr {
        Expr::Var(name) => rename_name(name, names),
        Expr::Not(expr) => rename_expr(expr, names),
        Expr::Binary(_, lhs, rhs) => {
            rename_expr(lhs, names);
            rename_expr(rhs, names);
        }
    }
}

fn rename_name(name: &mut String, names: &HashMap<String, String>) {
    if let Some(replacement) = names.get(name) {
        name.clone_from(replacement);
    }
}

fn pipe_endpoint_owners_selected(
    graph: &BlockGraph,
    u: IVec3,
    v: IVec3,
    selected_blocks: &HashSet<IVec3>,
) -> bool {
    let Some(u_block) = graph.get_endpoint_block(u) else {
        return false;
    };
    let Some(v_block) = graph.get_endpoint_block(v) else {
        return false;
    };
    selected_blocks.contains(&u_block.pos()) && selected_blocks.contains(&v_block.pos())
}

fn selection_rotation_pivot(graph: &BlockGraph) -> eyre::Result<IVec3> {
    let mut blocks = graph.blocks();
    let Some(first) = blocks.next() else {
        bail!("Select at least one block to rotate");
    };
    let mut min = first.pos();
    let mut max = first.pos();
    let mut count = 1usize;
    for block in blocks {
        let pos = block.pos();
        min = min.min(pos);
        max = max.max(pos);
        count += 1;
    }
    if count == 1 {
        return Ok(min);
    }

    fn rounded_midpoint(min: i32, max: i32) -> i32 {
        let sum = i64::from(min) + i64::from(max);
        let rounded = if sum >= 0 {
            (sum + 1) / 2
        } else {
            (sum - 1) / 2
        };
        i32::try_from(rounded).expect("midpoint of two i32 coordinates fits in i32")
    }

    Ok(IVec3::new(
        rounded_midpoint(min.x, max.x),
        rounded_midpoint(min.y, max.y),
        rounded_midpoint(min.z, max.z),
    ))
}

/// Formats a signed degree label (e.g. `+90deg`) for a quarter-turn count.
pub(crate) fn rotation_degrees_label(quarter_turns: i32) -> String {
    format!("{:+}deg", quarter_turns * 90)
}
