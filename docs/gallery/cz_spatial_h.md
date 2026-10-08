# CZ with a spatial Hadamard

A Hadamard on a spatial connection changes the boundary basis at the interaction. The two input wires remain available at the outputs after the controlled Z operation.

:::{caution}
This computation contains spatial Hadamard pipes. The current construction can
reduce the effective circuit distance below the requested code distance.
Check the emitted circuit's distance before relying on it. Prefer temporal
Hadamards or a redesigned layout when preserving circuit distance is required.
See [the distance boundary](../circuit-constructions.md#distance-boundary).
:::

## Block graph

```{bloq-view} cz_spatial_h
```

## BLOG source

```{gallery-blog} cz_spatial_h
```

See [BLOG Format](../graphs/blog.md) for the syntax or return to
[all examples](index.md).
