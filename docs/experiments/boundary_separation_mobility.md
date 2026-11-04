# Boundary separation mobility: registered counterexample and candidate scope

Status: implemented experimentally, measured, and rejected by the unchanged sealed precision target. The `BoundaryMobility` variant and implementation were removed. Production fluid code is byte-identical to the committed baseline; only the ignored old-mode counterexample/strong target and a test-only read-only body-slot accessor remain. No CLI/schema support or user-facing mode was added. The replayable [candidate patch](../../tools/experiments/boundary_separation_mobility.patch) preserves the rejected implementation and its test wrappers.

## Observed implementation and hypothesis

`Fluid::separate_particles` assigns each particle half of the missing diameter. `sweep_separation_slide` removes the component of a correction directed into its encountered wall; the rejected share is not reassigned to the other particle. Its one bounded residual correction again divides the remaining deficit equally. For a nearly vertical pair with the lower particle held at floor clearance, a full intended pair correction therefore need not produce a full pair-distance correction. In the ideal vertical case, first correction leaves half the deficit and the residual leaves one quarter. This is an analytic hypothesis about the existing algorithm, not a measured new result.

This hypothesis is consistent with the [sealed fixture's recorded failures](sealed_particle_separation.md): first failure step14 pair0/5 and worst failure step19 pair55/60 both have a lower particle at y=.04m. It does not yet establish which phase creates the worst deficit. Four density iterations precede pair separation; the density update after separation computes statistics without changing positions. Later pair visits and the final wall projection can still decrease an earlier pair's distance. Existing phase traces should distinguish these effects.

## Test-only isolated counterexample, registered before execution

Use the actual private separation function from a `cfg(test)` unit test, not a duplicated implementation:

- One static floor box centered `(0,-.05,0)`, half extents `(.6,.05,.6)`; top face y=0. No side walls, moving bodies, gravity integration or density correction in this isolated function test.
- Two particles, radius `.04f32`, positions `(0,.04,0)` and `(0,.10,0)`. Their initial separation is approximately .06m, below diameter .08m. Both initial centers satisfy the floor clearance. Set the actual predicted positions and actual symmetric neighbor list `[1]` / `[0]`; visit the unordered pair exactly once through the existing `separate_particles` call.
- Existing collision mode `SweptMeshes`, separation mode `BoundaryConstrained`; all unrelated descriptor values remain default. Preserve and record exact f32 input bits.
- Invoke one existing separation call, including its current bounded residual and final wall projection. Do not add repeated calls until green. The analytic ideal is lower y=.04 and final gap .075 (initial deficit .02, quarter deficit .005); record actual values rather than requiring these rounded predictions as an oracle.
- Independently widen returned f32 positions to f64, measure Euclidean distance, and evaluate signed floor clearance. Registered desired target: distance at least `2*f64::from(radius)-1e-4` and each center floor clearance at least `-1e-4`. Require finite positions and exactly two particles. The old implementation must compile and execute a real failing separation assertion; a build error or zero tests is not red.
- Keep a free-space control without the floor, and a pair whose requested displacement is entirely tangent to a single plane; existing modes must retain their registered arithmetic and results. Add a deterministic i/j ordering control before claiming an unbiased mobility allocation.

This is a geometric operator counterexample. It cannot establish full-step momentum/energy behavior, density accuracy, containment over time, or the sealed-container fix. A static wall may exchange momentum with the pair, so preserving the isolated pair center of mass is not a universal boundary criterion.

## Proposed opt-in candidate, not an implemented interface

The proposed explicit enum variant is `BoundaryMobility`. Preserve `Legacy` and `BoundaryConstrained` behavior and their existing trajectories bit for bit. The new variant must not silently replace `BoundaryConstrained`, and must not become the default. Public Rust/scene/CLI spelling, validation, round-trip handling and user-facing schema require parent approval before implementation; no unsupported interface is promised here.

Within a current pair visit, allocate the requested normal separation according to the feasible displacement of both endpoints. A blocked normal direction should contribute zero usable mobility; the other endpoint may take the rejected share when its sweep permits that displacement. The ideal single-floor case has lower endpoint mobility zero and upper endpoint mobility one. For a local planar active constraint and pair unit normal n, one possible linearized allocation uses the feasible projector P, weight w=n^T P n, and lambda=gap/(wA+wB), with endpoint displacements lambda*PA*n and -lambda*PB*n. For an inward direction at a single wall, P=I-NN^T; outward motion remains unconstrained. This is a linearized proposal, not an exact finite-distance or multiwall solution. Do not divide by zero when both are blocked. For tilted pairs and intersecting boundaries, a scalar projected-normal weight is only a proposal: validate the achieved pair-distance change and sweep every actual segment against the real geometry. Do not assume that a cached single wall normal represents all active walls, or normalize a nearly zero feasible direction into an unbounded correction.

Budget is fixed: the existing four PBF density iterations, one existing unordered-pair traversal, and the existing bounded residual opportunity. No extra postpass, repeated neighbor sweep, iteration increase or hidden density solve. Record actual sweep, slide and residual work; unchanged outer loop counts do not prove unchanged computational cost. Any additional query cost requires separately registered performance acceptance after physical targets pass. Preserve moving-mesh correction semantics: corrections query current boundary geometry and must not replay boundary time.

Required controls before acceptance include free space and tangent motion; both endpoints blocked; floor-plus-wall corner; arbitrary pair orientation and reversed particle ordering; static primitive and finite mesh boundaries; and current moving-boundary continuity tests. A robust future implementation may need multiple active normals, but that extension is not authorized by this registration.

## Full acceptance remains unchanged

The mandatory full target is the exact [sealed diameter-separation fixture](sealed_particle_separation.md): 125 particles, radius .04m, original six-wall geometry/initial velocity/materials and timestep, four PBF iterations, 120 actual 60Hz outer steps. Scan all 7,750 unordered pairs per step with independent f64 geometry, 930,000 samples total, minimum distance `2*f64::from(radius)-1e-4` (approximately .0799m). Complete all120steps before the final separation assertion. Retain original particle/mass, finite/speed, six-face clearance, exact-step and non-vacuous-work checks.

A two-particle green alone never means the sealed target is fixed. No threshold widening, changed initial state, extra iterations/substeps, enlarged container, skipped failing steps or sleeping surrogate. Preserve first/worst pair and phase evidence, default/old-mode bitwise controls, source hashes and formal job exit statuses. Production acceptance, broader gate and any release commit remain parent decisions after the unchanged full target passes; no desktop tar is needed for a diagnostic-only outcome.

## Actual execution and rejection

The first old-mode command failed compilation with E0616 because `PhysicsWorld::bodies` is private; this is not a physical red. A minimal `cfg(test)` read-only slot accessor corrected the fixture without changing production visibility or math. The retry successfully compiled and actually executed the single-call operator: distance **0.074999995530 m**, target **0.079899998212 m**, lower `(0,.04,0)`, upper `(0,.114999995,0)`. Finite/count/floor-clearance assertions passed, then separation failed with exit **101**.

An explicit experimental `BoundaryMobility` stub routed to the unchanged BoundaryConstrained algorithm and successfully compiled. Both ignored new-mode test wrappers actually ran: the isolated floor assertion failed at the same distance; the sealed wrapper completed 120 steps / 930,000 pair samples and reproduced the old-mode first violation (step14 pair0/5) and minimum **0.05428381219042488 m** (step19 pair55/60). Exit **101** was formally collected before implementing the candidate.

The candidate preserved the initial equal-half swept corrections. Only inside the existing single residual opportunity, it estimated an active wall normal from rejected initial displacement, projected the requested pair direction unilaterally, and divided the gap by combined projected mobility. Outward displacement remained free. Nonfinite or weight ≤1e-6 returned zero correction. All actual segments still used the existing swept/slide path. This rejected-displacement estimate can combine multiple contacts and is not proven to be the real active-set normal; finite nonlinear geometry is not an exact projector. Small positive mobility can produce large requested corrections; no robustness guarantee is claimed. No extra pair pass or PBF iteration was added.

The implemented candidate successfully compiled and executed two tests (release compile 54.46s, test execution 0.08s). Result: **one passed, one failed**, exit **101**:

| Metric | Stub / old mode | Implemented candidate |
|---|---|---|
| Single-floor pair distance (m) | 0.074999995530 | 0.079999998212 — passes isolated target |
| Sealed first violation | step14, pair0/5, 0.07847939028909445 m | step15, pair0/5, 0.0769171661319568 m |
| Sealed worst | step19, pair55/60, 0.05428381219042488 m | step19, pair60/85, 0.06264746328208627 m |
| Full steps / pair samples | 120 / 930,000 | 120 / 930,000 |
| Contact samples | 11,407 | 11,302 |
| Separation pushes | 13,535 | 13,768 |
| Nonzero density corrections | 11,894 | 11,778 |
| Other invariant failure | None | None |

Candidate first violation positions: `(-.16145246,.04,-.21188988)` and `(-.18050449,.10486566,-.24857388)`. Candidate worst positions: `(-.24559467,.071962334,-.020210812)` and `(-.2682333,.07984954,.037668277)`. Both runs preserved total calibrated particle mass **123.789966106415** and minimum wall clearance **−0.000000007451 m**, within the registered tolerance. These checks establish containment and mass invariance, not density or momentum accuracy.

The unchanged 0.079899998212m target still fails; the improved minimum and isolated green do not meet acceptance. The candidate was immediately rejected without changing the fixture, threshold, mobility cutoff, iterations, or traversal budget. Tilted/corner/blocked/order controls, old-mode bitwise trajectory comparisons of the candidate, moving-boundary controls, precise sweep-cost counts, and performance timing were **not run**, because mandatory full precision already failed. No performance benefit is claimed. No CLI/schema wiring or release artifact was produced for this candidate.

## Restore and reproduce

The fluid production prefix preceding `#[cfg(test)] mod tests` was checked byte-for-byte against HEAD after removal; SHA-256 of that prefix is `0f7bb4cd99d7b7c0846eb4e65649e67944c9d75ee17bb0b1223cc5ae42b65ce3`. Legacy and BoundaryConstrained production algorithms, enum, and defaults are restored exactly. The old-mode ignored targets remain distinct from normal green regression tests. The new enum, implementation, and new-mode wrappers are absent from shipped source.

The [patch](../../tools/experiments/boundary_separation_mobility.patch) applies to the restored source, which already includes the retained ignored targets and test-only world accessor. It contains the complete candidate delta, including enum and both new-mode wrappers. SHA-256: `ca00aa2eeaaa3d5445fc9972e525482514ddadc5413cf9b4b9b92c30e258b4bf`. `git apply --check` passed against the restored working tree. Restore-source SHA-256 values at handoff: fluid `d57d93dd4302eb34abcf640023a1a5ff065396d6dfbb85be1ad3c0e728e93699`, world `788bc2dbe96564b014a83acac8d7cccb6a3c2cc59ea53d82d2be4afbf504e0db`.

Use an isolated checkout of this restored experiment record, Rust 1.99 and the workspace offline dependencies:

```bash
git apply --check tools/experiments/boundary_separation_mobility.patch
git apply tools/experiments/boundary_separation_mobility.patch
cargo test --release --offline --locked -p quench3d-physics --lib \
  fluid::tests::mobility_ -- --ignored --nocapture
```

Expect successful compilation and the sealed assertion failure above. To reproduce the stub, retain the patch's variant/dispatch/wrappers but remove its new-mode residual allocation, leaving the historical equal-half residual. To execute only the retained old-mode counterexample without applying the patch:

```bash
cargo test --release --offline --locked -p quench3d-physics --lib \
  fluid::tests::boundary_floor_single_visit_target -- --ignored --exact --nocapture
```

All three actual builds used the coordinated `target/tmp/round180-boundary-mobility/build.lock`, workspace CARGO_HOME/RUSTUP_HOME/offline environment, and formally collected jobs. Local supplementary evidence is under `target/tmp/round180-boundary-mobility/`: `floor-red.log` is the invalid compile failure; `floor-red-valid`, `stub-red`, and `candidate` each have `.log`, `.status`, and `.source` files. Each valid run's status is 101. These ignored local artifacts supplement the durable measurements and replay patch above. No full gate result is claimed by this experiment record; the parent runs the restored-source gate separately.
