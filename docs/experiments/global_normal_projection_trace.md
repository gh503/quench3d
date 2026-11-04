#12-layer global normal candidate: bounded observation

This is observation of a rejected candidate, not a solver fix or accepted precision capability. Main production remains restored; experiment runs in isolated checkout `/home/gh503/Documents/infra-dev/quench3d-round182-global-normal-replay`, branch `round182-global-normal-replay`, from5080da8 plus [candidate patch](../../tools/experiments/global_normal_projection.patch). A second unused candidate-only checkout `/home/gh503/Documents/infra-dev/quench3d-global-normal-replay` remains; no cleanup or source deletion was performed.

## Fixed diagnostic

Original12layer fixture, margin.005/slop.005/velocity20/position8,60Hz480actualpublicsteps (8second diagnostic, NOT30second acceptance). Geometry/materials/sleep/operator/coarse eligibility/direction arithmetic are unchanged from the archived candidate. Detailed solver observations cover steps400..440, all20rounds; coarse accepted/reason observations cover1..480. Extra gradient gathers/SAT/logging are observation only, add no solverAp and are not performance measurement.

The independent f64 OBB oracle was copied into cfg(test) code.480step peak pair `0.005135424688967494` and floor `0.0045088037607438625` match the archived candidate exactly within1e-12. This validates those measured peaks against the prior run, not a claim that every intermediate public state was bitwise compared. Full30second acceptance remains the separate failed experiment.

## Findings

- Coarse chain accepted in ALL20rounds for every step1..476:9520accepted observations. No coarse rejection occurs before first excess. Steps477..480 follow allbody sleep and have no solver rounds. Chainlength12, selected pivot ratios near.5. This excludes a coarse eligibility drop in this measured trajectory.
- Steps400..440 have820beforefriction,820freshgradient,820Ap,820alpha and820afternormal records. No missing-stage early return occurs. Each observed round used oneAp; aftergradient is a separate gather.
-458rounds contain a negative-direction bound candidate.457take the full quadratic-optimal step; oneis bound-limited:step414round12, bodypair1,2,lambda=.0010118071222677827,p=-.0003080634537224997,ratio alpha/alphaopt=.8324942472605782. This is not repeated nearzerostep locking. Other rounds without a negative direction have no finite bound candidate. A broad inference that bounds never affect the trajectory would be wrong; sustained bound trapping is not supported here.
- At first excess step419round20, original pair0,1(body slots1,2) has deepest preintegration contact penetration4.993935mm, below5mmslop. After friction its closing normal velocity is.0021426405m/s; after global normal step it is.0022420855m/s; after relaxation.0017311447m/s. Position preserves realvelocity; that below-slop point receives no own correction.4.993935+.0017311447/60m predicts approximately5.022787mm, close to the independent poststep SAT5.022749019mm. Rotation/geometric measurement account for the small difference; no exact scalarposition identity is claimed.

At the same round, globalKKT infinity norm is.0021068763beforefriction,.0021426405freshgradient,.004033381after-normal,.0034193613afterrelax/position. Alphaopt=alpha=1.3496187276949076, so first-excess final round is not boundlimited. AQP descent step need not decrease KKT infinity norm monotonically; this increase alone does not prove a mathematical bug. Remaining pointwise closing velocity plus step-start slop gating explains the immediate threshold crossing. Whether the force-only coarse basis lacks the relevant rocking modes remains a hypothesis requiring a controlled algorithm change; it is not established causally by this observation.

## Replay and evidence

[global_normal_projection_trace.patch](../../tools/experiments/global_normal_projection_trace.patch) is an incremental cfg(test) observation patch **against the already applied candidate**, not main. SHA256 `b72cdb5d11fbcae0e712f1012bdb7e6b291921522d4f15a8df1ed4ac563939ab`. Apply order in an isolated5080da8 checkout:

```bash
git apply tools/experiments/global_normal_projection.patch
git apply --check tools/experiments/global_normal_projection_trace.patch
git apply tools/experiments/global_normal_projection_trace.patch
cargo test --release --offline --locked -p quench3d-physics --lib \
  global_projection_12_layer_bounded_observation -- \
  --exact world::tests::global_projection_12_layer_bounded_observation --nocapture
```

Incremental apply-check passed against the second uninstrumented candidate checkout. Job `bash-286531ee11d34f14` successfully compiled in55.72s and was formally collectedexit0. Original two warnings (copied friction final assignment and copied unused oracle helper) are recorded; no Clippygreen is claimed. Source hashes/log/status,15870line trace and bounded summaries remain under main `target/tmp/round182-global-replay/`. `summary.md` contains selected rounds; `counts.md` contains fullwindow stage and bound counts. No performance, wholeworkspace gate, packaging or commit was run for this observation. No additional algorithm parameter was changed.
