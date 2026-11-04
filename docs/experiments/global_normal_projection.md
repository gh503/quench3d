# Global normal projection: registered candidate

Status: **implemented, measured and rejected** against the unchanged registration below. Exact implementation baseline `5080da8`.32layers30seconds passes in this experiment, but12layers fails, so the candidate is not accepted or shipped. All four production/test source files were restored byte-for-byte to5080da8, retaining its previously committed diagnostics. This record is not a delivered product capability.

## Candidate and immutable acceptance

New runtime option `global_normal_projection: bool`, defaultfalse. Defaultfalse executes the historical per-point velocity solve with unchanged floating arithmetic/order. Enabled candidate uses TRUE effective inverse mass and world inverse inertia for every contact response and every applied delta. Equal/opposite normal impulses are applied to both bodies; no dynamic mass freezing, shock propagation, artificial support anchors or physical approximation is allowed. No changes to materials, sleep, timestep, position solver, relaxation, slop or speculative targets.

Before algorithm implementation, add configuration/call plumbing and the fixed acceptance while enabled behavior is still scalar. The stub must compile and actually run all worlds to produce a real precision assertion failure; compilation errors are not behavior red.

Precision acceptance: original side0.5/mass1 boxes, initial2mm gaps, finite floor half extents(4,.25,4) centered at(0,-.25,0), margin.005, slop.005, velocity budget20, position8, relaxation1,60Hz1800actualpublicsteps30seconds. All4/12/32layer worlds execute fully before final acceptance. Independent f64 OBB SAT checks EVERY body pair and finite floor after EVERY step, including sleeping states: both peak overlaps must be<=.00501m at each scale. Maintain finite state/mass/bodycount/actualstep/contact checks. No smaller layer count, shorter duration, larger floor, wider threshold, extra substeps, altered friction/sleep or larger iteration budget can replace this target.

## First implementation: fresh projected gradient

For each island, matrixfree normal operator A=J M^-1 J^T uses the same true M as impulse application. Accumulated normal impulses lambda>=0 retain existing cache semantics. At each of the20velocity rounds: perform enabled friction updates in deterministic original order; compute FRESH gradient g=Jv_current-target from actual velocities; choose feasible preconditioned projected descent direction; perform at most ONEAp; take the bounded quadratic step; apply actual delta=newlambda-oldlambda with true mechanics. Joint iterations keep their established placement and their changes are included in the next fresh gradient. Do not retain CG history or use stale friction-before gradients/b between rounds.

At lambda_i=0 mask infeasible negative direction components BEFORE checking g dot p<0. If the masked preconditioned direction is not descending, choose feasible diagonal projected descent BEFORE the soleAp. If no finite descent/positive curvature exists, safely consume/skip the scheduled iteration; no extra matvec, backtracking or inner convergence loop. For descending p use alpha=min(-g dot p/(p dot Ap),min_{p_i<0}(-lambda_i/p_i)). Roundoff handling must keep newlambda nonnegative and velocity update exactly consistent with the stored actual impulse delta. No uncounted correction impulse or extra scalar normal pass follows the global step. Zero-lambda separating coordinates remain fixed unless their gradient requires release; positivelambda with positivegradient can retract to0.

## SPD preconditioner and fixed fallback

Prepare a deterministic static-rooted UNBRANCHED support chain coarse representation. P aggregates compatible interface contacts to one normal force coefficient per chain edge using fixed normalized distributions. T=P^TAP uses true mass and inertia, including aggregate lever arms. For an unbranched chain T is tridiagonal because only adjacent interfaces share a dynamic body. Factor by deterministic LDL^T with relative positive pivot threshold1e-9*maxdiag; invalid/nonfinite/nonpositive/too-small pivots fall back to diagonal preconditioning. No eta or other regularization is added to either A or the preconditioner in this first candidate.

Use additive B=D^-1+P T^-1 P^T where D contains positive normal diagonal entries; symmetric free-set masking M_F B M_F preserves positivity on the free subspace. Bound-feasibility clipping and descent check are still necessary. Branching support graphs, rootless islands, ambiguous/cyclic chain topology or incompatible geometry use diagonal preconditioning ONLY. Every true contact remains in the full A regardless of coarse eligibility; no discarded sibling edges or ghost static constraints. Rebuild topology each step so removing support releases it. Do not call the coarse inverse exact for arbitrary branching trees: sibling contacts share a body and introduce nontridiagonal coupling.

## Explicit work and performance budget

Each round has one fresh-gradient gather contactpass, at most oneAproduct(two contactpasses plus onebodypass), onepreconditioner application(twochainpasses), and bounded projection/vector updates. Twenty rounds therefore include about60contactpasses+20bodypasses+40chainpasses, plus friction and once-per-step setup; this is more work than20scalar sweeps and must not be hidden under identical iteration labels. No extra inner active-set/CG iterations or retry products are allowed.

Only if ALL precision scales pass, run production-library cost comparison for12/32layers, eightwarmup plus120measuredsteps, fivetrials per mode/scale in the same release binary, alternating trial order. Include preparation, topology/factorization and all solver work. Median regression must be<=15% for EACH scale. Independent SAT/logging diagnostic walltime is not a performance measurement. Precision failure stops performance and rejects the candidate without threshold or parameter tuning.

## Independent required verification

- SPD boundary trap: B=[[1,2],[2,5]],lambda=(0,1),g=(-1,1),A=I. Raw p=(-1,-3) is descending but gives zeroboundstep. Feasibility clipping yields(0,-3), alpha=1/3,newlambda=(0,0), and true objective decrease for b=(-1,0). Check positive step, nonnegativity and actual QP descent.
- Release cases: A=I,lambda=0,g=(-1,+1) releases only the first coordinate; positive lambda/positive gradient retracts to0; empty active set and mixed boundary changes. Validate actual velocity delta matches stored impulse delta.
- MatrixfreeAp versus independently assembled denseJ M^-1 J^T on small islands, using true inverse inertia and offset COM; equal/opposite impulse response and both tangent-axis torques. IndependentJn/Jt offcenter friction impulse must change normal gradient as dense mechanics predicts, including a signflip case, proving freshg is used. Cover speculative/restitution targets, not an invalid universal zero-energy claim; energy nonincrease only under suitable zero-restitution/zerobias/noexternal assumptions.
- Rootless free dynamic collision/chain total linear and angular momentum, including spin plus orbital angular momentum about a fixed world origin. Static support removal/motion, branch/bridge/cycle fallback, redundant fourcoplanar point PSD modes, degenerate/ill-conditioned diagonals, finite values and no artificial support forces.
- If precision passes, defaultoff bits against prechange golden/reference trajectories; enabled actual nativeparallel with multipleactiveislands and arena holes must assertparallel_used=true, including joint/no-joint routes. A single32tower fallback is not parallel execution evidence. Same shared solver helper is used by all paths.

## Evidence, rejection and scope

Record exact baseline/source hashes, stub and candidate compile/execution results, allworld counts/first excess/footprint evidence, mathematical tests, compatibility and actualparallel outcomes, and conditional performance. Analytic green cannot substitute for30second acceptance. If candidate precision fails, archive complete reproducible patch including option/plumbing/tests and restore production byteexact while retaining prior diagnostic tests and independent peer changes. Clearly list unperformed verification and known limitations. No full gate/package/commit is authorized by this registration alone; the parent controls those stages.

Anticipated implementation scope after source release: solver.rs(configuration, matrixfree workspace/projected iteration/coarse preconditioner/test helpers), world.rs and parallel.rs(call plumbing/workspace ownership and shared iteration scheduling), stack_precision.rs(fixed acceptance via existing independent oracle). No fluid production edits or unrelated UI/source changes.

## Actual measurement and rejection

The scalar stub successfully compiled in18.95s and completed all three worlds/5400steps before a real assertion failure(exit101). Scalar peaks reproduce the existing baseline:4layers1.299087/1.097863mm,12layers3.537737/3.593264mm,32layers436.883123/201.288257mm for pair/floor respectively. Stub job `bash-74bff69ab38f437e` was formally collected.

Two mathematical tests successfully compiled and passed(exit0): the SPD zero-bound direction trap with feasible release cases, and one offset-lever-arm two-body matrixfree response versus an independent scalar effective-mass expression plus linear momentum cancellation. This is a limited two-test scope, not a full dense multi-contact/Jn-Jt/energy/degeneracy/parallel validation. Product scratch mechanics use f32 vectors and true f32 world inertia, with f64 outer coefficients; the analytic response test tolerates1e-6, not exactf64linear algebra.

The implemented candidate then successfully compiled in17.69s and completed ALL three worlds/5400steps before failing the unchanged overall acceptance:

| Layers | Pair peak mm | Finite floor peak mm | Precision pass |
|---:|---:|---:|---|
|4|1.756570|1.549389|yes|
|12|5.135425|4.508804|no|
|32|1.526149|1.125908|yes|

32layers first all slept at step38 and all1800poststeps still ran the independent all-pair/floor SAT checks. Pair/floor peak both occur38, maximum horizontal center drift.007610579m, wholly-outside samples0. This is a real fixed32layer30second precision result, but does not override the12layer failure or establish a general engine guarantee.

12layers first exceeds at step419, original pair0,1 depth5.022749mm, with both full footprints inside the finite floor. Pair peak occurs438, floor peak466, first allsleep476, maximum horizontal center drift.273704681m, wholly-outside samples0. It does not exhibit the earlier32layer collapse/escape, but still violates the registered strict precision threshold. No tolerance, support-angle threshold, sleep, material, margin or iteration budget was changed after measurement.

Combined job `bash-5466875996034318` was formally collectedexit101; math.status records0 and candidate.status records101. Source hashes verified after completion. Logs/status/source hashes remain under `target/tmp/round181-global-normal/`. The new copied friction helper has an unused final vrel assignment warning; no Clippy green is claimed.

## Unfinished and known limitations

Rejected immediately after precision, without a performance benchmark or complete compatibility verification. Actual serial/parallel candidate equivalence and default-off reference digest were not run. Prototype serial global step uses one commonalpha across all supplied contacts while parallel passes island slices, so multipleisland behavior is not yet equivalent. The scalar single-stack precision runs remain valid, but no actualparallel claim is made.

Prototype recomputes contact grouping/chain construction/LDL factors every round,20times per step, rather than the intended once-per-step setup. Thus its work is higher than the registered intended production layout; no timing benefit is inferred from SAT walltimes. Coarse eligibility/fallback ratios, per-round alpha/KKT traces and the mechanism of the12layer first excess were NOT measured. Normal direction alignment and topology conditions can disable the coarse chain, but no specific transition cause is established.

The enabled path copies scalar tangential friction but omits the scalar rolling-friction angular impulse stage. The fixed precision fixture has rollingfriction0, so this does not explain those measurements, but it violates general material behavior preservation and would require correction before acceptance. Other required multi-contact dense/KKT, full angular momentum/offsetCOM Jn-Jt release, topology and actualparallel tests are unperformed. No full gate, package or commit was run for this rejected experiment.

## Archived complete replay

[global_normal_projection.patch](../../tools/experiments/global_normal_projection.patch) contains the complete candidate configuration, solver, BodyView static metadata, call plumbing and mathematical/precision tests against5080da8. SHA256 `006be2bc5437d97b6491849c1e9b40abeaaad92a2c17a0d71ca625d7daffdade`. All four changed source files were restored byte-for-byte from5080da8 and `git apply --check` passed after restoration. Previously committed Round178 diagnostics and other production functionality remain. The public flag does not exist in the restored library.

```bash
# Use an isolated checkout at5080da8 with this archived patch available.
git apply --check tools/experiments/global_normal_projection.patch
git apply tools/experiments/global_normal_projection.patch
cargo test --release --offline --locked -p quench3d-physics --lib global_projection_ -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --test stack_precision \
  global_normal_projection_sustained_precision_target -- --ignored --exact --nocapture
```

The mathematical command is expected to pass; the precision command must compile and is expected to fail at12layers as recorded. Do not count compilation failure as replay of the behavior result.
