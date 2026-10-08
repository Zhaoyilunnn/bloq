# Patch Alignment

A block graph specifies the boundary bases of each patch and how blocks and
pipes connect. It leaves the physical placement of the stabilizers to the
compiler. Before constructing circuits, we need a shared convention for the
bulk $X/Z$ checkerboard and the weight-two stabilizers along patch boundaries.
This **patch alignment** makes neighboring patches compatible on the same
qubit lattice.

This chapter introduces the alignment conventions, explains why Bloq uses
fixed bulk alignment, and gives the coordinates used by the
[Circuit Constructions](circuit-constructions.md).

## Two alignment conventions

An ordinary rotated surface code patch has weight-four stabilizers in its
interior and weight-two stabilizers on its boundaries. Opposite boundaries
share a basis, and adjacent boundaries have opposite bases. Exchanging the
boundary bases therefore raises a physical design choice: keep the stabilizer
supports in place and exchange their bases, or keep the bulk checkerboard and
move the boundary stabilizers.

```{figure} assets/paper/patch-alignment-conventions.svg
:alt: Three distance-five patches: fixed bulk with a top X boundary, fixed boundary with a top X boundary, and their common top Z boundary reference. Red plaquettes are X stabilizers and blue plaquettes are Z stabilizers.
:width: 100%

Patch alignment conventions. (a) Fixed bulk alignment with
a top $X$ boundary. (b) Fixed boundary alignment with a top $X$ boundary.
(c) A top $Z$ boundary, for which the two illustrated conventions coincide.
Comparing (a) with (c) keeps the bulk coloring and moves the boundary
stabilizers. Comparing (b) with (c) keeps the stabilizer supports and exchanges
their bases. Red denotes $X$ stabilizers; blue denotes $Z$ stabilizers.
```

With **fixed boundary alignment**, stabilizer supports stay in place when
$X$ and $Z$ are exchanged. A transversal Hadamard exchanges their bases without
requiring boundary realignment. However, neighboring patches can then have
different bulk checkerboards. Joining them in a spatial junction can require
stretched stabilizers to match the two patterns.

With **fixed bulk alignment**, each bulk plaquette's basis is determined by
its position on the lattice. Changing the boundary bases moves the weight-two
boundary stabilizers while leaving the interior coloring unchanged. Spatially
joined patches share a checkerboard, but Hadamard interfaces need additional
circuit design to preserve that convention.

| Convention | What stays fixed when boundary bases exchange? | Circuit consequence |
| --- | --- | --- |
| Fixed bulk | Bulk checkerboard | Ordinary spatial junctions share a checkerboard; Hadamard interfaces need realignment or domain-wall circuits |
| Fixed boundary | Stabilizer supports | Transversal Hadamards preserve supports; some spatial junctions need stretched stabilizers |

## Why Bloq uses fixed bulk

Bloq's current circuit realization uses fixed bulk alignment, giving ordinary
lattice-surgery junctions a shared checkerboard. The additional work belongs
to the Hadamard constructions:

- A [temporal Hadamard](circuit-constructions.md#temporal-hadamard) realigns the
  stabilizer supports before applying transversal Hadamard gates. The basis
  exchange then restores the global checkerboard.
- A [spatial Hadamard](circuit-constructions.md#spatial-hadamard) joins patches
  across a basis-exchanging domain wall using extended stabilizer checks.

:::{note}
The block compiler architecture supports adding other patch alignment
conventions and circuit techniques, such as diagonal hook alignment. Fixed bulk
is the current choice because its circuit constructions are more mature and
easier to implement. We expect to extend Bloq with selectable alignment
conventions and circuit realizations in future versions.
:::

(one-global-checkerboard)=
## Coordinates and spacing

Bloq places patches compactly, leaving one row or column of data qubit sites
between neighboring patches. Physical coordinates use a doubled square lattice,
with $y$ increasing upward. For a standard distance-$d$ patch:

| Quantity | Convention |
| --- | --- |
| Data qubits | Both coordinates odd, from 1 through $2d-1$ locally |
| Stabilizer ancillas | Both coordinates even |
| Full footprint, including boundary ancillas | $(2d+1)\times(2d+1)$ lattice sites |
| Neighboring block-cell offsets | $2d+2$ along either spatial axis |
| Surgery seam | One intervening row or column of data sites |

The footprint counts lattice sites, not occupied qubits. An ordinary isolated
patch contains $d^2$ data qubits and $d^2-1$ stabilizer ancillas; boundary sites
without stabilizers remain unoccupied.

For example, at $d=5$ the local data coordinates are $1,3,5,7,9$ on each axis.
The full footprint runs from 0 through 10, and the next block cell starts at
offset 12. Seam data can occupy the intervening coordinate 11 when the patches
join.

```{mermaid}
flowchart LR
  left["Patch A<br/>local sites 0…2d"] --- seam["Seam data<br/>2d+1"]
  seam --- right["Patch B<br/>offset 2d+2"]
```

The same coordinates define one global checkerboard. For an ancilla at
global position $(x,y)$, the bulk basis is

$$
P(x,y)=
\begin{cases}
X & (x+y)/2\text{ is even},\\
Z & (x+y)/2\text{ is odd}.
\end{cases}
$$

Both ancilla coordinates are even, so $(x+y)/2$ is an integer. At Bloq's
supported odd distances, translating by one block-cell offset $2d+2$ changes
this integer by $d+1$, which is even. Neighboring block cells therefore retain
the same checkerboard phase. The comparison figure above uses a translated,
vertically reflected drawing frame; the formula above defines Bloq's physical
coordinates.
