# Global normal contact-wrench coarse basis: registered experiment

Status: implemented, measured and **rejected**. Registration below preceded the algorithm change. Isolated sources have been restored to the force-only candidate plus its observation patch; main production remained unchanged. Experiment is isolated in `/home/gh503/Documents/infra-dev/quench3d-round182-global-normal-replay` from5080da8 plus the archived global-normal candidate and observation patch. Main production source remains unchanged. Original force-only coarse basis already produced a genuine compiled full30second precision failure at12layers, while4/32passed. This new experiment tests the hypothesis that adding two rocking directions improves convergence; it does not assume that hypothesis is proven.

Change ONLY coarse preconditioning. Generate deterministic orthonormal contact-space columns in order from1,(ra cross normal) dot tangent1,(ra cross normal) dot tangent2. Use stable twice modifiedGramSchmidt and fixed relative rank threshold1e-9, retaining0..3 independent columns without softening. Preserve samepairnormal/coplanarity compatibility, trueM/worldinertia and fullcontact A. VariableblocktridiagonalT=P^TAP uses full geometry and positive relativepivot1e-9maxdiag with wholechain diagonal fallback if invalid; no regularization. Apply additive diagonal plus M_F P T^-1P^T M_F, a symmetric masked approximation, not an exact inverse of the restricted operator. Setup/factor remains perround and is counted honestly. The implemented preconditioner explicitly symmetrizes coarse diagonal/offdiagonal geometry terms as0.5(T+Tᵀ); this changes only the preconditioner, not the true operator. Small block inverses are derived through Cholesky and triangular solves, then used in block Schur elimination; no claim of avoiding explicit small inverses is made.

All outer projected-gradient/friction/normal operator/bound rules remain unchanged:20global rounds with at mostoneAp each, position8, relaxation1, margin.005, slop.005, originalmaterials/sleep/geometry/60Hz. No support-angle threshold or otherparameter change accompanies the basis change. Fixed acceptance is all4/12/32layers each1800actualpublicsteps30seconds, everypoststep independent allpair/floorOBBSAT<=.00501m; allthree worlds complete before assertion. Do not replace this with a snapshot/math result, shorten duration, enlarge floor or widen tolerance.

Before fullprecision, independent multi-contact/offdiagonal dense-reference tests must validate orthonormalP, both rocking moments, trueM/offsetgeometry denseJ M^-1J^T projectedoperator againstblocksolve, positivity and degenerate rank handling. The existing419observation can measure pointwise closing/KKT but remains diagnostic, not acceptance. Algebraic descent need not monotonically lowerKKTinf.

If anyscale fails, reject and archive an incremental patch relative to the original candidate (document required application order), restore isolated priorstate, and do not benchmark. Only if allprecision scales pass should compatibility work proceed: inherited rolling-friction omission, serial multiisland alpha discrepancy, actualparallel/defaultbits and complete physicalmath checks remain blockers before shipping. Conditional performance retains12/32scales,8warmup+120measuredsteps,fivetrials alternatingorder,<=15%medianregression eachscale including allsetup/work. No productionfeature/performance claim exists before these checks. Parent controls mainfullgate/commit.

## Actual measurements

Independent eightcontact/twointerface test first successfully compiled and failed against the old force-only preconditioner: projected dense residual.6304761931198797,exit101. After implementation it passed(exit0). The test subtracts the toy positive diagonal contributionr/2 from the total preconditioner output before checkingPᵀAcoarse=Pᵀr; its diagonal2 is a supplied toy preconditioner diagonal, not the physical contact diagonal. Independent denseJ includes multiple contact offdiagonals and both rocking directions on an asymmetric rectangular patch with unit dynamic inertia; this is not complete offsetCOM/non-diagonal-inertia verification. P orthogonality/unit norms and two-point rank2 are checked. Local algebraic green does not imply fulltrajectory precision.

Allthree worlds then completed1800steps each before the unchanged acceptance assertion failed:

| Layers | Pair peak mm | Finite floor peak mm | Precision pass |
|---:|---:|---:|---|
|4|1.292889|1.102165|yes|
|12|4.580928|3.442951|yes|
|32|433.889511|158.910009|no|

12layers first allsleep49; pair/floor peaks bothstep8, but all1800poststeps still ran independentSAT. This improves12layers relative to the force-only candidate.32layers first exceeds atstep8, original pair3,4 depth5.017632096mm, both complete footprints insidefloor. Pair peak84,floor129; final31sleep/1awake, horizontal center drift40.7253m and1587whollyoutside body-step samples. This loses the prior force-only32layer pass. No scale-specific mode switch, threshold change or parameter mixture was attempted. Whether block fallback, boundary limitation or other dynamics caused that reversal is unmeasured; no causal diagnosis is asserted.

Mathstub job `bash-6927504961474bf1` formally collectedexit101 (compile53.58s). Combined math/candidate job `bash-be0b281a610f44cb` formally collectedexit101; math.status records0(compile53.75s),candidate.status101(compile18.94s; measured diagnostic1.35s). Evidence remains under main `target/tmp/round182-global-replay/wrench-*`. Warnings include inherited copiedfriction unused assignment, now-unused force-only helper, and copied oracle unused helper in tests; no Clippygreen claim.

Rejected after precision. No performance, actualparallel, rolling-friction fix, defaultbits, fullphysics compatibility or extra sweep was run. All previously documented candidate limitations remain. Original projected-gradient iteration budget, trueM, support-angle threshold and material/slop settings were unchanged. Main production source was never modified by this experiment.

## Archived replay

[global_normal_wrench_basis.patch](../../tools/experiments/global_normal_wrench_basis.patch), SHA256 `60f7b399b84ae63aec189dc8bb6b0fc286d91d3be4ed2ab5ff41d7fc635ca240`, is a complete incremental patch **against the original global-normal candidate**. It includes the bounded observation instrumentation and new coarse basis/math test; do NOT also apply the separate trace patch when replaying this archive. Application order in isolated5080da8 checkout:

```bash
git apply tools/experiments/global_normal_projection.patch
git apply --check tools/experiments/global_normal_wrench_basis.patch
git apply tools/experiments/global_normal_wrench_basis.patch
cargo test --release --offline --locked -p quench3d-physics --lib \
  wrench_basis_two_interface_dense_reference -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --test stack_precision \
  global_normal_projection_sustained_precision_target -- --ignored --exact --nocapture
```

Incremental apply-check passed against an uninstrumented originalcandidate checkout. Math should pass and fullprecision should compile then fail at32layers as recorded. Isolatedworktree solver/world were restored exactly to originalcandidate plus the separate trace patch after archiving; no worktree cleanup/delete was performed.
