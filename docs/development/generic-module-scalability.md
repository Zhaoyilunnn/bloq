# Generic module compilation: evidence and claim boundary

Bloq uses one generic composed-surface planner. The adder-specific factor chain,
finite banks, translated-tile dispatch and runtime strategy selector are removed.
Default benchmark widths are 3/8/16/33; larger explicit widths remain supported
for scaling inspection. Generated results, rejected prototypes and frozen
source/binary provenance stay local.

## What a paper can claim

A supported empirical statement is:

> Bloq compiles hierarchical adaptive block graphs using shared physical
> templates and complete signed readout fragments. On the tested controlled-adder
> family at distance 3, physical template count remains 90 through 128 bits.
> The 128-bit instance compiles in a median 6.26 seconds with 2.15 GB peak RSS
> on an Apple M5 Pro running macOS 27.0.1.

Those numbers describe this workload and host. They are not a proof of linear
compilation, bounded memory per module, or near-linear end-to-end scaling.
Calling the source a composition of small modules does not establish any of
those properties: the current planner still constructs graph-wide source rows
and explicit readout equations. A warm complete-program cache hit is also not
evidence for scaling changed roots.

The defensible modularity claim concerns physical template reuse and the
hierarchical authoring interface. Fixed-topology definition objects can also be
reused across roots; the general guarded route still certifies child definitions
on a root cache miss and plans the composed root's readouts globally.

## Measurements

Three alternating fresh-process samples per build used d3, matching empty-feature
release builds, distinct Cargo targets, frozen source/binary hashes, default
compiler limits and a quiet compiler host. Source generation was outside timing.
The final 128-bit comparison was:

| Implementation | Compile | Physical binding | Peak RSS |
| --- | ---: | ---: | ---: |
| Shared module fragments | 6.660 s | 3.698 s | 2.571 GB |
| Plus query retirement | 6.338 s | 3.335 s | 2.154 GB |
| Plus future-use cache admission | 6.257 s | 3.288 s | 2.149 GB |

The combined lifetime changes reduce time 6.0%, binding 11.1% and peak RSS 16.4%
in that pass. An earlier 64/96/128 retirement sweep took 1.529/3.981/6.322 seconds.
Doubling width from 64 to 128 increased time 4.13 times. This is roughly quadratic
behavior over the measured range, not an asymptotic theorem. RSS varies between
passes; the percentages are observations rather than guaranteed savings.

A fresh structural diagnostic counted stored IR without serializing it:

| Bits | Templates | Template instances | IR nodes | IR edges | Readout term occurrences |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 33 | 90 | 7,474 | 44,659 | 172,184 | 313,973 |
| 64 | 90 | 14,573 | 185,771 | 717,257 | 1,293,635 |
| 96 | 90 | 21,901 | 440,299 | 1,699,241 | 3,071,987 |
| 128 | 90 | 29,229 | 620,835 | 2,416,074 | 3,934,967 |

Readout terms count record occurrences and boundary bindings per classical
invocation. The later [shared-storage implementation](classical-storage.md)
stores equal definitions once; this table retains invocation counts for comparison.
IR edges and terms grow more than threefold when width doubles from 64
to 128. An emitter must pay for its explicit output, but these measurements do
not prove the chosen representation is minimal or that compilation is
output-optimal. A scalable design must address output expansion as well as
intermediate algebra.

The source diagnostic also finds 639/959/1,279 complete queries and
839,291/1,958,203/2,905,010 total support-site visits at 64/96/128 bits. At 128,
composition takes about 0.11 s, causal named planning 1.08 s, terminal pivots
0.52 s, and normalization/reconstruction 0.42 s. A separate binding diagnostic
spends about 0.93 s in local cases, 0.90 s in Boolean collection, 0.52 s in packet
emission and 0.06 s in gateway resolution. These phase observations include
instrumentation and are not independent cold-run medians.

## Classical calls and sparse readout data

AND/XOR/Select evaluation already happens at runtime. The compilation bottleneck
includes deriving their graph-wide coefficient functions and materializing the
resulting readout/guard graph. Calls help when they preserve a compact program
for that derivation or application, rather than merely renaming existing gates.

Compact classical programs are a promising next representation. Ordinary calls
around the existing expressions, however, do not remove their argument edges.
The useful unit is a shared program with local record/operator tables and
independently available readout rows. This can extend the existing readout-recipe
semantics without introducing an author-facing language or arbitrary Rust callbacks.

A structural probe compiled 3/33/64/96/128-bit adders, CCZ teleportation, the
phase-gradient gallery, T and spatial-H CZ at d3. It interned exact ordered
payloads, then split record lists at instance boundaries and interned local
patterns by `(template, ordered measurement indices)`. Every chunk was
reconstructed and compared with its original, including order and occurrences.
This is a storage diagnostic, not an implemented compiler or VM speedup.

| Bits | Compute nodes | Stored record terms | Terms in unique instance chunks | Chunk uses |
| ---: | ---: | ---: | ---: | ---: |
| 33 | 27,834 | 309,250 | 50,290 | 81,596 |
| 64 | 122,570 | 1,274,652 | 98,185 | 344,445 |
| 96 | 294,954 | 3,027,196 | 147,625 | 824,429 |
| 128 | 436,269 | 3,879,825 | 197,074 | 1,052,008 |

The larger adders all use just **183 template-local record patterns containing
725 indices**. Instance-local pooling removes 94.9% of repeated record terms
at 128 bits; owner/pattern references and row uses still need storage. Whole-row
pooling alone retains 594,195 record terms plus 3,690 boundary-operator terms,
versus 3,934,967 total terms originally. No pooling aliases different physical
instances or cuts, and none permits moving a record read across activation.
Combining whole-packet and local-chunk pools leaves 124,007 packet-use references,
9,908 distinct packet rows, 145,575 chunk references inside those rows, and
44,625 distinct owner/chunk pairs at 128 bits. This is a concrete compact
encoding opportunity, although guards and all scheduling metadata remain.
The phase-gradient case also has reusable local patterns: 17 patterns contain
58 indices, versus 887 original record occurrences. These small non-adder cases
check representation coverage, not scaling across independent workload families.

The larger problem is guard representation. At 128 bits, Compute accounts for
71.6% of classical nodes, with 1,281,012 incoming value edges. Its expressions
have only 36 exact slot-local shapes (399 expression nodes and 268 input
occurrences), but each occurrence binds different arguments. Pooling those
shapes saves expression storage; calling one of 36 functions per Compute still
leaves 436,269 calls and their input edges. Also, only 12,066 Computes are
syntactically affine. A fixed GF(2) matrix does not directly represent the
remaining AND/OR/Select expressions; proving more functions affine would be a
separate optimization.

Three levels of change have different consequences:

| Representation | Benefit | Remaining cost |
| --- | --- | --- |
| Intern local payloads and expression shapes | Removes repeated data while retaining current execution | Same calls, guards and dependency graph |
| Shared guard program plus sparse readout rows | Moves internal graph nodes/edges into compact typed arrays; enables reuse during execution | Still proportional to stored instructions and row entries |
| Composed local transfer programs retained without expansion | Can avoid materializing graph-wide coefficients and repeated query uses | Requires bounded interfaces/fronts and exact causal/signed composition |

Sparse storage costs proportional to rows plus stored entries; it does not make
quadratically many entries linear. The distinction is visible here: unique
instance-chunk payload grows about twofold from 64 to 128 bits, while chunk uses
grow 3.05-fold and Compute count 3.56-fold. Shared complete rows or sub-recipes can
reduce repeated uses further, but that must be measured rather than assumed.

For example, all prefix parities have a triangular expanded matrix with
`n(n+1)/2` entries, but `s[i+1] = s[i] XOR x[i]` uses a constant-size local update
and O(n) calls. The gain comes from retaining the recurrence, not from spelling
the triangular matrix as a sparse function argument. A module implementation
needs an analogous factorization of its actual signed relation. Small source
modules alone do not prove that their composed classical state remains small.

### Smallest sound kernel contract

Prefer an immutable pool owned by the compiled Bloq IR, with references from
existing Compute/readout concepts. Keep local record patterns, boundary
operators, guard expressions and row-composition references as typed data.
Use sparse GF(2) rows for parity, a shared Boolean program for guards, and
explicit signed boundary composition. A Rust interpreter can execute this data;
a host-function pointer or a new general-purpose DSL is unnecessary.

- Each row keeps its own activation, readiness dependencies, fold roles, owner
  bindings and raw/corrected observable identity. Complete Observable nodes
  remain the decoder boundaries. Sharing data does not merge decoder requests.
- Required inputs are distinct from nonzero coefficients. For example,
  `x XOR x` is zero when x is known but remains unavailable when x is missing;
  deleting the matrix column must not delete that requirement. Select likewise
  retains the unselected operand's availability. Inactive reads still obey
  current scheduling dependencies, then produce zero/empty bindings without
  accessing records or decoding.
- Calls expose results per causal row or bounded availability frontier. A single
  atomic whole-program call that waits for every record can deadlock feedback:
  an early output may control the quantum work producing a later input.
  Execution caches belong to the shot/attempt and cannot survive a retry as
  already-computed values.
- Preserve physical instance/face identities and exact signed operator products.
  Crossing reconstruction consumes a complete local coefficient tuple; separately
  binding and XORing arbitrary source leaves is unsound. A plain Boolean matrix
  alone does not encode this operation.
- Retain the compact representation through VM preparation, scheduling, pinning,
  codecs, slicing and memory edits. An unconditional lowering back to scalar
  nodes merely moves expansion to another phase. Compilation must emit the
  program directly to reduce its own peak allocation and construction cost.

Keep compile-time checks for source validity, complete signed relations, output
safety and causal feasibility. Runtime can evaluate certified coefficient
programs, apply local response tables, fold actual records and materialize a
readout's physical binding when its inputs become available. Moving symbolic
solves or branch searches to every shot is a different tradeoff: it can reduce
ahead-of-time work while increasing runtime latency, and is not a bounded
compiler result by itself.

This separation has precedents rather than requiring a general language:
MLIR documents [explicit function interfaces](https://mlir.llvm.org/docs/Dialects/Func/),
[sparse row/block encodings](https://mlir.llvm.org/docs/Dialects/SparseTensorOps/),
and [separate value/readiness dependencies](https://mlir.llvm.org/docs/Dialects/AsyncDialect/).
These are design references, not evidence of a speedup in Bloq.

The evidence supports a compact guarded-readout program experiment, beginning
with exact pooled local payloads and independent query roots. It does not yet
justify a general classical-function language or a paper claim of bounded
end-to-end compilation. Acceptance must include total program-table entries,
peak compilation/VM-preparation memory, per-shot work and latency, as well as
visible IR node/edge counts. The source-algebra changes below remain necessary.

## Retained implementation

- Connection validation indexes child interface ports once per used definition.
- Fixed-definition compilation checks cache hits before expanding missing child
  geometry. The root geometry is linked once for placement.
- Complete queries are partitioned by authored instance. Identical fragments
  share every native coefficient, activation, gateway role and actual physical
  columns. Private directions, signed reconstruction and named provenance remain
  part of the original complete relation.
- Physical binding reuses complete packets, boundary operators and transport
  offsets, then cancels contributions before emitting each decoder equation.
  Optional images are discarded before joint Boolean remapping.
- Coefficients are released after their final direct or fragment use. Activation,
  feedback and corrected-frame folds remain for assembly. Cache admission keeps
  only fragments with a future consumer. Spent budgets are never refunded.
- After ordinary payload merging, repeated ordered readout inputs share existing
  unindexed `Observable` fragments when this strictly reduces edges. Each complete Observable
  retains its identity and decoder request.

The query interner and response caches retain bounded optional payloads; an
oversized response still uses the same exact planner and cumulative budget.
No adder recognition, runtime whole-root solve or new classical-function IR is
needed for these changes.

## Rejected experiments

| Experiment | Finding |
| --- | --- |
| Late terminal deferral | Leaves were already graph-wide. At 128 bits it took 7.03 s versus 6.83 s for shared fragments and used more memory. |
| Early seam tags with local witness leaves | Preserved complete raw rows and metadata after fixing off-activation tag deletion, but root reconstruction retained the old readout-planning cost. Selected seam pivots still grew to 129 columns at 128 bits. |
| One-site query fragments | Filled the interner and retained 1,633,292 fragment entries at 128 bits. The screen regressed from 7.25 s to 7.73 s. |
| Lazy named-candidate admission | Stopped coefficient reconstruction on rejection, but 128-bit compilation stayed at 6.96 s in a paired comparison. Dropped because the dominant expansion remained. |
| Composed terminal binding | Exact streamed support and complete local tuple queries preserved the checked IR. Individual probes slowed the 64-bit adder from 1.57 to 9.71 s. Batches of 32 sites and shared feedback projections reduced this to 2.73 s against a 1.60 s baseline, still a regression. |
| Native leaves through static binding | Temporary seam coordinates preserved exact native and planned rows on eleven static fixtures. At yoke widths 64/128/256, compilation took 150/671/3,997 ms against 71/247/1,011 ms. Peak memory at 256 fell from 161 to 102 MB, but repeated projection became more expensive with width. |

The terminal-binding and native-leaf rejection screens each used one paired
fresh-process d3 sample per case, with frozen release binaries and separate Cargo
targets on the Apple M5 Pro/macOS 27.0.1 host. Native leaves were limited to
always-active static graphs without source actions; adaptive causal planning was
not migrated. Canonical IR matched for the checked yokes and adder. Sources,
proofs and results remain under `target/composed-binding/`; both compiler
migrations were removed. A compact expression DAG is insufficient when its
consumers repeatedly traverse the same history.

These implementations remain local experiments. They do not establish a bounded
module algorithm. Exact raw-row, activation, feedback and fold comparisons cover
the corrected early-seam prototype on seven gallery graphs. A code-aware Pro
review independently identified its tag-deletion defect and the later query
interfaces that still require full physical rows.

## Required design change for a stronger claim

The remaining direction is a coherent relation and readout representation change,
not another cache-size adjustment:

1. Retain local signed generators, seam lenses, complete private directions and
   reconstruction references from the start. A parent must not copy every
   descendant's physical coefficients. Root-owned tensors need the same treatment.
2. Carry those references through causal admission and the global input kernel.
   Query exact raw coefficients and ordered support lazily. Preserve full support
   lengths for sparse pivot choices, creation-time coordinate copies, mapping
   checkpoints and strict readiness of the final coefficient functions.
   Independently normalizing each child is unsound: seam cancellation can create
   additional global kernel directions. T resources and Multiplex Z remain protected.
3. Bind a complete native site tuple before crossing reconstruction and signed
   gateway resolution. Reuse composed readout responses through a compact DAG
   while preserving cancellation, physical owners, activation and each readout's
   own raw/corrected decoder pair. Simple XOR of separately bound source leaves
   is not a valid signed response rule.
4. Prove and measure bounds in terms of actual interface/front width, guarded
   contexts, reconstruction visits and emitted IR size. Fixed small modules alone
   do not bound these quantities. Batched backward replay may share query work;
   its live query front must also remain bounded.

Acceptance requires exact signed/private-source equivalence, source-reference
pivots, named-value safety, raw off-activation values, GC/resource-limit behavior,
and physical fidelity. Measure cold and changed-root workloads across independent
modular families, varied topology, nesting and guard correlations. Report growing
fronts and typed exhaustion. No successful measured path may silently restore a
whole-root matrix and then claim bounded compilation.

The next representation change has potential, but no tested replacement currently
satisfies these conditions with an end-to-end scaling gain. The generic pipeline
therefore retains the validated improvements above without adding speculative
planner modes. Reproduce structural counts with `profile_adder_scaling WIDTH 3`
and source support with `profile_source_planner WIDTH`; see the
[benchmark methodology](benchmarks.md) for timing and provenance.

## Composed transfers through input normalization

`BooleanRow::probe_coefficients` queries raw coordinates in the leaf/scaled-XOR
witness DAG without expanding a row. It preserves order, duplicates and
coefficients outside activation, and prunes provably disjoint subgraphs. Separate
queries can still revisit the same history.

`BooleanDecisionDiagram::extend_witness_front` now shares projection work across
fixed row roots. It indexes the needed transfer uses and requested leaf terms,
then propagates one coordinate at a time in expression order. All contributions
combine before a parent consumes them. Only nonzero contributions enter the
worklist, so independent components do not form a dense row-by-column scan.
The sparse resulting fronts are installed together after successful evaluation;
exhaustion retains the old rows and partition without refunding spent work.

This is an ownership boundary: the caller supplies every surviving deferred row.
Columns only move into the explicit front; old deferred clones cannot be used
across this boundary. Original leaves, scales and front coefficients remain BDD
roots. Setup still visits the tape's history, and growing interfaces or query
fronts can still require substantial work. This is not yet the general immutable
snapshot/map backend needed by causal planning.

The generic planner uses this operation before global input normalization. It
exposes only input and protected-resource coordinates, then performs the same
source-ordered kernel elimination while the physical payload stays composed.
T resources, Multiplex Z, private directions and raw off-activation values are
preserved. Full physical rows are reconstructed later for the existing feedback,
residual-readout and binding consumers. The removed early expansion is therefore
a production improvement, not removal of all compiler-wide expansion.

The prefix-transfer work probe now distinguishes independent replay from shared
projection. Counts include constant work and are not timings:

| Prefixes | Independent oldest-coordinate queries | Batched projection |
| ---: | ---: | ---: |
| 64 | 16,896 | 1,471 |
| 128 | 66,560 | 2,943 |
| 1,024 | 4,202,496 | 23,551 |

The batch test also scales independent rows and disjoint requested columns,
rejecting a dense cross-product implementation. Exact checks cover promotion,
subsequent weighted pivots, new leaves, raw inactive rows, collection and every
work-limit cut through the operation.

### Compiler measurements

A wider version of the existing yoked-memory geometry exercises a nonempty global
input kernel. Width six reproduces the gallery geometry and port roles. This is
an additional workload, not evidence of bounded authored-module interfaces.
Three alternating fresh-process pairs at d3 used matching release builds,
separate Cargo targets, frozen hashes, an Apple M5 Pro and macOS 27.0.1:

| Workload | Previous compile | Composed compile | Previous peak RSS | Composed peak RSS |
| --- | ---: | ---: | ---: | ---: |
| Yoke, 64 columns | 66.93 ms | 65.03 ms | 28.9 MB | 26.5 MB |
| Yoke, 128 columns | 253.86 ms | 245.74 ms | 70.3 MB | 59.3 MB |
| Yoke, 256 columns | 1,026.59 ms | 996.32 ms | 215.8 MB | 162.8 MB |
| Adder, 128 bits | 6,416.19 ms | 6,423.80 ms | 2,171.4 MB | 2,165.8 MB |

Values are medians; RSS covers the compile/count process. The 256-column yoke
reduces peak memory 24.6% and compilation time 2.9%. Its separately measured
readout planning falls from 158.01 to 136.90 ms (13.4%). The adder is essentially
unchanged. Doubling yoke width still costs about four times as much, and emitted
IR/query/support counts are unchanged. No near-linear compiler claim follows.
Canonical IR text is identical for 8/64-column yokes and the ten-bit adder;
raw-row comparisons also include a 32-column yoke and guarded/resource fixtures.

Reuse `profile_adder_scaling yoke:256 3`, `profile_source_planner yoke:256`, and
`profile_composed_transfers 1024`. Results, rejected iterations and source/binary
provenance remain under `target/composed-compiler/`.

Earlier causal planning and final physical consumption still need native local
leaves and immutable copy/mask/constrain checkpoints, exact support ordering and
complete local tuple queries. The rejected binding experiments above show why
independent replay is insufficient. Independent child normalization, front-only
support ranking, or binding leaves separately would change semantics. Broad
support counting, BDD growth and emitted output remain explicit costs.

## Shared composed readout recipes

The retained next step shares ordered readout operands using the existing
unindexed `Observable` fragments and `Compose` edges. It adds no classical language, runtime opcode or codec
format. A late IR pass interns balanced operand pairs after the usual payload
merges, and emits only subtrees whose shared wiring saves edges. Building these
trees costs at most one pair per original operand occurrence; it does not expand
transfer coefficients. Running the same factoring before payload merging was
rejected because it obstructed those merges and enlarged the adder IR.

Recipes preserve ordered leaves, repeated occurrences, fold roles and physical
owners. Each original Observable remains the decoder boundary. Strict input
availability and record timestamps use existing recipe execution semantics.
Activated Observables are left intact, and levels containing Discard are skipped
to preserve their implicit execution barriers. The pass also skips levels whose
readout adjacency does not agree with slot and edge-index order: boundary products
can carry signs, so sorting their operands is not a valid normalization.

At d3, stored edge counts change as follows. Quantum nodes, templates, physical
measurements and stored readout payload terms are unchanged; recipe nodes increase
the node count.

| Workload | Previous IR edges | Shared IR edges | Previous IR nodes | Shared IR nodes |
| --- | ---: | ---: | ---: | ---: |
| Yoke, 64 columns | 20,405 | 5,185 | 1,211 | 1,560 |
| Yoke, 128 columns | 77,685 | 11,211 | 2,427 | 3,268 |
| Yoke, 256 columns | 302,837 | 24,037 | 4,859 | 6,538 |
| Adder, 64 bits | 717,257 | 649,532 | 185,771 | 188,725 |
| Adder, 128 bits | 2,416,074 | 2,125,250 | 620,835 | 628,385 |

Three alternating fresh-process pairs on the same Apple M5 Pro/macOS 27.0.1 host
found essentially unchanged compilation time and peak memory. Three further runs
of the final candidate reproduced these counts: median compilation was
67.67/252.23/1,014.65 ms for the yokes and 1.56/6.33 s for the adders. The candidate
check reused the earlier frozen baseline measurements rather than repeating the
whole comparison. Both builds used separate Cargo targets and frozen source and
binary hashes.

For the 128-column yoke, the earlier baseline's median VM preparation was 9.54 s;
the final candidate's median was 1.70 s. Binary size fell from 318,318 to 192,542
bytes, and peak RSS of the complete compile/codec/preparation probe fell from
265.7 to 108.2 MB. These are three-sample medians; the memory figures are not
compile-only peaks. No shots were timed. A single 256-column candidate prepared
in 14.96 s. The corresponding baseline probe exceeded its 240-second limit
without completed output, so it supplies no precise stage timing or speedup.

This removes repeated edges and improves preparation, but still builds expanded
readouts before compressing them. Compilation and preparation remain superlinear
over these widths. A stronger scaling result still requires shared, bounded
query evaluation through causal planning and physical binding, followed by direct
compact emission. Results and rejected iterations remain local under
`target/composed-binding/`; reproduce with `profile_adder_scaling` and
`profile_classical_storage`.
