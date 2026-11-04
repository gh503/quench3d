# Sealed particle separation: registered acceptance target

Status: registered target executed in release and failed after successful compilation (exit 101). Exactly one test completed all 120 steps. The test remains explicitly ignored because diameter-level separation is an unmet goal, rather than a guarantee of the shipped API. Production code, defaults, and existing diagnostic tests are unchanged.

## Fixed fixture

The new `sealed_container_diameter_separation_target` in [fluid tests](../../crates/quench3d-physics/src/fluid.rs) copies the existing `boundary_constrained_separation_closed_container_120_steps` fixture without changing geometry, initial state, iterations, or duration:

- Six static boxes, inner faces X/Z ±0.5 m and Y 0/1 m. Thickness 0.1 m; overlapping full outer spans prevent legal corner bypass. Floor/ceiling centers `(0,-0.05,0)` / `(0,1.05,0)`, half extents `(0.6,0.05,0.6)`; X walls centers `(±0.55,0.5,0)`, half extents `(0.05,0.6,0.6)`; Z walls centers `(0,0.5,±0.55)`, half extents `(0.6,0.6,0.05)`.
- Gravity `(0,-9.81,0)`, fixed dt `1/60`, 120 actual public `world.step` calls (approximately two seconds).
- Fluid center `(0,0.5,0)`, half extents `(0.2,0.2,0.2)`, spacing 0.1 m, radius 0.04 m, 125 initial particles, capacity 2000, initial velocity `(0.15,0,-0.1)` m/s.
- Four existing PBF iterations; particle collision enabled, `SweptMeshes`, `BoundaryConstrained`; all remaining fields keep `FluidDesc::default()`. No emission, drain, bubbles, or foam.

## Registered pass criteria

After every actual step, independently widen the stored f32 positions to f64 and scan **all 7,750 unordered pairs**, not the solver neighbor list. Require every pair distance to be at least `2 * f64::from(radius) - 1e-4` m (approximately 0.0799 m). Record the first failing step/pair/positions and the global worst step/pair/positions; finish all 120 steps before asserting the separation target. There are 930,000 expected pair samples.

At every step also require exactly 125 particles and velocities, bitwise unchanged calibrated per-particle mass and unchanged total mass, finite position/velocity, stored speed at most `MAX_SPEED + 1e-4`, independent analytic clearance from all six inner faces at least `-1e-4` m, exact step count and no accumulator remainder. Density trace must contain 125 × 4 entries each step; aggregate actual contacts, separation pushes, and nonzero density corrections must all be nonzero. These work counts establish exercise, not density accuracy.

The fixed 0.1 mm geometric tolerance is registered before executing this new test. Do not lower the separation threshold, enlarge the container, change timestep/initial velocity, increase iterations, or skip early failing steps to obtain a pass. This target is limited to this fixture and duration; it does not establish arbitrary-scale, moving-boundary, or full incompressibility guarantees. The container is not full of nominal liquid volume, so this is not a sealed full-volume pressure test.

## Prior evidence, distinct from the new target

The committed [changelog](../../CHANGELOG.md) records the exact existing BoundaryConstrained 125-particle/120-step fixture's minimum pair distance as **0.054283812190 m**, with 13,535 separation pushes, 11,894 nonzero density corrections, and 11,407 contact samples. That existing normal test records the minimum without enforcing diameter separation. Its result does not count as execution of this newly registered stronger test.

The older Legacy-separation fixture records **0.027011101845 m** at step 69, pair 112/122; `closed_container_worst_pair_phase_attribution` replays and attributes that distinct old-mode trajectory. It is not the expected numeric output of the new BoundaryConstrained target. No density error distribution has been measured by this new registration.

## Execution after build-slot authorization

Use Rust 1.99 and the workspace offline dependencies. The owner coordinates the shared build slot before running:

```bash
cargo test --release --offline --locked -p quench3d-physics --lib \
  fluid::tests::sealed_container_diameter_separation_target \
  -- --ignored --exact --nocapture
```

Confirm one test actually executes, preserve complete output and exit status, and record first/worst samples and invariant/work summaries. A compilation error, zero executed tests, or an interrupted process is not a physical red. Keep this target explicitly ignored until an accepted fix meets the unchanged criteria; normal gate success must not be described as passing this target. 

## Actual release result

Rust/cargo 1.99.0 compiled successfully in 58.44 s; the exact command above executed one test, completed 120 steps and 930,000 pair samples in 0.08 s, and failed the registered separation assertion with exit 101. The single runner held `target/tmp/round178-stack-convergence/build.lock`; complete local output and status are retained in `target/tmp/round178-sealed-separation/red.log` and `red.status`. This run used the current Rust shims with no CARGO_HOME/RUSTUP_HOME override; offline dependency resolution succeeded. No second build or rerun was needed.

- Required minimum distance: **0.079899998212 m**.
- First violation: step **14**, pair **0/5**, distance **0.07847939028909445 m**. Positions: `(-0.16657573, 0.04, -0.22187544)` and `(-0.16900542, 0.118238635, -0.227517)`.
- Global worst: step **19**, pair **55/60**, distance **0.05428381219042488 m**. Positions: `(-0.2921768, 0.04, -0.03773418)` and `(-0.27360618, 0.08827286, -0.021254053)`.
- Every count/mass/step/work/finite/speed/boundary invariant passed (`invariant_failure=None`). Total calibrated particle mass remained **123.789966106415**. Minimum six-face signed clearance was **−0.000000007451 m**, within the registered −0.0001 m tolerance.
- Non-vacuous work: **11,407** contact samples, **13,535** separation pushes, **11,894** nonzero density corrections. These reproduce the prior fixture counts, not density accuracy or momentum conservation.

Frozen source SHA-256 values before and during the run:

| Source | SHA-256 |
|---|---|
| [fluid](../../crates/quench3d-physics/src/fluid.rs) | `d14a8426fc98a20d899f6aef130db0d7188081b757ee44ed9f601b898e211d77` |
| [world](../../crates/quench3d-physics/src/world.rs) | `d42eea5ee3bff201d38b06f03d3a7d84ab21ab7f056ac76be2123844e23d70b5` |
| [stack test](../../crates/quench3d-physics/tests/stack_precision.rs) | `9ee085bf8794953d25a7afb9e47d51a5f66f7a29d2b4ea1ba7c8e1cc1df92f4e` |

No production candidate was implemented and no parameter or acceptance threshold changed after measurement. This is a confirmed separation failure in a contained fixture, not a wall-leak failure or a proof about full-volume incompressibility. No full gate was run as part of this isolated measurement.
