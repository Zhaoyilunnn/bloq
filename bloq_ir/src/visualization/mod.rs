//! Standalone SVG views of the physical program DAG, using the editor's layout.
//!
//! Rendering preserves region bodies and does not instantiate templates, select
//! guarded alternatives, execute the program, or perform a full validation audit.

pub mod layout;

use std::collections::HashMap;
use std::fmt::Write;

use crate::{Bloq, BloqEdge, BloqNode, BloqNodeId, ClassicalNode, NodeProvenance, SubGraph};

impl Bloq {
    /// Render the program dependency graph as a self-contained SVG.
    ///
    /// `include_classical` includes classical nodes and their incident edges.
    /// When false, quantum nodes, regions and their direct dependencies remain.
    /// Nested region bodies are drawn inside their owning container. Quantum, composition,
    /// value and order edges retain distinct colors; this is a structural view
    /// of all alternatives, not one selected runtime execution.
    ///
    /// ```
    /// let program = bloq_ir::Bloq::new();
    /// let svg = program.to_svg(true);
    /// assert!(svg.starts_with("<svg"));
    /// ```
    pub fn to_svg(&self, include_classical: bool) -> String {
        let mut rendered = render_level(self.top(), include_classical);
        rendered.width = rendered.width.max(430.0);
        write!(rendered.svg, "<g transform=\"translate(0 {})\"><text style=\"fill:#4a65b8\">Quantum</text><text x=\"100\" style=\"fill:#b788ee\">Compose</text><text x=\"210\" style=\"fill:#4b8c67\">Value</text><text x=\"280\" style=\"fill:#758198\">Order (dashed)</text></g>", rendered.height + 28.0).expect("write SVG legend");
        rendered.height += 40.0;
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\" role=\"img\" aria-label=\"Bloq IR dependency graph\"><style>text{{font-family:system-ui,sans-serif;font-size:13px;fill:#202536}}.region{{fill:#f7f8fc;stroke:#758198}}.quantum{{fill:#e6edff;stroke:#4a65b8}}.classical{{fill:#e8f6ed;stroke:#4b8c67}}</style><defs>{}</defs><rect width=\"100%\" height=\"100%\" fill=\"white\"/><g transform=\"translate(24 24)\">{}</g></svg>",
            rendered.width + 48.0,
            rendered.height + 48.0,
            rendered.width + 48.0,
            rendered.height + 48.0,
            ["quantum", "compose", "value", "order"].into_iter().zip(["#4a65b8", "#b788ee", "#4b8c67", "#758198"]).map(|(kind, color)| format!("<marker id=\"{kind}\" markerWidth=\"7\" markerHeight=\"7\" refX=\"6\" refY=\"3\" orient=\"auto\"><path d=\"M0 0 L6 3 L0 6 Z\" fill=\"{color}\"/></marker>")).collect::<String>(),
            rendered.svg,
        )
    }
}

struct RenderedLevel {
    width: f32,
    height: f32,
    svg: String,
}

fn render_level(level: &SubGraph, include_classical: bool) -> RenderedLevel {
    let nodes = level
        .nodes()
        .filter(|(_, node)| include_classical || node.try_classical().is_none())
        .collect::<Vec<_>>();
    let mut bodies = HashMap::new();
    let mut sizes = Vec::new();
    for (id, node) in &nodes {
        let mut width = 190.0_f32.max(node_label(node).len() as f32 * 8.0 + 60.0);
        if let Some(detail) = node_detail(level, *id, node) {
            width = width.max(detail.len() as f32 * 8.0 + 24.0);
        }
        let mut height = 60.0;
        if let Some(region) = node.try_region() {
            let mut children = Vec::new();
            for (_, body) in region.bodies() {
                let child = render_level(body, include_classical);
                width = width.max(child.width + 32.0);
                height += child.height + 24.0;
                children.push(child);
            }
            bodies.insert(id.0, children);
        }
        sizes.push((id.0, (width, height)));
    }
    let size_of = sizes.iter().copied().collect::<HashMap<_, _>>();
    let edges = level
        .edges()
        .filter(|edge| size_of.contains_key(&edge.source.0) && size_of.contains_key(&edge.target.0))
        .collect::<Vec<_>>();
    let pairs = edges
        .iter()
        .map(|edge| (edge.source.0, edge.target.0))
        .collect::<Vec<_>>();
    let placed = layout::compute_relative(&sizes, &pairs, 40.0);
    let mut svg = String::new();
    for (index, edge) in edges.into_iter().enumerate() {
        let (kind, color, dash) = match edge.edge {
            BloqEdge::Quantum(_) => ("quantum", "#4a65b8", ""),
            BloqEdge::Compose { .. } => ("compose", "#b788ee", ""),
            BloqEdge::Value { .. } => ("value", "#4b8c67", ""),
            BloqEdge::Order => ("order", "#758198", " stroke-dasharray=\"4 3\""),
        };
        let mut path = String::new();
        for (segment, &[start, first, second, end]) in placed.routes[index].iter().enumerate() {
            if segment == 0 {
                write!(path, "M{} {} ", start.0, start.1).expect("write path start");
            }
            write!(
                path,
                "C{} {} {} {} {} {} ",
                first.0, first.1, second.0, second.1, end.0, end.1
            )
            .expect("write routed curve");
        }
        write!(svg, "<path data-edge=\"{kind}\" d=\"{path}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"1.5\"{dash} marker-end=\"url(#{kind})\"/>").expect("write SVG string");
    }
    for (id, node) in nodes {
        let (x, y) = placed.positions[&id.0];
        let (w, h) = size_of[&id.0];
        let kind = if node.try_region().is_some() {
            "region"
        } else if node.try_quantum().is_some() {
            "quantum"
        } else {
            "classical"
        };
        write!(svg, "<g data-node=\"{}\"><rect class=\"{kind}\" x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" rx=\"7\"/><text x=\"{}\" y=\"{}\">N{} · {}</text>", id.0, x + 12.0, y + 23.0, id.0, escape_text(&node_label(node))).expect("write SVG string");
        if let Some(quantum) = node.try_quantum() {
            write!(
                svg,
                "<text x=\"{}\" y=\"{}\">{} instances · {} guards</text>",
                x + 12.0,
                y + 44.0,
                quantum.instances.len(),
                quantum.guards.len()
            )
            .expect("write SVG string");
        }
        if let Some(detail) = node_detail(level, id, node) {
            write!(
                svg,
                "<text x=\"{}\" y=\"{}\">{}</text>",
                x + 12.0,
                y + 44.0,
                escape_text(&detail)
            )
            .expect("write SVG provenance");
        }
        if let Some(children) = bodies.remove(&id.0) {
            let mut top = y + 44.0;
            for child in children {
                write!(
                    svg,
                    "<g transform=\"translate({} {top})\">{}</g>",
                    x + 16.0,
                    child.svg
                )
                .expect("write SVG string");
                top += child.height + 24.0;
            }
        }
        svg.push_str("</g>");
    }
    RenderedLevel {
        width: placed.content.0.max(190.0),
        height: placed.content.1.max(60.0),
        svg,
    }
}

fn node_label(node: &BloqNode) -> String {
    match &node.provenance {
        NodeProvenance::OutputFrame { basis, .. } => {
            format!("{} · Output frame {basis:?}", node.kind_name())
        }
        NodeProvenance::BranchSelector { .. } => format!("{} · Branch selector", node.kind_name()),
        _ => match node.try_classical() {
            Some(ClassicalNode::Observable { index: None, .. }) => "Observable fragment".into(),
            Some(ClassicalNode::Observable {
                index: Some(index), ..
            }) => format!("Observable {index}"),
            _ => node.kind_name().into(),
        },
    }
}

fn node_detail(level: &SubGraph, id: BloqNodeId, node: &BloqNode) -> Option<String> {
    node.try_classical()?;
    let mut parts = Vec::new();
    if let Some(slot) = node.activation {
        parts.push(
            level
                .value_inputs(id)
                .find(|input| input.slot == slot)
                .map_or_else(
                    || format!("when input {slot}"),
                    |input| format!("when N{}", input.producer.0),
                ),
        );
    }
    if node.provenance != NodeProvenance::None {
        parts.push(node.provenance.to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClassicalExpr, QuantumNode, RegionNode};

    #[test]
    fn semantic_roles_preserve_node_kinds_and_escape_source_names() {
        let mut program = Bloq::new();
        for basis in [crate::Basis::X, crate::Basis::Z] {
            let node = BloqNode::classical(ClassicalNode::Compute {
                expr: ClassicalExpr::Const(false),
            })
            .with_provenance(NodeProvenance::OutputFrame {
                port: glam::ivec3(1, 2, 3),
                basis,
            });
            program.add_node(node);
        }
        program.add_node(
            BloqNode::classical(ClassicalNode::Compute {
                expr: ClassicalExpr::Const(false),
            })
            .with_provenance(NodeProvenance::BranchSelector {
                name: "a < b & c".into(),
            }),
        );
        let svg = program.to_svg(true);
        assert!(svg.contains("Compute · Output frame X"));
        assert!(svg.contains("Compute · Output frame Z"));
        assert!(svg.contains("frame x (1,2,3)"));
        assert!(svg.contains("Compute · Branch selector"));
        assert!(svg.contains("selector a &lt; b &amp; c"));
        assert!(!svg.contains("selector a < b & c"));
        assert!(!program.to_svg(false).contains("Output frame"));
    }

    #[test]
    fn classical_flag_filters_nodes_and_edges_without_dropping_regions() {
        let mut body = SubGraph::new();
        let q0 = body.add_node(BloqNode::quantum(QuantumNode::default()));
        let q1 = body.add_node(BloqNode::quantum(QuantumNode::default()));
        body.add_edge(q0, q1, BloqEdge::quantum(Vec::new()));
        let first = body.add_node(BloqNode::classical(ClassicalNode::Compute {
            expr: ClassicalExpr::Const(true),
        }));
        let second = body.add_node(BloqNode::classical(ClassicalNode::Compute {
            expr: ClassicalExpr::In(0),
        }));
        body.add_edge(first, second, BloqEdge::value(0));
        let fragment = body.add_node(BloqNode::classical(ClassicalNode::observable_fragment(
            Vec::new(),
            Vec::new(),
        )));
        let observable = body.add_node(BloqNode::classical(ClassicalNode::observable(0)));
        body.add_edge(fragment, observable, BloqEdge::compose(0));
        let mut program = Bloq::new();
        program.add_node(BloqNode::region(RegionNode::RepeatUntilSuccess {
            restart_source: None,
            restart_condition: ClassicalExpr::Const(false),
            body,
        }));
        let full = program.to_svg(true);
        assert!(full.contains("RepeatUntilSuccess"));
        assert!(full.contains("Compute"));
        assert!(full.contains("data-edge=\"value\""));
        assert!(full.contains("data-edge=\"compose\""));
        assert!(full.contains("marker id=\"compose\""));
        let quantum = program.to_svg(false);
        assert!(quantum.contains("RepeatUntilSuccess"));
        assert!(quantum.contains("Quantum"));
        assert!(quantum.contains("data-edge=\"quantum\""));
        assert!(!quantum.contains("Compute"));
        assert!(!quantum.contains("data-edge=\"value\""));
        assert!(!quantum.contains("data-edge=\"compose\""));
        assert_eq!(full, program.to_svg(true));
    }
}
