"""Project the public glTF exporter into small SVGs for the Blocks and Pipes guide.

Run with: uv run --project bloq_py --no-sync python tools/render_block_thumbnails.py
The native exporter supplies the geometry and colors. Pipe walls render from
both sides, retaining their open connections when endpoint blocks are hidden.
"""

import base64
import json
import math
from pathlib import Path
import struct
import tempfile

import bloq


def thumbnail(graph, path, title, *, opaque=False, pipe_axis=None, view_side=1):
    with tempfile.TemporaryDirectory() as temporary:
        source = Path(temporary) / "graph.gltf"
        graph.export_gltf(source)
        model = json.loads(source.read_text())
        buffers = [base64.b64decode(buffer["uri"].split(",", 1)[1])
                   for buffer in model["buffers"]]

    def accessor(index):
        item = model["accessors"][index]
        view = model["bufferViews"][item["bufferView"]]
        dimensions = {"VEC3": 3, "VEC4": 4}[item["type"]]
        assert item["componentType"] == 5126
        offset = view.get("byteOffset", 0) + item.get("byteOffset", 0)
        stride = view.get("byteStride", dimensions * 4)
        return [struct.unpack_from("<" + "f" * dimensions, buffers[view["buffer"]], offset + i * stride)
                for i in range(item["count"])]

    # glTF is y-up. Look down from above, with both spatial axes visible.
    def project(point):
        x, y, z = point
        return ((x - view_side * z) / math.sqrt(2), (x + view_side * z) / math.sqrt(6) - y * math.sqrt(2 / 3))

    triangles, lines, points, visible_edges, front_planes = [], [], [], set(), []

    def edge_key(a, b):
        return tuple(sorted(tuple(round(c, 5) for c in vertex) for vertex in (a, b)))

    for primitive in model["meshes"][0]["primitives"]:
        vertices = accessor(primitive["attributes"]["POSITION"])
        material = model["materials"][primitive["material"]]["pbrMetallicRoughness"]["baseColorFactor"]
        colors = accessor(primitive["attributes"]["COLOR_0"]) if "COLOR_0" in primitive["attributes"] else [material] * len(vertices)
        size = 2 if primitive.get("mode", 4) == 1 else 3
        for start in range(0, len(vertices), size):
            group = vertices[start:start + size]
            # Hide both endpoint Ports, including their wireframes.
            if pipe_axis is not None and not all(.5 - 1e-5 <= p[pipe_axis] <= 2.5 + 1e-5 for p in group):
                continue
            points.extend(map(project, group))
            if size == 2:
                lines.append(group)
            else:
                rgba = [material[i] * colors[start][i] for i in range(4)]
                if opaque:
                    rgba[3] = 1
                # Native colors are linear, SVG colors are sRGB.
                rgb = [round(255 * (12.92 * c if c <= .0031308 else 1.055 * c ** (1 / 2.4) - .055)) for c in rgba[:3]]
                a, b, c = group
                u, v = [b[i] - a[i] for i in range(3)], [c[i] - a[i] for i in range(3)]
                normal = (u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0])
                front = normal[0] + normal[1] + view_side * normal[2] > 0
                # The native pipe material is double-sided: its rear walls
                # remain visible through the open attachment ends.
                if front or pipe_axis is not None or rgba[3] < 1:
                    depth = sum(v[0] + v[1] + view_side * v[2] for v in group) / 3
                    triangles.append((depth, group, rgb, rgba[3]))
                if front or rgba[3] < 1:
                    visible_edges.update(edge_key(group[i], group[(i + 1) % 3]) for i in range(3))
                    front_planes.append((normal, a))
    left, right = min(p[0] for p in points), max(p[0] for p in points)
    top, bottom = min(p[1] for p in points), max(p[1] for p in points)
    scale = min(112 / (right - left), 88 / (bottom - top))

    def coordinates(group):
        return " ".join(f"{64 + (x - (left + right) / 2) * scale:.2f},{52 + (y - (top + bottom) / 2) * scale:.2f}" for x, y in map(project, group))

    svg = ['<svg xmlns="http://www.w3.org/2000/svg" width="128" height="104" viewBox="0 0 128 104">',
           f"<title>{title}</title>"]
    for _, group, rgb, alpha in sorted(triangles, key=lambda triangle: triangle[0]):
        svg.append(f'<polygon points="{coordinates(group)}" fill="rgb({",".join(map(str, rgb))})" fill-opacity="{alpha:.3f}"/>')
    for group in lines:
        on_front_plane = pipe_axis is not None and any(
            all(abs(sum(n[i] * (p[i] - a[i]) for i in range(3))) < 1e-5 for p in group)
            for n, a in front_planes)
        if edge_key(*group) in visible_edges or on_front_plane:
            svg.append(f'<polyline points="{coordinates(group)}" fill="none" stroke="#343a40" stroke-width=".65"/>')
    path.write_text("\n".join(svg) + "\n</svg>\n")


def main():
    output = Path(__file__).resolve().parents[1] / "docs" / "assets" / "blocks"
    output.mkdir(parents=True, exist_ok=True)
    kinds = {"regular-cube": "ZXZ", "spatial-cube": "ZZX", "port": "Port",
             "y": "Y", "measurement": "X", "t": "T", "selective": "XY",
             "walking": bloq.BlockKind.walking("ZXZ", (1, 0)),
             "rotation": bloq.BlockKind.patch_rotation("Z", (0, 1))}
    for name, kind in kinds.items():
        graph = bloq.BlockGraph()
        graph.add_block(bloq.Block((0, 0, 0), kind))
        thumbnail(graph, output / f"{name}.svg", name.replace("-", " "), opaque=name == "port",
                  view_side=-1 if name == "rotation" else 1)
    for name, direction, hadamard in (("pipe", "+X", False),
                                      ("temporal-hadamard", "+Z", True),
                                      ("spatial-hadamard", "+X", True)):
        graph = bloq.BlockGraph()
        graph.add_block(bloq.Block((0, 0, 0), "ZXZ" if hadamard else "Port"))
        graph.add_block(bloq.Block((0, 0, 1) if direction == "+Z" else (1, 0, 0), "XZX" if hadamard else "Port"))
        graph.add_pipe(bloq.Pipe((0, 0, 0), direction, hadamard=hadamard))
        thumbnail(graph, output / f"{name}.svg", name.replace("-", " "), opaque=True,
                  pipe_axis=1 if direction == "+Z" else 0)
    print(f"Rendered {len(kinds) + 3} native block and pipe thumbnails in {output}")


if __name__ == "__main__":
    main()
