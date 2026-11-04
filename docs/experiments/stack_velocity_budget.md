#32-layer stack convergence diagnostic

Baseline `a2c21a7`. This experiment changes only test instrumentation and an ignored configuration diagnostic. Production solver arithmetic and default configuration are unchanged. The32-layer30second5mm target remains unmet.

## Fixed measurement

Original side0.5 boxes, mass1, initial centers y=.25+i*.502, finite floor half extents(4,.25,4) centered at(0,-.25,0). Margin.005, position8, relaxation1 and default materials/warmstarting/sleep are unchanged. Acceptance remains independent all-pair and finite-floor f64 OBB SAT overlap <=.00501m on every public step at60Hz for1800steps30seconds, including sleeping bodies. No reduced layer count or duration substitutes for acceptance.

## First16-step phase evidence

`world::tests::stack_solver_32_layer_phase_replay` is an explicitly ignored diagnostic. `Q3D_STACK_SOLVER_TRACE=1` records prepared/warm/velocity/relax/position phases, bounded to33 bodies and the first16steps. Each pair reports the point with worst closing normal residual; pseudo and friction values belong to that point and are not extrema over all manifold points. Body velocities and pseudo velocities are also recorded. Existing12-layer replay is retained.

At step8, floor/body contact has step-start penetration4.300949mm. After20velocity sweeps its closing normal velocity is .056636192m/s; after relaxation it remains .044658974m/s. Position pseudo response is zero because penetration is below slop. Integrating approximately .044658974/60m of closure predicts5.045265mm, close to next-step measured engine penetration5.045198mm and the existing independent SAT5.045191mm (rotation/geometry account for the difference).

At step9, original neighboring layers1,2 (body slots2,3) have penetration4.9340455mm, residual closing velocity .054674674m/s after velocity solve and .048675828m/s after relaxation. Their selected point has zero pseudo response. The independent poststep SAT previously records5.745184mm. First failure precedes any floor departure. Thus finite velocity convergence and step-start slop gating explain the first excess;5mm slop is not a hard postintegration bound. This does not establish a warmstart/friction bug or attribute the entire later collapse to one mechanism.

Every poststep public pose/quaternion/linear/angular velocity/mass/sleep state is bitwise equal between parallel=false and parallel=true configurations. Actual parallel execution occurred on zero steps: the single stack falls back to serial. This is not a test of the parallel execution path.

## Controlled20 versus80 velocity iterations

Pre-registered six worlds: each budget20/80 runs4/12/32layers for1800steps,10800actualsteps total. Shared existing SAT helper runs every step. All six complete before asserting the80candidate target; known-failing20baseline does not stop candidate measurement.

| Velocity | Layers | Pair peak mm | Finite floor peak mm | Pass |
|---:|---:|---:|---:|---|
|20|4|1.299087|1.097863|yes|
|20|12|3.537737|3.593264|yes|
|20|32|436.883123|201.288257|no|
|80|4|1.232317|1.100204|yes|
|80|12|1.558353|1.297610|yes|
|80|32|110.460549|180.485814|no|

All20baseline values reproduce the previous sustained experiment. With80velocity sweeps32layers first exceeds at step365, pair11,12,5.069925mm, while both complete footprints remain inside the finite floor. Pair peak occurs at450 and floor peak447. Final state has18sleeping/14awake; later collapse and floor escape persist. More velocity work delays failure and reduces sampled peaks but does not meet the fixed target. No production solver fix, accepted32-layer recipe or performance improvement is delivered. No additional parameter sweep or performance benchmark was run; printed wall times include SAT and are diagnostic only.

## Reproduce

Use existing offline Rust1.99 dependencies and the workspace's Cargo/Rustup environment. Run one build at a time.

```bash
Q3D_STACK_SOLVER_TRACE=1 cargo test --release --offline --locked \
  -p quench3d-physics --lib world::tests::stack_solver_32_layer_phase_replay \
  -- --ignored --exact --nocapture
cargo test --release --offline --locked -p quench3d-physics \
  --test stack_precision stack_velocity_budget_80_controlled_diagnostic \
  -- --ignored --exact --nocapture
```

The phase replay successfully compiled and passed(exit0); it is diagnostic, not precision acceptance. The budget test successfully compiled and failed its registered80candidate acceptance(exit101) after all six worlds completed. Original logs/status/registration/source hashes and a bounded summary remain under `target/tmp/round178-stack-convergence/`. Both jobs were formally collected and source hashes verified after execution. Full workspace gate, packaging and performance measurement were not run for this diagnostic.
