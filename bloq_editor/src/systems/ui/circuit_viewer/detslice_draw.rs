//! Detector-slice overlay rendering: the detection regions each `DETECTOR`
//! compares, drawn over the per-node qubit layout for one flattened moment.
//!
//! The visual rules mirror Stim's `detector_slice` SVG output: detector regions are sorted
//! big-under-small and drawn as polygons, lenses, or circles; logical
//! observables are separate per-term circles so a high-weight logical does not
//! cover the circuit between its terms.
//!

use std::f32::consts::TAU;

use bevy_egui::egui::{self, Color32, Mesh, Pos2, Shape, Stroke};
use bloq_circuit::PauliBasis;
use glam::IVec2;

use super::geometry::{CanvasGeometry, grid_pos as project};
use crate::resources::{SliceRegionId, SliceRegionView, SliceVisibility};

/// The palette entries the overlay needs, lifted out of `CircuitViewerColors`.
#[derive(Clone, Copy)]
pub(super) struct DetsliceColors {
    x: Color32,
    y: Color32,
    z: Color32,
    mixed: Color32,
    outline: Color32,
    break_marker: Color32,
}

impl DetsliceColors {
    pub(super) fn from_circuit_colors(colors: &super::CircuitViewerColors) -> Self {
        Self {
            x: colors.detslice_x,
            y: colors.detslice_y,
            z: colors.detslice_z,
            mixed: colors.detslice_mixed,
            outline: colors.detslice_outline,
            break_marker: colors.detslice_break,
        }
    }

    fn pauli(&self, pauli: PauliBasis) -> Color32 {
        match pauli {
            PauliBasis::X => self.x,
            PauliBasis::Y => self.y,
            PauliBasis::Z => self.z,
        }
    }

    pub(super) fn region_color(&self, region: &SliceRegionView) -> Color32 {
        homogeneous_pauli(region).map_or(self.mixed, |pauli| self.pauli(pauli))
    }
}

/// Draws the detector-slice regions for one moment, plus any anticommutation
/// breaks. `isolated` keeps one region at full opacity while the rest dim.
pub(super) fn draw_detector_slices(
    painter: &egui::Painter,
    regions_at_moment: &[SliceRegionView],
    breaks: &[IVec2],
    geometry: CanvasGeometry,
    colors: DetsliceColors,
    isolated: Option<SliceRegionId>,
    visibility: SliceVisibility,
) {
    let shapes = compute_region_shapes(regions_at_moment, colors, visibility);
    let outline_width = (geometry.pitch * 0.06).clamp(0.8, 2.0);

    for shape in &shapes {
        let alpha = match isolated {
            Some(id) if id == shape.id => 1.0,
            // Another region is isolated: fade this one to the background.
            Some(_) => 0.25,
            // Stim fills 3+ term regions at 0.75 so overlaps stay readable;
            // 1-2 term lenses/circles stay solid.
            None if shape.term_count >= 3 => 0.75,
            None => 1.0,
        };

        let points = project_points(&shape.outline, geometry);
        if points.len() < 3 {
            continue;
        }

        // Fill first (no stroke); the contrast outline is a separate pass below
        // so it stays crisp over the gradient fans drawn between them.
        painter.add(Shape::convex_polygon(
            points.clone(),
            shape.fill.gamma_multiply(alpha),
            Stroke::NONE,
        ));

        // Mixed-Pauli regions have no single colour; a radial-gradient fan in
        // each term's Pauli colour blooms from that corner (Stim's per-corner
        // blur). Dimmed by the same hover alpha.
        for fan in &shape.fans {
            painter.add(Shape::mesh(fan_mesh(fan, geometry, alpha)));
        }

        // Contrast outline pass, on top of fill and fans.
        painter.add(Shape::convex_polygon(
            points,
            Color32::TRANSPARENT,
            Stroke::new(outline_width, colors.outline.gamma_multiply(alpha.max(0.5))),
        ));
    }

    // Anticommutation breaks: hollow markers on the qubits where a region hit a
    // gate it does not commute with. The Layer-2 tape supplies these; today the
    // slice was clean so the list is empty.
    if visibility.detectors {
        let break_radius = (geometry.pitch * 0.3).clamp(4.0, 14.0);
        for &coord in breaks {
            painter.circle_stroke(
                project(coord.x as f32, coord.y as f32, geometry),
                break_radius,
                Stroke::new(outline_width.max(1.5), colors.break_marker),
            );
        }
    }
}

/// The topmost region whose shape contains `pointer`, or `None`. Topmost is the
/// smallest region (drawn last, over the larger ones).
pub(super) fn region_at_pointer(
    regions_at_moment: &[SliceRegionView],
    geometry: CanvasGeometry,
    pointer: Pos2,
    visibility: SliceVisibility,
) -> Option<SliceRegionId> {
    regions_at_moment
        .iter()
        .filter(|region| visibility.shows(region.id))
        .filter(|&region| region_contains_pointer(region, geometry, pointer))
        .min_by_key(|region| region.terms.len())
        .map(SliceRegionView::id)
}

fn region_contains_pointer(
    region: &SliceRegionView,
    geometry: CanvasGeometry,
    pointer: Pos2,
) -> bool {
    let vertices: Vec<Pos2> = angle_sorted_vertices(region)
        .into_iter()
        .map(|coord| project(coord.x, coord.y, geometry))
        .collect();
    if matches!(region.id, SliceRegionId::Observable { .. }) {
        return vertices
            .iter()
            .any(|vertex| vertex.distance(pointer) <= geometry.pitch * 0.6);
    }
    match vertices.as_slice() {
        [] => false,
        // A circle/lens: hit-test as proximity to a term site. The generous
        // radius keeps a one/two-term region grabbable next to its qubit site.
        [_] | [_, _] => vertices
            .iter()
            .any(|vertex| vertex.distance(pointer) <= geometry.pitch * 0.6),
        _ => point_in_polygon(&vertices, pointer),
    }
}

/// A cached region's shape in grid space: a closed outline (dense where curved)
/// plus its fill colour and any per-term Pauli corner gradient fans.
#[derive(Clone)]
struct RegionShape {
    id: SliceRegionId,
    term_count: usize,
    /// Closed outline vertices in grid coordinates (y up), projected at draw.
    outline: Vec<egui::Vec2>,
    fill: Color32,
    /// Radial-gradient corner fans for mixed-Pauli regions, one per term (Stim's
    /// per-corner blur). Empty when the region is a single Pauli.
    fans: Vec<GradientFan>,
}

/// One term's corner gradient: a fan of rays from the qubit, each clipped to the
/// region outline, coloured solid at the centre and fading to transparent at the
/// rim (Stim's `blur_radius` circle clipped to the detector polygon). Stored in
/// grid space; the mesh (with the hover alpha) is built at draw time.
#[derive(Clone)]
struct GradientFan {
    /// The term's qubit position (a vertex of the region outline).
    center: egui::Vec2,
    /// Rim point per ray: `center + dir * length`, where `length` is the ray
    /// clipped to the outline (0 for rays pointing out of the region, so the fan
    /// collapses to the interior corner sector).
    rim: Vec<egui::Vec2>,
    /// The term's Pauli colour, at full opacity; alpha is applied at draw.
    color: Color32,
}

/// Builds the grid-space shapes, largest region first so smaller ones layer on
/// top (Stim's descending-term-count draw order).
fn compute_region_shapes(
    regions_at_moment: &[SliceRegionView],
    colors: DetsliceColors,
    visibility: SliceVisibility,
) -> Vec<RegionShape> {
    let mut ordered: Vec<&SliceRegionView> = regions_at_moment
        .iter()
        .filter(|region| visibility.shows(region.id))
        .collect();
    ordered.sort_by_key(|region| std::cmp::Reverse(region.terms.len()));
    ordered
        .into_iter()
        .filter(|region| !region.terms.is_empty())
        .flat_map(|region| match region.id {
            SliceRegionId::Detector { .. } => vec![region_shape(region, colors)],
            SliceRegionId::Observable { .. } => region
                .terms
                .iter()
                .map(|term| RegionShape {
                    id: region.id(),
                    term_count: 1,
                    outline: circle_outline(egui::vec2(term.qubit.x as f32, term.qubit.y as f32)),
                    fill: colors.pauli(term.pauli),
                    fans: Vec::new(),
                })
                .collect(),
        })
        .collect()
}

fn region_shape(region: &SliceRegionView, colors: DetsliceColors) -> RegionShape {
    let vertices = angle_sorted_vertices(region);
    let outline = match vertices.len() {
        1 => circle_outline(vertices[0]),
        2 => lens_outline(vertices[0], vertices[1]),
        _ => polygon_outline(&vertices),
    };
    let (fill, fans) = fill_and_fans(region, &outline, colors);
    RegionShape {
        id: region.id(),
        term_count: region.terms.len(),
        outline,
        fill,
        fans,
    }
}

/// The fill colour and mixed-region gradient fans. A single-Pauli region fills
/// solid in that Pauli's colour with no fans; a mixed region fills grey and gets
/// one radial-gradient fan per term, clipped to `outline`, in that term's Pauli
/// colour — Stim's per-corner blur, replacing the dots the qubit sites hid.
fn fill_and_fans(
    region: &SliceRegionView,
    outline: &[egui::Vec2],
    colors: DetsliceColors,
) -> (Color32, Vec<GradientFan>) {
    if let Some(pauli) = homogeneous_pauli(region) {
        return (colors.pauli(pauli), Vec::new());
    }

    let fans = region
        .terms
        .iter()
        .map(|term| {
            let center = egui::vec2(term.qubit.x as f32, term.qubit.y as f32);
            gradient_fan(center, colors.pauli(term.pauli), outline)
        })
        .collect();
    (colors.mixed, fans)
}

fn homogeneous_pauli(region: &SliceRegionView) -> Option<PauliBasis> {
    let first = region.terms.first()?.pauli;
    region
        .terms
        .iter()
        .all(|term| term.pauli == first)
        .then_some(first)
}

/// Rays cast per corner gradient.
const FAN_RAYS: usize = 24;
/// Gradient reach in grid units (Stim's `blur_radius`, ≈ one inter-qubit step).
const FAN_RADIUS: f32 = 0.9;

/// Builds one term's gradient fan: `FAN_RAYS` rays from `center`, each clipped
/// to the first crossing of `outline` (capped at `FAN_RADIUS`). The qubit sits
/// on the outline, so rays leaving the region clip to ~0 and the fan naturally
/// becomes the interior corner sector — Stim's polygon-clipped blur circle.
fn gradient_fan(center: egui::Vec2, color: Color32, outline: &[egui::Vec2]) -> GradientFan {
    let rim = (0..FAN_RAYS)
        .map(|k| {
            let angle = TAU * k as f32 / FAN_RAYS as f32;
            let dir = egui::vec2(angle.cos(), angle.sin());
            // A step into the ray decides in/out; the vertex itself sits on the
            // boundary, so probe just off it (ignoring the t≈0 self-crossing).
            let length = if point_in_polygon_grid(outline, center + dir * 1e-2) {
                first_ray_crossing(center, dir, outline).map_or(FAN_RADIUS, |t| t.min(FAN_RADIUS))
            } else {
                0.0
            };
            center + dir * length
        })
        .collect();
    GradientFan { center, rim, color }
}

/// The nearest outline crossing (`t > 1e-3`) of the ray `origin + t·dir`, or
/// `None` if the ray never re-crosses the closed polygon.
fn first_ray_crossing(origin: egui::Vec2, dir: egui::Vec2, poly: &[egui::Vec2]) -> Option<f32> {
    let mut best: Option<f32> = None;
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let edge = poly[(i + 1) % n] - a;
        let denom = dir.x * edge.y - dir.y * edge.x;
        if denom.abs() < 1e-9 {
            continue;
        }
        let diff = a - origin;
        let t = (diff.x * edge.y - diff.y * edge.x) / denom;
        let u = (diff.x * dir.y - diff.y * dir.x) / denom;
        if t > 1e-3 && (0.0..=1.0).contains(&u) {
            best = Some(best.map_or(t, |b| b.min(t)));
        }
    }
    best
}

/// Even-odd point-in-polygon test in grid space (the screen-space
/// [`point_in_polygon`] twin, used while building the cached fan geometry).
fn point_in_polygon_grid(poly: &[egui::Vec2], p: egui::Vec2) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (vi, vj) = (poly[i], poly[j]);
        if (vi.y > p.y) != (vj.y > p.y) {
            let x = vi.x + (p.y - vi.y) / (vj.y - vi.y) * (vj.x - vi.x);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Tessellates a [`GradientFan`] into a screen-space mesh: a solid core out to
/// half radius, fading to transparent at the rim. `alpha` is the region's hover
/// alpha, applied to every vertex.
fn fan_mesh(fan: &GradientFan, geometry: CanvasGeometry, alpha: f32) -> Mesh {
    let core = fan.color.gamma_multiply(0.9 * alpha);
    let rim = fan.color.gamma_multiply(0.0);
    let n = fan.rim.len();

    let mut mesh = Mesh::default();
    // Vertex 0 is the centre; then per ray a mid vertex (half radius, solid) and
    // a rim vertex (transparent), so the gradient runs centre→mid solid, mid→rim
    // fading — Stim's "solid to 50%, fade to 0 at 100%".
    mesh.colored_vertex(project_vec(fan.center, geometry), core);
    for &rim_point in &fan.rim {
        let mid = fan.center + (rim_point - fan.center) * 0.5;
        mesh.colored_vertex(project_vec(mid, geometry), core);
        mesh.colored_vertex(project_vec(rim_point, geometry), rim);
    }
    for i in 0..n {
        let (mid_i, rim_i) = (1 + 2 * i, 2 + 2 * i);
        let next = (i + 1) % n;
        let (mid_j, rim_j) = (1 + 2 * next, 2 + 2 * next);
        mesh.add_triangle(0, mid_i as u32, mid_j as u32);
        mesh.add_triangle(mid_i as u32, rim_i as u32, rim_j as u32);
        mesh.add_triangle(mid_i as u32, rim_j as u32, mid_j as u32);
    }
    mesh
}

/// Region term coordinates as float grid points sorted counter-clockwise around
/// their centroid, so the outline traces a simple (non-self-crossing) polygon.
fn angle_sorted_vertices(region: &SliceRegionView) -> Vec<egui::Vec2> {
    let mut points: Vec<egui::Vec2> = region
        .terms
        .iter()
        .map(|term| egui::vec2(term.qubit.x as f32, term.qubit.y as f32))
        .collect();
    let count = points.len() as f32;
    let centroid = points.iter().fold(egui::Vec2::ZERO, |sum, p| sum + *p) / count;
    points.sort_by(|a, b| {
        angle_from(centroid, *a)
            .partial_cmp(&angle_from(centroid, *b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    points
}

fn angle_from(origin: egui::Vec2, dst: egui::Vec2) -> f32 {
    let d = dst - origin;
    d.y.atan2(d.x)
}

/// A circle around a single term, sampled into a polygon. Radius ≈ 0.45 grid
/// units — large enough to hover next to the qubit site square.
fn circle_outline(center: egui::Vec2) -> Vec<egui::Vec2> {
    const SAMPLES: usize = 24;
    const RADIUS: f32 = 0.45;
    (0..SAMPLES)
        .map(|k| {
            let angle = TAU * k as f32 / SAMPLES as f32;
            center + egui::vec2(angle.cos(), angle.sin()) * RADIUS
        })
        .collect()
}

/// A lens between two terms: two arcs bowed to opposite sides of the segment, so
/// a two-term region reads as a rounded shape rather than a bare line.
fn lens_outline(a: egui::Vec2, b: egui::Vec2) -> Vec<egui::Vec2> {
    const SAMPLES: usize = 12;
    let axis = b - a;
    let length = axis.length();
    if length < f32::EPSILON {
        return circle_outline(a);
    }
    let perp = egui::vec2(-axis.y, axis.x) / length;
    let bow = length * 0.2;
    let mut points = Vec::with_capacity(2 * (SAMPLES + 1));
    for side in [1.0f32, -1.0] {
        for k in 0..=SAMPLES {
            let t = k as f32 / SAMPLES as f32;
            // Walk a→b on the first pass and b→a on the second so the two arcs
            // close into one loop.
            let t = if side > 0.0 { t } else { 1.0 - t };
            let bump = 4.0 * t * (1.0 - t);
            points.push(a + axis * t + perp * (bow * bump * side));
        }
    }
    points
}

/// A polygon over 3+ terms. Near-collinear edges (flat regions) bow outward via a
/// sampled quadratic curve so they keep visible area, matching Stim's
/// `_start_many_body_svg_path`.
fn polygon_outline(vertices: &[egui::Vec2]) -> Vec<egui::Vec2> {
    let count = vertices.len();
    let centroid = vertices.iter().fold(egui::Vec2::ZERO, |sum, p| sum + *p) / count as f32;
    let mut points = Vec::new();
    for k in 0..count {
        let prev = vertices[(k + count - 1) % count];
        let a = vertices[k];
        let b = vertices[(k + 1) % count];
        let next = vertices[(k + 2) % count];
        if is_collinear(prev, a, b) || is_collinear(a, b, next) {
            let mid = (a + b) * 0.5;
            let edge = b - a;
            let length = edge.length().max(f32::EPSILON);
            let mut normal = egui::vec2(-edge.y, edge.x) / length;
            // Bow away from the region centre so flat edges bulge outward.
            if normal.dot(mid - centroid) < 0.0 {
                normal = -normal;
            }
            let control = mid + normal * (length * 0.15);
            sample_quadratic(a, control, b, &mut points);
        } else {
            points.push(a);
        }
    }
    points
}

/// Appends a quadratic Bézier from `a` to `b` (control `c`) as line samples,
/// omitting the final point so consecutive edges do not double up vertices.
fn sample_quadratic(a: egui::Vec2, c: egui::Vec2, b: egui::Vec2, out: &mut Vec<egui::Vec2>) {
    const SAMPLES: usize = 6;
    for k in 0..SAMPLES {
        let t = k as f32 / SAMPLES as f32;
        let inv = 1.0 - t;
        out.push(a * (inv * inv) + c * (2.0 * inv * t) + b * (t * t));
    }
}

/// Whether `b` lies on (or very near) the line through `a` and `c`, i.e. the
/// three points are effectively collinear.
fn is_collinear(a: egui::Vec2, b: egui::Vec2, c: egui::Vec2) -> bool {
    const ATOL: f32 = 3e-2;
    let d1 = b - a;
    let d2 = c - b;
    let (n1, n2) = (d1.length(), d2.length());
    if n1 < ATOL || n2 < ATOL {
        return true;
    }
    let d1 = d1 / n1;
    let d2 = d2 / n2;
    // Cross product magnitude of the unit directions: ~0 when parallel.
    (d1.x * d2.y - d1.y * d2.x).abs() < ATOL
}

/// Projects a grid coordinate (y up) to screen space, mirroring
/// `geometry::grid_pos`.
fn project_points(grid_points: &[egui::Vec2], geometry: CanvasGeometry) -> Vec<Pos2> {
    grid_points
        .iter()
        .map(|point| project(point.x, point.y, geometry))
        .collect()
}

fn project_vec(grid_point: egui::Vec2, geometry: CanvasGeometry) -> Pos2 {
    project(grid_point.x, grid_point.y, geometry)
}

/// Even-odd ray-cast point-in-polygon test in screen space.
fn point_in_polygon(vertices: &[Pos2], point: Pos2) -> bool {
    let mut inside = false;
    let mut j = vertices.len() - 1;
    for i in 0..vertices.len() {
        let (vi, vj) = (vertices[i], vertices[j]);
        let crosses = (vi.y > point.y) != (vj.y > point.y);
        if crosses {
            let x = vi.x + (point.y - vi.y) / (vj.y - vi.y) * (vj.x - vi.x);
            if point.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::super::geometry::CoordBounds;
    use super::*;
    use glam::ivec2;

    fn region(id: (u32, u32), terms: &[(IVec2, PauliBasis)]) -> SliceRegionView {
        SliceRegionView {
            id: SliceRegionId::Detector {
                owner_node: id.0,
                detector: id.1,
            },
            coords: None,
            terms: terms
                .iter()
                .map(|&(qubit, pauli)| bloq_circuit::RegionTerm { qubit, pauli })
                .collect(),
        }
    }

    fn test_geometry() -> CanvasGeometry {
        CanvasGeometry {
            bounds: CoordBounds {
                min: ivec2(0, 0),
                max: ivec2(4, 4),
            },
            origin: egui::pos2(0.0, 0.0),
            pitch: 20.0,
        }
    }

    fn all_slices() -> SliceVisibility {
        SliceVisibility {
            detectors: true,
            observables: true,
        }
    }

    #[test]
    fn homogeneous_region_fills_solid_without_fans() {
        let colors = test_colors();
        let region = region(
            (1, 0),
            &[
                (ivec2(0, 0), PauliBasis::Z),
                (ivec2(1, 0), PauliBasis::Z),
                (ivec2(1, 1), PauliBasis::Z),
                (ivec2(0, 1), PauliBasis::Z),
            ],
        );

        let shape = region_shape(&region, colors);

        assert_eq!(shape.fill, Color32::BLUE);
        assert!(shape.fans.is_empty(), "single-Pauli region needs no fans");
    }

    #[test]
    fn observable_region_uses_separate_term_markers() {
        let mut observable = region(
            (0, 0),
            &[
                (ivec2(0, 0), PauliBasis::X),
                (ivec2(4, 0), PauliBasis::Z),
                (ivec2(4, 4), PauliBasis::Y),
                (ivec2(0, 4), PauliBasis::X),
            ],
        );
        observable.id = SliceRegionId::Observable { index: 12 };

        let shapes = compute_region_shapes(
            std::slice::from_ref(&observable),
            test_colors(),
            all_slices(),
        );

        assert_eq!(shapes.len(), observable.terms.len());
        assert_eq!(
            shapes.iter().map(|shape| shape.fill).collect::<Vec<_>>(),
            [Color32::RED, Color32::BLUE, Color32::GREEN, Color32::RED]
        );
        assert!(shapes.iter().all(|shape| shape.fans.is_empty()));
        assert_eq!(
            region_at_pointer(
                &[observable],
                test_geometry(),
                project(2.0, 2.0, test_geometry()),
                all_slices(),
            ),
            None,
            "empty space between observable terms must stay interactive with the circuit"
        );
    }

    #[test]
    fn visibility_filters_detector_and_observable_regions() {
        let detector = region((1, 2), &[(ivec2(0, 0), PauliBasis::X)]);
        let mut observable = region((0, 0), &[(ivec2(1, 0), PauliBasis::Z)]);
        observable.id = SliceRegionId::Observable { index: 3 };
        let regions = [detector, observable];

        let shapes = compute_region_shapes(
            &regions,
            test_colors(),
            SliceVisibility {
                detectors: false,
                observables: true,
            },
        );

        assert_eq!(shapes.len(), 1);
        assert_eq!(shapes[0].id, SliceRegionId::Observable { index: 3 });
    }

    #[test]
    fn mixed_region_greys_and_fans_each_term() {
        let colors = test_colors();
        // Two-term lens: X on (0,0), Z on (1,0). The region extends toward +x
        // from the X corner and toward -x from the Z corner.
        let region = region(
            (1, 0),
            &[(ivec2(0, 0), PauliBasis::X), (ivec2(1, 0), PauliBasis::Z)],
        );

        let shape = region_shape(&region, colors);

        assert_eq!(shape.fill, Color32::GRAY);
        assert_eq!(shape.fans.len(), 2);
        // Each term gets a fan in its own Pauli colour, centred on its qubit.
        let x_fan = shape.fans.iter().find(|f| f.color == Color32::RED).unwrap();
        let z_fan = shape
            .fans
            .iter()
            .find(|f| f.color == Color32::BLUE)
            .unwrap();
        assert_eq!(x_fan.center, egui::vec2(0.0, 0.0));
        assert_eq!(z_fan.center, egui::vec2(1.0, 0.0));

        for fan in &shape.fans {
            // Every rim point stays inside the region bbox (+ eps for the bow),
            // i.e. the fan is clipped to the outline, not a free circle.
            for &p in &fan.rim {
                assert!(
                    p.x >= -0.3 && p.x <= 1.3 && p.y.abs() <= 0.3,
                    "rim point {p:?} escaped the region",
                );
            }
            let lengths: Vec<f32> = fan.rim.iter().map(|p| (*p - fan.center).length()).collect();
            // Rays pointing out of the region clamp to ~0; rays into it span out.
            assert!(lengths.iter().any(|l| *l < 0.05), "no clamped outward ray");
            assert!(lengths.iter().any(|l| *l > 0.4), "no spanning inward ray");
        }
    }

    #[test]
    fn pointer_hits_enclosing_plaquette_and_misses_outside() {
        let geometry = test_geometry();
        let plaquette = region(
            (2, 3),
            &[
                (ivec2(0, 0), PauliBasis::Z),
                (ivec2(2, 0), PauliBasis::Z),
                (ivec2(2, 2), PauliBasis::Z),
                (ivec2(0, 2), PauliBasis::Z),
            ],
        );
        let regions = [plaquette];

        let inside = project(1.0, 1.0, geometry);
        let outside = project(3.5, 3.5, geometry);

        assert_eq!(
            region_at_pointer(&regions, geometry, inside, all_slices()),
            Some(SliceRegionId::Detector {
                owner_node: 2,
                detector: 3,
            })
        );
        assert_eq!(
            region_at_pointer(&regions, geometry, outside, all_slices()),
            None
        );
    }

    fn test_colors() -> DetsliceColors {
        DetsliceColors {
            x: Color32::RED,
            y: Color32::GREEN,
            z: Color32::BLUE,
            mixed: Color32::GRAY,
            outline: Color32::BLACK,
            break_marker: Color32::from_rgb(255, 0, 255),
        }
    }
}
