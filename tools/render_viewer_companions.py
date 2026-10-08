"""Retain the paper's 2D panels when replacing its 3D screenshots with viewers.

Requires Typst and the original research figure sources; leaves those sources unchanged.
"""

import argparse
import hashlib
import html
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def zx_companions(output, paper_figures):
    """Draw the paper's ZX correspondence and local Pauli identities as SVG."""
    def write(name, width, height, content):
        (output / name).write_text(
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">'
            f'<rect width="{width}" height="{height}" fill="white"/>'
            '<style>text{font:14px sans-serif;fill:#111;text-anchor:middle}line{stroke:#111;stroke-width:2}'
            '.web-X{stroke:#d72626;stroke-width:6}.web-Z{stroke:#284ecb;stroke-width:6}</style>'
            + content + '</svg>\n')

    def line(x1, y1, x2, y2, pauli=''):
        return f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" class="web-{pauli}"/>'

    def dot(x, y, basis, radius=12, phase=''):
        color = {'Z': '#7396ff', 'X': '#ff7f7f'}[basis]
        return (f'<circle cx="{x}" cy="{y}" r="{radius}" fill="{color}" stroke="#111"/>'
                + (f'<text x="{x}" y="{y+5}">{phase}</text>' if phase else ''))

    def junction(x, y, basis, paulis='IIII', gates=False):
        content = []
        for (dx, dy), pauli in zip(((-48, 0), (48, 0), (0, -48), (0, 48)), paulis):
            content.append(line(x, y, x + dx, y + dy))
            if pauli == 'I':
                continue
            if gates:
                gx, gy = x + dx * .65, y + dy * .65
                content.append(f'<g data-pauli="{pauli}">{dot(gx, gy, pauli, radius=10, phase="π")}</g>')
            else:
                content.append(line(x, y, x + dx, y + dy, pauli))
        content.append(dot(x, y, basis))
        return ''.join(content)

    panels = []
    for degree in range(1, 5):
        x = 80 + (degree - 1) * 160
        panels.append(f'<text x="{x}" y="22">{degree} {"leg" if degree == 1 else "legs"}</text>')
        for dx, dy in ((-50, 0), (50, 0), (0, -50), (0, 50))[:degree]:
            panels.append(f'<line x1="{x}" y1="95" x2="{x+dx}" y2="{95+dy}"/>')
        panels.append(dot(x, 95, 'Z', radius=10))
    write('zx-spiders.svg', 640, 165, ''.join(panels))
    content = []
    for x, name in ((120, 'Control'), (320, 'Target')):
        content.append(f'<line x1="{x}" y1="45" x2="{x}" y2="265"/>'
                       f'<text x="{x}" y="25">{name} output</text><text x="{x}" y="292">{name} input</text>')
    content.append('<line x1="120" y1="155" x2="320" y2="155"/>')
    for x1, y1, x2, y2 in ((120,45,120,265), (120,155,320,155), (320,45,320,155)):
        content.append(f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" style="stroke:#d72626;stroke-width:6"/>')
    for x, basis in ((120, 'Z'), (320, 'X')):
        content.append(dot(x, 155, basis, radius=14))
    write('zx-cnot-web.svg', 440, 315, ''.join(content))

    # The same native graph and highlighted edges used in the paper figure.
    cnot = json.loads((paper_figures / 'data/zx-cnot-map.json').read_text())
    topology = {'nodes': [{key: node[key] for key in ('id', 'position', 'basis')}
                          for node in cnot['nodes']],
                'edges': cnot['native_edges'], 'web_edges': cnot['web_edges']}
    points = {node['id']: (80 + (0 if node['position'][1] == 0 else
                              280 if node['position'][0] else 140),
                          300 - node['position'][2] * 235 / 3)
              for node in cnot['nodes']}
    content = [f'<metadata id="source-graph">{html.escape(json.dumps(topology))}</metadata>',
               '<text x="220" y="20">Direct translation</text>',
               '<text x="710" y="20">After identity removal</text>',
               '<text x="455" y="180" style="font-size:30px">→</text>']
    for edges, pauli in ((cnot['native_edges'], ''), (cnot['web_edges'], 'X')):
        for u, v in edges:
            content.append(line(*points[u], *points[v], pauli))
    for node in cnot['nodes']:
        if node['basis'] != 'Port':
            content.append(dot(*points[node['id']], node['basis']))
    for x, name in ((80, 'Control'), (360, 'Target'), (600, 'Control'), (820, 'Target')):
        content.append(f'<text x="{x}" y="45">{name} output</text>'
                       f'<text x="{x}" y="330">{name} input</text>')
    for x in (600, 820):
        content.append(line(x, 65, x, 300))
    content.append(line(600, 180, 820, 180))
    for segment in ((600, 65, 600, 300), (600, 180, 820, 180), (820, 65, 820, 180)):
        content.append(line(*segment, 'X'))
    content.extend((dot(600, 180, 'Z'), dot(820, 180, 'X')))
    write('zx-cnot-correspondence.svg', 900, 350, ''.join(content))

    content = ['<text x="230" y="25">Bare spider</text>',
               '<text x="460" y="25">Pauli insertion</text>',
               '<text x="750" y="25">Pauli web</text>']
    for row, (basis, paulis, caption) in enumerate((
            ('Z', 'XXXX', 'All X legs'), ('Z', 'ZIZI', 'A pair of Z legs'),
            ('X', 'ZZZZ', 'All Z legs'), ('X', 'XIXI', 'A pair of X legs'))):
        y = 110 + row * 140
        content.append(f'<g data-basis="{basis}" data-paulis="{paulis}">'
                       f'<text x="85" y="{y-8}">{basis} spider</text>'
                       f'<text x="85" y="{y+15}">{caption}</text>')
        content.extend((junction(230, y, basis),
                        f'<text x="340" y="{y+8}" style="font-size:28px">=</text>',
                        junction(460, y, basis, paulis, gates=True),
                        f'<text x="610" y="{y+8}" style="font-size:28px">⇒</text>',
                        junction(750, y, basis, paulis), '</g>'))
    write('zx-spider-rules.svg', 900, 590, ''.join(content))

    content = ['<text x="400" y="22">Combine webs: repeated support cancels</text>']
    for x, paulis in ((120, 'ZIZI'), (400, 'IZZI'), (680, 'ZZII')):
        content.append(junction(x, 95, 'Z', paulis))
    content.extend(('<text x="260" y="104" style="font-size:28px">⊕</text>',
                    '<text x="540" y="104" style="font-size:28px">=</text>',
                    '<text x="400" y="210">Join matching support on a shared edge</text>',
                    junction(300, 290, 'Z', 'XXXX'),
                    junction(500, 290, 'Z', 'XXXX'),
                    line(348, 290, 452, 290, 'X')))
    write('zx-web-composition.svg', 800, 355, ''.join(content))


def companion(source, name):
    if name == "preliminary-cnot":
        start = source.index('    editor-render("data/preliminary-cnot.png"')
        stop = source.index('    [(b) Logical CNOT pipe diagram],', start)
        return source[:start] + source[stop + len('    [(b) Logical CNOT pipe diagram],\n'):]
    if name == "regular-cube-observables":
        start = source.index('#grid(columns: (34mm, 46mm)')
        return source[:start] + '#align(center, annotated-patch)\n'
    if name == "spatial-cube-patches":
        source = source.replace('if annotated { surfaces } else { editor-render(render, height: 24mm) },', '')
        source = source.replace('columns: if annotated { (41mm, 42mm) } else { (37mm, 46mm) }', 'columns: (1fr,)')
        return source
    if name == "patch-rotation":
        start = source.index('#align(center + horizon, grid(')
        return source[:start] + '#align(center + horizon, evolution)\n'
    raise ValueError(name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--paper-figures', type=Path, required=True)
    parser.add_argument('--zx-only', action='store_true', help='Regenerate only the ZX SVG illustrations')
    args = parser.parse_args()
    output = ROOT / 'docs/assets/paper'
    provenance = ROOT / 'docs/assets/source-provenance.json'
    records = json.loads(provenance.read_text())
    zx_companions(output, args.paper_figures)
    for filename, source_name in (('zx-spiders.svg', 'preliminary-zx-junctions.tex'),
                                  ('zx-cnot-web.svg', 'data/zx-cnot-reduced.tikz'),
                                  ('zx-cnot-correspondence.svg', 'data/zx-cnot-map.json'),
                                  ('zx-spider-rules.svg', 'preliminary-zx-relations.tex'),
                                  ('zx-web-composition.svg', 'preliminary-zx-relations.tex')):
        record = {'file': f'assets/paper/{filename}',
                  'source': f'Research figure source: {source_name}',
                  'source_sha256': hashlib.sha256((args.paper_figures / source_name).read_bytes()).hexdigest(),
                  'sha256': hashlib.sha256((output / filename).read_bytes()).hexdigest(),
                  'derivation': '2D ZX connectivity and Pauli-web rules, with basis indicated by spider color',
                  'generator': 'tools/render_viewer_companions.py'}
        records['files'] = [r for r in records['files'] if r['file'] != record['file']]
        records['files'].append(record)
    if args.zx_only:
        provenance.write_text(json.dumps(records, indent=2) + '\n')
        return
    with tempfile.TemporaryDirectory() as temporary:
        work = Path(temporary)
        shutil.copyfile(args.paper_figures / 'lib.typ', work / 'lib.typ')
        (work / 'data').mkdir()
        for path in (args.paper_figures / 'data').glob('*.json'):
            shutil.copyfile(path, work / 'data' / path.name)
        for name in ('preliminary-cnot', 'regular-cube-observables', 'spatial-cube-patches', 'patch-rotation'):
            source = (args.paper_figures / f'{name}.typ').read_text()
            # Remove unused screenshot bindings so no PNG files are needed.
            edited = companion(source, name)
            if name == 'regular-cube-observables':
                edited = edited.replace('#let block-graph = image("regular-cube-cutaway.png", width: 32mm)', '')
            elif name == 'spatial-cube-patches':
                edited = edited.replace('#let surfaces = image("spatial-cube-cutaway.png", width: 34.46mm)', '')
            (work / f'{name}.typ').write_text('#set page(fill: white)\n' + edited)
            destination = output / f'{name}-physical.svg'
            subprocess.run(['typst', 'compile', '--root', str(work), str(work / f'{name}.typ'), str(destination)], check=True)
            record = {'file': f'assets/paper/{destination.name}',
                      'source': f'Research figure source: {name}.typ',
                      'source_sha256': hashlib.sha256(source.encode()).hexdigest(),
                      'sha256': hashlib.sha256(destination.read_bytes()).hexdigest(),
                      'derivation': '2D panels retained; 3D screenshots replaced by documentation viewers'}
            records['files'] = [r for r in records['files'] if r['file'] != record['file']]
            records['files'].append(record)
    provenance.write_text(json.dumps(records, indent=2) + '\n')


if __name__ == '__main__':
    main()
