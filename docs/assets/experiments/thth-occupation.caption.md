**Hardware-oriented execution from Bloq IR.** Logical-site occupation for one illustrative noisy THTH shot at `d=9`; time runs downward. The two `|T⟩`-state cultivations start at `t=0`, while the live input and cube work arrive later. Hatches distinguish synchronization, dependency-wait, and decoder-latency QEC; hollow side symbols distinguish physical idle reasons. Inset A shows the native THTH block graph with diagram time increasing upward. Inset B maps logical-site IDs to source-lattice coordinates. Seed `94` finishes at `t=410` after 3 factory attempts (1 detector rejection, 0 GAP rejection, 2 accepts). Rectangles denote logical-site reservation, not physical-qubit utilization. Cultivation bars summarize cultivation and escape, including local ancilla work; column widths do not encode physical-qubit area.

### Reproducibility and audit

- Uniform physical and idle noise: `p=0.0005`; gate duration `1`; decoder latency `10` rounds. Mock decoder: acceptance `0.8`, accepted accuracy `0.999`, rejected accuracy `0.9`.
- Boundary arrows: Input marks the event-derived logical arrival at S10, `t=150`. Output marks the zero-duration ideal-boundary handoff at S02, `t=410`; the horizontal cap is the terminal cross-section, not added occupation time.
- Idle side markers sit at each interval's true midpoint: hollow diamonds identify synchronization and hollow circles identify decoder latency. Symbol size does not encode duration.
- Factory attempts: S04: detector rejections=1, GAP rejections=0, accepted body end `t=105`, accepted `t=165`; S11: detector rejections=0, GAP rejections=0, accepted body end `t=82`, accepted `t=142`.
- Merged cube groups reserve all authored origin sites even if one moment touches a subset. Full `(x,y,z)` members are retained in the CSV.
- Engine: `ticit`. Bloq commit: `74216bf3d59ab9fbb5686719d43d937247beb564+working-tree`. Bloq version: `0.1.0`. Trace SHA-256: `85fa7ad24567e766516ee709597951036dd6bf035271e450a2598ad2af2c250a`.
- Structured wait intervals: synchronization=37, dependency wait=13, decoder latency=21.

- Working-tree compiler source SHA-256: `74911df3bdfa172c130ceb5aecbe83bf0af889549c911e9fe5cc88275f09476a`. Renderer SHA-256: `1e663f7030a1341565ee537f23c810a97dac7788e841840060b2da06d54026c5`.
