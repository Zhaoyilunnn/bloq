//! Shared layered placement and obstacle-aware edge routing for dependency graphs.

use std::collections::HashMap;

use layout::{
    core::{
        base::Orientation,
        format::{ClipHandle, RenderBackend},
        geometry::{Point, Position},
        style::StyleAttr,
    },
    std_shapes::shapes::{Arrow, Element, ShapeKind},
    topo::layout::VisualGraph,
};

/// Cubic Bézier segments, each containing start, two controls, and end.
pub type CubicSpline = Vec<[(f32, f32); 4]>;

/// Cached geometry at zoom 1.0. Renderers apply pan and zoom without relayout.
#[derive(Debug)]
pub struct RelativeLayout {
    /// Normalized top-left coordinates, keyed by node id.
    pub positions: HashMap<u32, (f32, f32)>,
    /// Routed splines in the original input edge order, including parallel edges.
    pub routes: Vec<CubicSpline>,
    /// Bounding box covering nodes and routed edges.
    pub content: (f32, f32),
}

/// Lay out sized nodes and route their edges with `layout-rs`.
///
/// Regions are opaque rectangles; callers size their contents bottom-up.
/// Node ids must be unique, sizes and gap finite, and all endpoints present.
/// Compute once per graph edit, rather than per frame.
///
/// # Panics
///
/// Panics if an edge refers to an absent node.
pub fn compute_relative(
    nodes: &[(u32, (f32, f32))],
    edges: &[(u32, u32)],
    gap: f32,
) -> RelativeLayout {
    if nodes.is_empty() {
        return RelativeLayout {
            positions: HashMap::new(),
            routes: Vec::new(),
            content: (0.0, 0.0),
        };
    }

    let orientation = Orientation::LeftToRight;
    let mut graph = VisualGraph::new(orientation);
    let handles = nodes
        .iter()
        .map(|&(id, (width, height))| {
            let size = Point::new(f64::from(width), f64::from(height));
            let mut element = Element::create(
                ShapeKind::new_box(""),
                StyleAttr::simple(),
                orientation,
                size,
            );
            element.pos = Position::new(
                Point::zero(),
                size,
                Point::zero(),
                Point::splat(f64::from(gap.max(1.0))),
            );
            (id, graph.add_node(element))
        })
        .collect::<HashMap<_, _>>();
    for (index, &(from, to)) in edges.iter().enumerate() {
        graph.add_edge(
            Arrow::simple_with_properties("", index.to_string()),
            handles[&from],
            handles[&to],
        );
    }

    let mut collector = RouteCollector {
        routes: vec![Vec::new(); edges.len()],
    };
    graph.do_it(false, false, false, &mut collector);
    let columns = graph
        .dag
        .ranks()
        .iter()
        .map(|rank| {
            rank.iter().map(|&node| graph.pos(node)).fold(
                (f32::INFINITY, f32::NEG_INFINITY),
                |(left, right), pos| {
                    (
                        left.min(pos.left(false) as f32),
                        right.max(pos.right(false) as f32),
                    )
                },
            )
        })
        .collect::<Vec<_>>();
    for (&(from, to), route) in edges.iter().zip(&mut collector.routes) {
        let from = graph.dag.level(handles[&from]);
        let to = graph.dag.level(handles[&to]);
        if from.abs_diff(to) == route.len() && from != to {
            // Follow the library's dummy lanes through the free column gutters.
            // Direct smooth interpolation can clip a tall intervening region.
            let mut routed = Vec::new();
            for (step, &[start, _, _, end]) in route.iter().enumerate() {
                let column = if from < to {
                    from + step
                } else {
                    from - step - 1
                };
                let gutter = columns[column].1.midpoint(columns[column + 1].0);
                let (entry, exit) = if from < to {
                    ((columns[column].1, start.1), (columns[column + 1].0, end.1))
                } else {
                    ((columns[column + 1].0, start.1), (columns[column].1, end.1))
                };
                let straight = |a: (f32, f32), b: (f32, f32)| {
                    let control = |t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                    [a, control(1.0 / 3.0), control(2.0 / 3.0), b]
                };
                routed.extend(
                    [
                        straight(start, entry),
                        [entry, (gutter, start.1), (gutter, end.1), exit],
                        straight(exit, end),
                    ]
                    .into_iter()
                    .filter(|segment| segment[0] != segment[3]),
                );
            }
            *route = routed;
        }
    }
    let mut positions = nodes
        .iter()
        .map(|&(id, _)| {
            let pos = graph.pos(handles[&id]);
            (id, (pos.left(false) as f32, pos.top(false) as f32))
        })
        .collect::<HashMap<_, _>>();
    let points = nodes
        .iter()
        .flat_map(|&(id, (width, height))| {
            let (x, y) = positions[&id];
            [(x, y), (x + width, y + height)]
        })
        .chain(collector.routes.iter().flatten().flatten().copied());
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for (x, y) in points {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    for (x, y) in positions
        .values_mut()
        .chain(collector.routes.iter_mut().flatten().flatten())
    {
        *x -= min_x;
        *y -= min_y;
    }
    RelativeLayout {
        positions,
        routes: collector.routes,
        content: (max_x - min_x, max_y - min_y),
    }
}

/// Geometry adapter: the library owns placement/routing, callers own rendering.
struct RouteCollector {
    routes: Vec<CubicSpline>,
}

impl RenderBackend for RouteCollector {
    fn draw_arrow(
        &mut self,
        path: &[(Point, Point)],
        _: bool,
        head: (bool, bool),
        _: &StyleAttr,
        properties: Option<String>,
        _: &str,
    ) {
        let index = properties
            .expect("edge identity")
            .parse::<usize>()
            .expect("edge index");
        let mut start = path[0].0;
        let mut control = path[0].1;
        for &(incoming, end) in &path[1..] {
            self.routes[index]
                .push([start, control, incoming, end].map(|p| (p.x as f32, p.y as f32)));
            start = end;
            control = end.add(end.sub(incoming));
        }
        if head.0 {
            self.routes[index].reverse();
            for segment in &mut self.routes[index] {
                segment.reverse();
            }
        }
    }

    fn draw_rect(
        &mut self,
        _: Point,
        _: Point,
        _: &StyleAttr,
        _: Option<String>,
        _: Option<ClipHandle>,
    ) {
    }
    fn draw_line(&mut self, _: Point, _: Point, _: &StyleAttr, _: Option<String>) {}
    fn draw_circle(&mut self, _: Point, _: Point, _: &StyleAttr, _: Option<String>) {}
    fn draw_text(&mut self, _: Point, _: &str, _: &StyleAttr) {}
    fn create_clip(&mut self, _: Point, _: Point, _: usize) -> ClipHandle {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Dependency order advances left to right, including alongside isolated nodes.
    #[test]
    fn chain_places_left_to_right_and_keeps_isolated_nodes() {
        let size = (130.0, 46.0);
        let nodes = [(0, size), (1, size), (2, size), (3, size)];
        let edges = [(0, 1), (1, 2)];

        let layout = compute_relative(&nodes, &edges, 28.0);

        assert_eq!(
            layout.positions.len(),
            4,
            "every node, including the isolated one, places"
        );
        let col = |id: u32| layout.positions[&id].0;
        assert!(col(0) < col(1), "successor sits in a later column");
        assert!(col(1) < col(2), "chain keeps advancing rightwards");
    }

    // An oversized (region container) tile mixed with default tiles must not
    // overlap its successor: the content box has to fit both full sizes.
    #[test]
    fn mixed_sizes_keep_tiles_disjoint_and_content_covers_them() {
        let big = (400.0, 300.0);
        let small = (130.0, 46.0);
        let nodes = [(0, big), (1, small)];
        let edges = [(0, 1)];

        let layout = compute_relative(&nodes, &edges, 28.0);

        let (bx, _) = layout.positions[&0];
        let (sx, _) = layout.positions[&1];
        assert!(
            bx + big.0 <= sx + 0.01,
            "successor starts after the big tile ends: {bx} + {} vs {sx}",
            big.0
        );
        assert!(layout.content.0 >= sx + small.0 - 0.01);
        assert!(layout.content.1 >= big.1 - 0.01);
    }

    #[test]
    fn long_edges_route_around_intervening_nodes() {
        let nodes = [(0, (180.0, 60.0)), (1, (400.0, 250.0)), (2, (180.0, 60.0))];
        let edges = [(0, 1), (1, 2), (0, 2)];
        let layout = compute_relative(&nodes, &edges, 40.0);
        let (x, y) = layout.positions[&1];
        assert!(
            layout.routes[2].len() > 1,
            "long edge retains its routing waypoints"
        );
        assert!(
            layout.routes[2].iter().any(|[start, first, second, end]| {
                start.1 != end.1 && first.1 == start.1 && second.1 == end.1
            }),
            "route uses smooth curves with horizontal endpoint tangents"
        );
        for segment in &layout.routes[2] {
            for step in 0..=100 {
                let t = step as f32 / 100.0;
                let u = 1.0 - t;
                let weights = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
                let (px, py) = segment
                    .iter()
                    .zip(weights)
                    .fold((0.0, 0.0), |(px, py), (&(cx, cy), weight)| {
                        (px + cx * weight, py + cy * weight)
                    });
                assert!(
                    !(x < px && px < x + 400.0 && y < py && py < y + 250.0),
                    "edge enters intervening region at {px},{py}; box={x},{y}; route={:?}",
                    layout.routes[2]
                );
            }
        }
    }

    #[test]
    fn parallel_cycles_and_self_edges_keep_identity_and_bounds() {
        let nodes = [(4, (180.0, 60.0)), (9, (180.0, 60.0)), (21, (180.0, 60.0))];
        let edges = [(4, 9), (4, 9), (9, 21), (21, 4), (9, 9)];
        let first = compute_relative(&nodes, &edges, 40.0);
        let second = compute_relative(&nodes, &edges, 40.0);
        assert_eq!(first.positions, second.positions);
        assert_eq!(first.routes, second.routes);
        assert_eq!(first.routes.len(), edges.len());
        for route in &first.routes {
            assert!(!route.is_empty());
            for &(x, y) in route.iter().flatten() {
                assert!(x.is_finite() && y.is_finite());
                assert!(x >= 0.0 && x <= first.content.0);
                assert!(y >= 0.0 && y <= first.content.1);
            }
        }
    }
}
