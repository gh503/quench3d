# Coplanar manifold block normal candidate: registered experiment

Baseline a2c21a7 plus the separately recorded Round178 test diagnostics. Status: **implemented, measured and rejected**. Production solver/configuration/plumbing and candidate-only tests were restored exactly to their pre-candidate contents; Round178 diagnostic tests and peer fluid tests remain. The registration below preceded implementation and was not relaxed. The archived candidate is experimental and not production-ready.

## Immutable acceptance and scope

Candidate public runtime configuration `manifold_block_normal: bool`, default false. Default false retains the original per-point `solve_velocity_contact` arithmetic and order. Enabled only: solve up to four compatible coplanar contact points from the SAME original manifold using nonnegative normal impulse active subsets of at most three independent support points. No changes to materials, sleep, margin, slop, timestep, position solver, relaxation, warmstart semantics, or iteration budget.

Precision acceptance is fixed before implementation: side0.5/mass1 boxes, initial2mm gap, existing finite floor half(4,.25,4), margin.005, slop.005, velocity20/position8/relaxation1,60Hz1800actualsteps30seconds,4/12/32layers. Each public step independently checks every body pair and finite-floor f64 OBB SAT <=.00501m. All three worlds complete before asserting acceptance. No reduced layers/duration/substeps, larger floor, changed friction or wider tolerance substitutes. First add config/plumbing and acceptance with the existing scalar behavior; it must compile and execute a real assertion red. Compile failure is not behavioral red. Default-off baseline remains the separately measured failed32layer recipe.

Only if all precision targets pass, run production-library performance comparison for12/32layers,8warmup+120measuredsteps,5trials per mode/scale, alternating trial order in the same release binary. Median regression must be<=15% at EACH scale. Diagnostic SAT/log walltimes are not performance benchmarks. Precision failure stops timing and rejects the candidate; archive a complete reproducible patch and restore production changes, preserving honest records.

## Numerical method and fallback

For pointi, J_i=[-n_i,-r_ai cross n_i,+n_i,+r_bi cross n_i], K_ij=J_i M^-1 J_j^T. With current normal impulses lambda_old and current point normal velocities after the enabled friction stage, b=v-target-Klambda_old. Solve lambda>=0,w=Klambda+b>=0,lambda_i*w_i=0. A same-normal coplanar4point normal matrix has rank<=3: normal translation plus two independent rocking torque directions. Do not invert a full singular4x4 or claim arbitrary2point support covers full3D torque.

Enumerate active masks deterministically, including empty, with active cardinality<=3; solve small active systems in f64 with relative pivot checks. Validate finite results, nonnegative active impulses, inactive nonnegative velocities and active residuals with explicitly bounded scale-relative numerical tolerance. Apply accepted normal impulse deltas in original point order and store individual accumulated impulses for the existing cache. No added diagonal softness or arbitrary regularization to force acceptance.

Preserve original manifold identity through flattened contacts and native parallel remapping; same body pair alone is insufficient for compound shapes. Geometry compatibility requires same oriented normal and coplanarity. Duplicate/collinear/ill-conditioned/nonfinite/incompatible geometry or no valid KKT subset falls back deterministically to the original per-point solver. Snapshot both BodyViews and up to four contacts before attempted enabled friction/block processing; restore fully before scalar fallback, avoiding double friction. Enabled scheduling can perform original point-order friction then coupled normals; default false stays in the original complete scalar function unchanged.

All serial and parallel execution routes share `solver::solve_velocity_contacts`; thread option/config through call plumbing rather than duplicate block algorithms. New metadata must not reorder default preparation floating arithmetic. Actual parallel verification must assert parallel_used=true in an appropriate multi-island fixture; the existing single32tower replay is serial fallback and is not parallel execution evidence.

## Required independent checks

Small matrix/KKT tests cover two tangent torque axes, nonnegative support, rank-deficient four coplanar points and degenerate fallback. Default-off public state bits and existing digest must remain unchanged. Enabled serial versus actual native parallel states/impulses must agree; inspect no-joint batch and joint iteration call paths. Analytic checks do not replace the full30second three-scale target. Keep the first16step32layer phase trace to measure remaining normal closure under the same20/8 budget; improvement alone is not acceptance.

## Actual evidence and rejection

The stub successfully compiled in20.29s and ran all three worlds/5400steps before a real assertion failure(exit101). Pair/floor peaks reproduced the scalar baseline:4layers1.299087/1.097863mm,12layers3.537737/3.593264mm,32layers436.883123/201.288257mm. An unused manifold metadata warning was expected in this stub, not a compile failure.

Implemented candidate mathematical test passed(exit0): asymmetric rectangular support with both independent rocking moments, rank3KKT, empty support and zero-matrix rejection. This is ONE mathematical test, not completion of all registered compatibility/fallback cases. The candidate then successfully compiled in20.78s and completed all three worlds/5400steps before failing the unchanged acceptance(exit101):

| Layers | Pair peak mm | Finite floor peak mm | Accepted |
|---:|---:|---:|---|
|4|0.942390|1.510429|yes|
|12|5.136020|3.773256|no|
|32|345.012218|177.069896|no|

12layers first exceeded at step1341, original pair0,1 depth5.019990mm with both complete footprints inside the floor. Its pair peak occurred1350.32layers first exceeded at step8: floor5.106917mm and pair0,1 depth5.384419mm, both full footprints inside; later pair peak occurred80 and floor172, with collapse/escape. Changing later sleep or floor escape interpretations cannot repair this early failure. Allworlds used the same fixed fixture and20/8budget. A copied friction-prefix final unused assignment generated a warning; no Clippy green or production readiness is claimed.

Rejected immediately after precision. No cost benchmark, actual native parallel candidate execution, new default-off digest check, rollback fault-injection, full degeneracy suite or bounded block success/fallback counter run was performed. Thus neither actual block usage rate nor a complete root-cause attribution is established. f32effective-mass dot products were widened to f64 before solving; finite f64impulses were cast to f32without an explicit overflow guard, another reason this archived experiment is not ready for production. No postfailure tolerance adjustment or extra parameter search was attempted.

Stub job bash-68694edd24a54353 was formally collectedexit101. Combined math/candidate job bash-1e655d4334e04810 was formally collectedexit101, with math.status recordingexit0 and candidate.status recordingexit101. Logs remain under `target/tmp/round179-manifold-block/`. No complete gate/package was run for this rejected solver experiment.

## Archived replay

The complete candidate including configuration, stable manifold metadata, serial/parallel call plumbing, math test and acceptance is [manifold_block_normal.patch](../../tools/experiments/manifold_block_normal.patch), SHA256 `ee98f08a7a8487553f292ce8e7d2fb08fe20da67bcb31ac336d76b4140087332`. It applies to the restored preblock source including the Round178 phase and80iteration diagnostic. `git apply --check` passed after restoration. Preblock snapshots were reconstructed from a2c21a7 plus the captured Round178 source diff, and all four source files were restored byte-for-byte from those snapshots. The patch is a rejected experiment, not a shipped public option.

```bash
git apply --check tools/experiments/manifold_block_normal.patch
git apply tools/experiments/manifold_block_normal.patch
cargo test --release --offline --locked -p quench3d-physics --lib \
  manifold_block_independent_kkt_and_empty_support -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --test stack_precision \
  manifold_block_normal_sustained_precision_target -- --ignored --exact --nocapture
```

The math command is expected to pass; the precision command must compile and is expected to fail as recorded. Use an isolated checkout for replay. The preblock stack helper prints allbody maximum drift for the original scalar32layer case281.158m; the different4.073655m drift in the older alternating candidate report belongs to that separate rejected candidate, not a changed baseline here.
