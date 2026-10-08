"""Check the construction examples' physical boundary and growth operations.

Run: uv run --project bloq_py --no-sync --with stim python tools/test_construction_slices.py
"""

from render_construction_slices import compiled_stages, examples


def count_targets(circuit, stage, gates):
    tick = total = 0
    for op in circuit.flattened():
        if op.name == "TICK":
            tick += 1
        elif stage["tick_start"] <= tick < stage["tick_stop"] and op.name in gates:
            total += len(op.targets_copy())
    return total


def main():
    cases = [case for case in examples() if case.family == "regular"]
    assert len(cases) == 48
    distance = 5
    for start in range(0, len(cases), 4):
        counts = []
        for case in cases[start:start + 4]:
            circuit, stages, _, _ = compiled_stages(case, distance)
            # Reject non-deterministic detector or observable parities.
            circuit.detector_error_model()
            counts.append((count_targets(circuit, stages[0], {"R", "RX", "RY"}),
                           count_targets(circuit, stages[-1], {"M", "MX", "MY"})))
        prepared, measured = counts[0]
        # Temporal input/output removes only the central patch's data collapse;
        # ancilla and spatial seam operations remain in the complete component.
        assert counts == [(prepared, measured),
                          (prepared - distance**2, measured),
                          (prepared, measured - distance**2),
                          (prepared - distance**2, measured - distance**2)], cases[start].name
    print("48 regular-cube circuits: deterministic parities and temporal boundary operations checked")

    for case in [case for case in examples() if case.family == "measurement"]:
        circuit, stages, _, _ = compiled_stages(case, distance)
        circuit.detector_error_model()
        closing = stages[0]
        readout = {"tick_start": closing["tick_stop"] - 1, "tick_stop": closing["tick_stop"]}
        assert count_targets(circuit, closing, {"R", "RX"}) == distance**2 - 1
        assert count_targets(circuit, closing, {"M", "MX"}) == 2 * distance**2 - 1
        assert count_targets(circuit, readout, {"M", "MX"}) == 2 * distance**2 - 1
    print("4 X/Z readouts: ancillas and data measured together at the end of one closing round")

    for case in [case for case in examples() if case.family == "t"]:
        assert case.distance is not None and case.distance > 5
        circuit, stages, _, _ = compiled_stages(case, case.distance)
        circuit.detector_error_model()
        growth, stabilization = stages[-2:]
        ancillas = count_targets(circuit, stabilization, {"R", "RX", "RY"})
        assert ancillas == case.distance**2 - 1
        new_data = count_targets(circuit, growth, {"R", "RX", "RY"}) - ancillas
        assert new_data == case.distance**2 - 5**2, case.name
    print("T examples: distance-five to distance-eleven growth initializes 96 new data qubits")


if __name__ == "__main__":
    main()
