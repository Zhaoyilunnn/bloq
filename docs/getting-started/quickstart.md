# Quick Start

In this guide, we walk through compiling two simple logical computations:
a static logical CNOT and a dynamic logical T gate. Each step is shown in
Python, Rust, and the `bloq` CLI; pick whichever you installed in
[Installation](installation.md). Both examples load built-in gallery graphs, so
you do not need to write any source yet.

## Static Logical CNOT

### Load the gallery graph

The `cnot` gallery entry implements a logical CNOT with control and target input
and output ports. Load it and print its BLOG definition:

::::{md-tab-set}
:::{md-tab-item} Python
```{literalinclude} ../examples/quickstart.py
:language: python
:start-after: "# [cnot-load-start]"
:end-before: "# [cnot-load-end]"
```
:::
:::{md-tab-item} Rust
```{literalinclude} ../examples/rust/quickstart.rs
:language: rust
:start-after: "// [cnot-load-start]"
:end-before: "// [cnot-load-end]"
:dedent: 4
```
:::
:::{md-tab-item} CLI
```sh
bloq gallery
bloq view --gallery cnot --html
```
The gallery command lists available entries; the viewer writes `cnot.html`.
The BLOG source is displayed below and can also be opened in the editor's gallery.
:::
::::

::::{container} bloq-example-layout
```{literalinclude} ../examples/quickstart-cnot.blog
:language: blog
```
:::{container} bloq-example-render
```{bloq-view} cnot
```
:::
::::

### Compile to Bloq IR

We then compile the block graph into Bloq IR format.

::::{md-tab-set}
:::{md-tab-item} Python
```{literalinclude} ../examples/quickstart.py
:language: python
:start-after: "# [cnot-compile-start]"
:end-before: "# [cnot-compile-end]"
```
:::
:::{md-tab-item} Rust
```{literalinclude} ../examples/rust/quickstart.rs
:language: rust
:start-after: "// [cnot-compile-start]"
:end-before: "// [cnot-compile-end]"
:dedent: 4
```
:::
:::{md-tab-item} CLI
```sh
bloq compile --gallery cnot -d 3 --backend ir-text -o cnot.bloqir
```
:::
::::

Bloq IR is a graph-based representation of a physical quantum program. Quantum
nodes place reusable circuit templates; classical nodes construct logical
readouts, decode measurements, and compute control values. Quantum, value, and
order edges describe their dependencies. The IR also retains detectors, logical
boundary operators, dynamic regions, and source provenance.

The same program can be saved as human-readable text (`.bloqir`) or compact binary
(`.bloq`). See [Inspect and exchange Bloq IR](../backends/ir.md) for its structure,
codecs, and inspection APIs. Here is the compiled CNOT in text form:

```{raw} html
<div class="bloq-output" role="region" aria-label="Compiled CNOT Bloq IR" tabindex="0">
```
```{literalinclude} ../examples/quickstart-cnot.bloqir
:language: text
```
```{raw} html
</div>
```

{download}`Download the CNOT IR <../examples/quickstart-cnot.bloqir>`.

### Visualize the IR graph

Export the graph as SVG, including its classical nodes:

::::{md-tab-set}
:::{md-tab-item} Python
```python
Path("cnot.svg").write_text(cnot_program.to_svg(include_classical=True), encoding="utf-8")
```
:::
:::{md-tab-item} Rust
```rust
std::fs::write("cnot.svg", cnot_program.to_svg(true))?;
```
:::
:::{md-tab-item} CLI
```sh
bloq view cnot.bloqir --svg --include-classical -o cnot.svg
```
:::
::::

```{raw} html
<div class="bloq-output bloq-ir-graph" role="region" aria-label="CNOT IR graph including classical nodes" tabindex="0">
```
```{image} ../assets/quickstart-cnot-ir.svg
:alt: Compiled CNOT dependency graph including quantum and classical nodes and nested regions.
```
```{raw} html
</div>
```

### Optional: emit to Stim

Bloq includes a default emitter from static Bloq IR to the Stim backend. It
translates physical operations and logical readouts into a circuit with detector
and observable annotations. See [Stim emission](../backends/emission.md#stim) for supported
IR constructs and emission options.

::::{md-tab-set}
:::{md-tab-item} Python
```{literalinclude} ../examples/quickstart.py
:language: python
:start-after: "# [cnot-emit-start]"
:end-before: "# [cnot-emit-end]"
```
:::
:::{md-tab-item} Rust
```{literalinclude} ../examples/rust/quickstart.rs
:language: rust
:start-after: "// [cnot-emit-start]"
:end-before: "// [cnot-emit-end]"
:dedent: 4
```
:::
:::{md-tab-item} CLI
```sh
bloq emit cnot.bloqir -o cnot.stim
```
:::
::::

The following is the emitted noiseless CNOT circuit:

```{raw} html
<div class="bloq-output" role="region" aria-label="Emitted CNOT Stim circuit" tabindex="0">
```
```{literalinclude} ../examples/quickstart-cnot.stim
:language: text
```
```{raw} html
</div>
```

{download}`Download the Stim circuit <../examples/quickstart-cnot.stim>`.

## Dynamic Logical T Gate

### Load the gallery graph

The `t_gate` implements a logical T gate using a cultivated T resource state
and gate teleportation. It demonstrates dynamic features: a repeat-until-success
cultivation protocol and measurement-basis selection conditioned on a logical
measurement result.

::::{md-tab-set}
:::{md-tab-item} Python
```{literalinclude} ../examples/quickstart.py
:language: python
:start-after: "# [t-load-start]"
:end-before: "# [t-load-end]"
```
:::
:::{md-tab-item} Rust
```{literalinclude} ../examples/rust/quickstart.rs
:language: rust
:start-after: "// [t-load-start]"
:end-before: "// [t-load-end]"
:dedent: 4
```
:::
:::{md-tab-item} CLI
```sh
bloq view --gallery t_gate --html
```
Open `t_gate.html` to inspect the gallery graph.
:::
::::

::::{container} bloq-example-layout
```{literalinclude} ../examples/quickstart-t.blog
:language: blog
```
:::{container} bloq-example-render
```{bloq-view} t_gate
```
:::
::::

### Compile to Bloq IR

::::{md-tab-set}
:::{md-tab-item} Python
```{literalinclude} ../examples/quickstart.py
:language: python
:start-after: "# [t-compile-start]"
:end-before: "# [t-compile-end]"
```
:::
:::{md-tab-item} Rust
```{literalinclude} ../examples/rust/quickstart.rs
:language: rust
:start-after: "// [t-compile-start]"
:end-before: "// [t-compile-end]"
:dedent: 4
```
:::
:::{md-tab-item} CLI
```sh
bloq compile --gallery t_gate -d 3 --backend ir-text -o t-gate.bloqir
```
:::
::::

The resulting IR retains runtime control alongside physical circuits:

```{raw} html
<div class="bloq-output" role="region" aria-label="Compiled dynamic T gate Bloq IR" tabindex="0">
```
```{literalinclude} ../examples/quickstart-t.bloqir
:language: text
```
```{raw} html
</div>
```

{download}`Download the T-gate IR <../examples/quickstart-t.bloqir>`.
The cultivation region uses `RepeatUntilSuccess`. Decoded `mzz` controls guarded
quantum instances for the selective X/Y measurement, and classical dependencies
carry corrected logical readouts. Inside the region, a `Compute` node ORs the
two observables' `Flip` outputs and supplies the explicit restart predicate.
Both decoder predictions must be false for success; physical postselection
failures also retry.

### Visualize the IR graph

::::{md-tab-set}
:::{md-tab-item} Python
```python
Path("t-gate.svg").write_text(t_program.to_svg(include_classical=True), encoding="utf-8")
```
:::
:::{md-tab-item} Rust
```rust
std::fs::write("t-gate.svg", t_program.to_svg(true))?;
```
:::
:::{md-tab-item} CLI
```sh
bloq view t-gate.bloqir --svg --include-classical -o t-gate.svg
```
:::
::::

```{raw} html
<div class="bloq-output bloq-ir-graph" role="region" aria-label="T Gate IR graph including classical nodes" tabindex="0">
```
```{image} ../assets/quickstart-t-ir.svg
:alt: Compiled T Gate dependency graph including quantum and classical nodes and nested regions.
```
```{raw} html
</div>
```

See the [Bloq IR chapter](../backends/ir.md) of the User Guide for node kinds,
output frames, observable composition, and retry conditions.

## Next steps

- Read the [User Guide](../user-guide.md) to learn the basic concepts and how
  compilation works.
- Explore the [Examples](../gallery/index.md) to see built-in computations and
  their block graphs in [BLOG format](../graphs/blog.md).
- Consult the references for the complete [Python API](../api/python.rst),
  [Rust API](../api/rust.md), and [CLI](../reference/cli.md).
