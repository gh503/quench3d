# Particle chain recompression: registered operator counterexample

Status: registered analytic hypothesis, **not compiled or executed**. No new physical red or green result is claimed. This registration adds no production API, test source or solver implementation. Actual execution requires the parent's later source/build-slot authorization.

## Question and fixed scope

Can a later pair correction recompress a previously separated pair even without a boundary? This isolates shared-particle constraint interaction from the distinct blocked-wall-share problem in [boundary mobility](boundary_separation_mobility.md). A single successful two-particle correction does not establish simultaneous satisfaction of a connected particle graph.

Call the actual private `Fluid::separate_particles` once from a future `cfg(test)` unit test. Do not duplicate the production operator as the test implementation. Use **no rigid bodies**, passing the empty body slice and `None` for optional bounds/index/mesh cache. Do not call `world.step`, integrate gravity, apply density corrections, rebuild velocities, or repeat separation until green. There is no elapsed simulation time in this operator experiment. The ordinary descriptor's four PBF iterations remain unchanged but are **not executed**; this experiment is distinct from the full-step sealed acceptance.

## Registered inputs

- Radius `.04f32`, spacing `.1f32`, particle collision enabled, collision mode `SweptMeshes`, separation mode `BoundaryConstrained`; unrelated descriptor values remain default.
- Three physical particles: A=`(0,1,0)`, B=`(.06,1,0)`, C=`(.12,1,0)`. Zero initial velocities, capacity exactly three, explicit initial positions. Preserve and report stored f32 input bits rather than silently replacing them with exact decimal arithmetic.
- Set actual predicted positions to those initial positions. Construct actual neighbor lists using the same `FluidGrid` neighbor API as production. Kernel radius is `2*spacing`, approximately .2m; all three initial unordered distances (.06, .06, .12m) are strictly inside support. Require all six directed neighbor edges, so missing neighbors cannot explain failure.
- Execute two separate fresh fixtures with index orders **[A,B,C]** and **[C,B,A]**. Map results back to physical particle identity for reporting. Preserve production grid/neighbor ordering; do not sort the production list to fit a prediction.
- Invoke one separation call per fixture, including the operator's ordinary final wall projection (with an empty body slice this has no boundary to project against).

## Analytic hypothesis, not measurements

The following table uses ideal decimal arithmetic to explain the expected mechanism. Actual f32 results may differ by rounding; these numbers are not substitutes for measured source output and are not frozen implementation-output contracts.

| Index order | First adjacent correction | Later adjacent correction | Predicted final physical A/B/C X | Predicted recompressed pair |
|---|---|---|---|---|
| A,B,C | AB: A→−.01, B→.07; AB reaches .08 | BC: B→.055, C→.135 | −.01 / .055 / .135 | AB: .08→.065m |
| C,B,A | CB: C→.13, B→.05; BC reaches .08 | BA: B→.065, A→−.015 | −.015 / .065 / .13 | BC: .08→.065m |

The nonadjacent A/C pair is visited but is already beyond diameter and should not be pushed. Each subsequent adjacent correction moves the shared middle particle toward the earlier endpoint. Reversing indices should exchange which physical pair is left compressed rather than solve the graph. Neither a static wall nor its lost correction share is required. Free-space arithmetic also bypasses BoundaryConstrained's boundary-triggered residual opportunity.

## Independent oracle and execution plan

For each permutation, widen the **returned stored f32 coordinates** to f64 before subtraction and compute Euclidean distances independently for all three unordered pairs. Do not use solver neighbor membership or production distance helpers as the acceptance oracle. Registered target for every pair is:

```text
distance >= 2 * f64::from(radius) - 1e-4 m
```

This is approximately .079899998212m for the actual `.04f32` radius. Do not round radius to an exact .04 f64 before computing the threshold. Record each pair's initial and final positions/distance, which physical pair fails, and the minimum for each ordering. Run **both fixtures completely before asserting the strong all-pair target**, so a first failure cannot conceal the second ordering.

Keep the following controls separate from the separation assertion:

- Exactly three particles; finite returned positions; unchanged per-particle calibrated mass and total mass.
- No unrequested Y/Z motion beyond a small predeclared rounding tolerance, and no velocity mutation by this position-only operator.
- Independent f64 sum of all three positions before/after within **1e-6m** per coordinate; centroid tolerance is that sum tolerance divided by three. Equal-half pair corrections preserve each visited pair's center algebraically, so this global check separates internal rearrangement from unexplained translation. Use tolerances for rounded stored positions, not bitwise equality of independently summed floats.
- With test-only existing `MESH_CONTINUITY_TRACE`, record each actual pair correction's before/request/accepted endpoints. In free space the accepted endpoints should match the requested corrections. Demonstrate that the first physical adjacent pair reaches diameter within **1e-6m**, and then the later pair visit reduces its distance. Do not infer phase attribution solely from the final minimum.
- Independent two-particle free-space control with A/B only, same radius and one call: distance meets the same diameter-minus-tolerance target and center is preserved. This distinguishes an isolated pair solve from graph interaction.

The predicted .065m residual is far below the fixed target, so ordinary f32 rounding cannot plausibly account for the hypothesized failure. Still, only a successful compilation followed by actual assertion failure counts as physical red. A compile error, zero executed tests, or analytic reasoning alone does not.

## Interpretation and limits

A failure would support the specific mechanism of later-pair recompression in a one-pass connected graph. It would not establish a new global projection method, sufficient iteration count, density accuracy, full-step momentum/energy conservation, or arbitrary geometry behavior. The isolated position operator has no velocity reconstruction; its center control is not a full momentum audit. Conversely, a single-pair green or aggregate centroid preservation would not satisfy the all-pair separation requirement.

Do not add repeated passes, extra PBF iterations, timestep changes, modified particle inputs, reordered production loops or a looser geometric threshold to make this registration pass. Any future graph-wide/active-boundary solver needs a separate registered algorithm and budget, plus the unchanged [sealed 125-particle full-step target](sealed_particle_separation.md). This document does not authorize implementing that solver.

No command has been run for this registration. A future test-only strong target should remain explicitly ignored while unmet; normal gate success must not be described as passing this target. Record actual command, source hashes, all permutation results and formal exit status only after execution is authorized and completed.
