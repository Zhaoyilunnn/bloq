# CCZ gate teleportation

Three multiplex data wires couple to an externally supplied CCZ state. Three named joint measurements choose explicit correction branches, and pairs of outcomes control the remaining Pauli feedback.

:::{caution}
This computation contains spatial Hadamard pipes. The current construction can
reduce the effective circuit distance below the requested code distance.
Check the emitted circuit's distance before relying on it. Prefer temporal
Hadamards or a redesigned layout when preserving circuit distance is required.
See [the distance boundary](../circuit-constructions.md#distance-boundary).
:::

## Block graph

```{bloq-view} ccz_gate_teleport
```

## BLOG source

```{gallery-blog} ccz_gate_teleport
```

See [BLOG Format](../graphs/blog.md) for the syntax or return to
[all examples](index.md).
