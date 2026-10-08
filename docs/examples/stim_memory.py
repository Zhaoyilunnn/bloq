"""Emit an X-memory circuit and check its noiseless detectors with Stim."""

import bloq

# [example-start]
program = bloq.compile(bloq.GalleryItem.X_MEMORY.load(), distance=3)
clean = bloq.emit_stim(program)
samples = clean.compile_detector_sampler(seed=7).sample(shots=32)
assert clean.num_detectors > 0
assert not samples.any()

noisy = bloq.emit_stim(program, noise=0.001)
noisy.to_file("memory.stim")
model = noisy.detector_error_model(decompose_errors=True)
assert model.num_detectors == noisy.num_detectors
print(f"qubits={noisy.num_qubits}, detectors={noisy.num_detectors}")
print(f"shots={samples.shape[0]}, noiseless detector events={int(samples.sum())}")
# [example-end]
