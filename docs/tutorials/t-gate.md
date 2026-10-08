# Logical T Gate

This tutorial shows how to compile and simulate a logical T gate with Bloq.
The gate uses a cultivated T state and measurement-dependent basis selection.

## Load and inspect the source

```{literalinclude} ../examples/tutorial_t_gate.py
:language: python
:start-after: "# [source-start]"
:end-before: "# [source-end]"
```

```{bloq-view} t_gate
```

The data input and output are open Ports. The corrected joint measurement
`mzz` selects the resource’s final X or Y measurement.

```{mermaid}
flowchart LR
  source["T resource: cultivate until accepted"] --> merge["Joint logical measurement"]
  data["Data input"] --> merge
  merge --> bit["Corrected mzz"] --> cap["Select X/Y measurement"]
  merge --> output["Data output + Pauli frame"]
  cap --> output
```

See [Actions](../graphs/actions.md) for the selection rule and
[T State Cultivation](t-cultivation.md) for resource preparation.

## Compile the dynamic IR

```{literalinclude} ../examples/tutorial_t_gate.py
:language: python
:start-after: "# [compile-start]"
:end-before: "# [compile-end]"
```

| Part of the IR | Behavior |
| --- | --- |
| `RepeatUntilSuccess` | Retry cultivation on physical postselection failure or either observable’s decoder `Flip` |
| Selected measurement | Execute the chosen X/Y arm and publish only its records |
| Corrected observables and output frame | Carry measurement results and Pauli corrections to the data output |

Only an accepted resource attempt exports results. Whole-program Stim emission
cannot represent this adaptive protocol. A Clifford proxy replaces the T
resource, so it does not simulate the T channel.

## T-source Proxy

To speed up noisy simulation, replace the physical cultivation circuit with an
ideal logical $|T\rangle$ state, then apply a logical Z error with probability
$q$. Calibrate $q$ from the infidelity of an accepted cultivated state, using
the chosen distance, physical noise strength, and GAP thresholds from
[T State Cultivation](t-cultivation.md#cultivated-state-calibration).

| Part | Model |
| --- | --- |
| Accepted T source | Ideal $\lvert T\rangle$ with a logical Z error of probability $q$ |
| Remaining gate circuit | Physical circuit noise of strength $p$ |
| Source acceptance | Account separately for the calibrated cultivation acceptance probability |

This proxy retains the non-Clifford T state. Compare it with physical-source
simulations for the output metric of interest, as shown below. Matching
accepted-state infidelity alone does not establish equality of the full noise
channels.

## Offline decoding and path-based postselection

For these noisy experiments, we do not yet have a fast dynamic simulator or a
real-time decoder. Instead, simulate each measurement path as a fixed circuit,
then decode its records offline. Keep a shot when the corrected measurement
results select the path that was simulated. This reproduces adaptive basis
selection through postselection.

| Step | Action |
| --- | --- |
| Sample paths | Run both X/Y measurement choices with the same number of attempts |
| Decode records | Use PyMatching to correct the branch selector and output Pauli frame |
| Select shots | Keep matching paths whose cultivation checks and source GAP thresholds pass |
| Score outputs | Apply the frame to the logical expectations and compute infidelity |
| Combine paths | Normalize each path's accepted count and error sum by its attempted shots |

A path mismatch is a sampling filter, not a physical failure. For two equally
sampled paths, effective attempts are half the total path shots. Average the six
input-state infidelities equally, and report the Choi check separately.

## Simulation

The noisy simulation tests six inputs (`0`, `1`, `+`, `-`, `+i`, `-i`) and an
independent Choi check. It uses offline decoding and postselection, with ten
memory rounds before basis selection and ten before output.

Download {download}`the script <../examples/tutorial_t_gate_simulation.py>` and
{download}`the circuit bundle <../assets/experiments/t-gate-characterization-d9.zip>`
into the same directory.

Use Python 3.12 or newer with the GAP-enabled PyMatching build:

```sh
uv add "clifft>=0.6,<1" "pymatching @ git+https://github.com/inmzhang/PyMatching.git@de4bb3e0796c1c9873d3ed5d1704364db10592cb"
uv run python tutorial_t_gate_simulation.py
```

The default uses 1,000 attempts per path to check the workflow. Increase
`--shots` to resolve the quoted rare-error rates. The script uses Clifft to
simulate the retained circuits.

```{literalinclude} ../examples/tutorial_t_gate_simulation.py
:language: python
```

```{figure} ../assets/experiments/t-gate-characterization.svg
:alt: Accepted output infidelity for six logical inputs, their mean, and an independent Choi check, comparing the physical cultivated source with its calibrated proxy at distance 9 and physical noise strength 0.001. Error bars are nominal 95% half-widths.
```

Continue with [8T-1CCZ Distillation Factory](ccz-factory.md) for a larger adaptive
resource protocol, or [Emulate Hardware Synchronization](../backends/thth.md) for a THTH example with explicit timing and protected waits.
