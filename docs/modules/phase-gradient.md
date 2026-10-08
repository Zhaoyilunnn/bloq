# Phase-Gradient Rotations

The `phase_gradient` gallery entry composes signed single-qubit rotations from
T-resource injection stages. It shows how a small family of reusable modules
can form a longer adaptive computation. Start with [modules](index.md) and
[T Block](../circuit-constructions.md#t-block).

:::{note}
The gallery's `k=4` label names a fixed single-qubit rotation construction. It
does not describe a four-qubit phase-gradient register or expose a general
rotation-synthesis API. Its source contains 22 signed $\pi/4$ rotations,
followed by a Hadamard.
:::

::::{md-tab-set}
:::{md-tab-item} Block graph
```{bloq-view} phase_gradient
```
:::
:::{md-tab-item} Modules
```{bloq-view} phase_gradient
:modules:
```
:::
::::

## What the source implements

Let $R_P(\theta)=\exp(-i\theta P/2)$. In time order, each row below applies
$R_X(a\pi/4)$ followed by $R_Z(b\pi/4)$:

| Pair | $a$ | $b$ |
| --- | --- | --- |
| 0 | +1 | +1 |
| 1 | +1 | +1 |
| 2 | −1 | −1 |
| 3 | −1 | −1 |
| 4 | −1 | +1 |
| 5 | −1 | −1 |
| 6 | +1 | −1 |
| 7 | −1 | +1 |
| 8 | −1 | +1 |
| 9 | +1 | −1 |
| 10 | +1 | +1 |

A final $H$ completes the map. This is the exact gate sequence checked by
Bloq's independent logical-map test, up to global phase. The example does not
attach an approximation-error bound to a different target rotation.

Four reusable definitions cover the two axes and rotation signs:

| Definition | Logical role | Transverse resource arm |
| --- | --- | --- |
| `RxPlusAtYPlus` | $R_X(+\pi/4)$ | Local `+Y` |
| `RxMinusAtYPlus` | $R_X(-\pi/4)$ | Local `+Y` |
| `RzPlusAtXPlus` | $R_Z(+\pi/4)$ | Local `+X` |
| `RzMinusAtXPlus` | $R_Z(-\pi/4)$ | Local `+X` |

Each definition has temporal `q_in` / `q_out` data ports and one T source. Here
is the positive X rotation:

```{literalinclude} ../../bloq_graph/assets/phase_gradient_k4.blog
:language: text
:start-at: module RxPlusAtYPlus
:end-before: module RxMinusAtYPlus
```

`m` is a named merge readout. `resolve` chooses the ordered measurement basis,
and `feedback` applies the declared correction. The positive and negative
definitions differ in their selective cap and, for X rotations, their feedback
condition. Substituting one for the other changes the signed operation.

## Place the instances

The root alternates resource arms around the data wire: `+Y`, `+X`, `−Y`, `−X`.
A Z half-turn moves a definition's arm to the opposite side while keeping its
time direction. This allows nearby instances without overlapping their arms.

```{literalinclude} ../../bloq_graph/assets/phase_gradient_k4.blog
:language: text
:start-at: s00: RxPlusAtYPlus
:end-at: s07: RzMinusAtXPlus
```

```{mermaid}
flowchart LR
  q["q_in"] --> rx0["s00: +Rx"] --> rz0["s01: +Rz"]
  rz0 --> rx1["s02: +Rx, rotate Z 180"] --> rz1["s03: +Rz, rotate Z 180"]
  rz1 --> rest["s04 … s21"] --> h["Hadamard seam"] --> output["q_out"]
```

All 22 stages connect explicitly. The last stage uses a Hadamard pipe into a
parent-owned cube, followed by the output Port:

```text
s20.q_out -> s21.q_in
s21.q_out -H> 1
1 -> 2
```

The full hierarchy has five definitions, including `main`. It contains 21
direct child-to-child seams. The final Hadamard is a connection; there is no
extra `FinalHadamard` module.

## Load and compile

Compile at distance three with ordinary T cultivation, then save text Bloq IR:

::::{md-tab-set}
:::{md-tab-item} Python
```python
from pathlib import Path
import bloq

example = bloq.GalleryItem.PHASE_GRADIENT
Path("phase-gradient.blog").write_text(example.source(), encoding="utf-8")
graph = example.load()
flat = graph.flatten()
assert flat.t_count == 22
assert flat.selective_count == 22
program = bloq.compile(graph, distance=3)
program.validate()
program.save("phase-gradient.bloqir")
```
:::
:::{md-tab-item} Rust
```rust
use bloq::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = GalleryItem::PhaseGradientK4.build();
    assert_eq!(source.modules().count(), 5);
    assert_eq!(source.instances.len(), 22);
    std::fs::write("phase-gradient.blog", source.to_blog_text())?;
    let program = compile(&source, 3)?;
    program.validate()?;
    std::fs::write("phase-gradient.bloqir", program.to_text())?;
    Ok(())
}
```
:::
::::

The named readouts remain output-safe after composition; the compiler derives terminal
corrections from the same signed relation.

CLI:

```bash
bloq compile --gallery phase_gradient -d 3 --backend ir-text -o phase-gradient.bloqir
bloq validate phase-gradient.bloqir
```

Download {download}`the full BLOG source
<../../bloq_graph/assets/phase_gradient_k4.blog>` to modify placements or open it
in the [editor](https://bloqec.com/editor/).

## Verification and proxies

The result retains T-source retries, selected measurements, and feed-forward.
Use the [verification VM](../backends/vm.md) to simulate those dependencies.
The T states here come from the compiled cultivation regions. They are not
assumed to arrive through external prepared-state ports.

For a Clifford distance-analysis circuit, the CLI supports a reproducible proxy:

```bash
bloq compile --gallery phase_gradient -d 3 --clifford-proxy --proxy-seed 7 -o phase-gradient-proxy.stim
```

:::{important}
A [Clifford proxy](../backends/emission.md#stim) replaces T resources with ideal ports
and jointly selects a reachable measurement path. It is a distance-analysis
stand-in, not an execution of the rotation channel or a measurement of its
logical error rate.
:::
