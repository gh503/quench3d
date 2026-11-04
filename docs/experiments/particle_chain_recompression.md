# Particle chain recompression: registered operator counterexample

Status: the registered cfg(test) operator target was implemented and executed after parent source/build-slot authorization. Release compilation succeeded; one exact ignored test completed both three-particle index orders and the isolated two-particle control, then failed only the final all-pair separation assertion (exit 101). No production API, solver implementation, parameters, or iteration budget changed. The strong target remains explicitly ignored while unmet; normal gate success does not mean it passes.

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

The original registration above preceded implementation and execution. The following actual result is separate from its analytic predictions.

## Actual release execution — Round217

The parent reviewed the new cfg(test) function `fluid::tests::particle_chain_recompression_target` before authorizing execution. `cargo fmt --all` completed with exit 0; the source then remained frozen throughout the single release job. Workspace CARGO_HOME/RUSTUP_HOME, CARGO_NET_OFFLINE=true, the Node24/toolchain PATH, and shared CARGO_TARGET_DIR=/home/gh503/Documents/infra-dev/quench3d/target/default-bits-candidate-193 were used. No concurrent Cargo job or production candidate was started.

```bash
cargo test --release --offline --locked -p quench3d-physics --lib \
  fluid::tests::particle_chain_recompression_target -- --ignored --exact --nocapture
```

Release compilation succeeded in 53.25 s. Exactly one test executed, including the isolated AB control and both ABC/CBA fixtures. The formally collected job `bash-2b1d01a942244537` exited **101**. Its only panic was the final aggregated all-pair separation assertion, after the completed-fixture count reached two. Complete output and status are retained locally at `target/tmp/round217-particle-chain/red.log` and `red.status`; `source.sha256` records frozen fluid source SHA-256 **b7a79a378e17750e67e1559224d510cd4b005861483b5ab1e2716f1e235291a6**.

The AB-only control passed the same diameter-minus-tolerance target, and its position sum, mass, finite-state and unchanged-velocity checks passed. Both three-particle fixtures had exactly six directed real FluidGrid neighbor edges. Every trace before endpoint matched reconstructed current state; free-space accepted endpoints matched requests; the final no-boundary projection left every point unchanged and reconstructed state matched actual `predicted`. Each first adjacent correction reached stored-radius diameter within 1e-6 m, and the subsequent actual pair correction reduced that earlier pair's distance. No assertion hard-coded the analytic .065 prediction.

| Index order | First physical pair | Distance after its correction (m) | Same pair after later visit / final minimum (m) | Other adjacent final distance (m) | Nonadjacent final distance (m) |
|---|---|---|---|---|---|
| ABC | A/B | 0.080000000075 | **0.064999999478** | 0.079999990761 | 0.144999990240 |
| CBA | C/B | 0.079999998212 | **0.064999997616** | 0.079999998212 | 0.144999995828 |

The fixed target was **0.079899998212 m**. The first reported violation was ABC physical A/B; the global minimum was CBA physical C/B. ABC final physical A/B/C X coordinates were approximately `[-.01, .055, .13499999]`; CBA mapped back to physical A/B/C was `[-.015000001, .065, .13]`. All Y coordinates remained 1 and all Z remained 0. Input X bits for A/B/C were `[0, 1031127695, 1039516303]`, Y bits 1065353216, Z bits 0; radius bits 1025758986. Calibrated per-particle mass bits remained 1065190808; three-particle total mass remained 2.970959186554. All position-sum checks passed the registered 1e-6 m per-axis tolerance; velocities and stored positions were unchanged by this position-only operator.

This actual red supports later-pair recompression in the existing single traversal independently of boundary blockage. It does not prove which phase dominates the separate sealed125-particle trajectory, fix density/pressure accuracy, or establish a sufficient global solver. The existing sealed125 × 120-step target and its 930000 independent pair samples remain mandatory and unchanged.

The current strong test also contains historical mechanism assertions (first pair reaches diameter, then is recompressed). Those diagnose this baseline and must not become requirements imposed on a future corrected solver. Before a production candidate is evaluated, preserve the baseline attribution evidence separately and separate the pure all-pair correctness target from these expected-failure mechanism assertions; do not loosen the geometric target or fixture. No production fix, performance claim, full gate, or release-artifact verification was performed by this isolated red job.
