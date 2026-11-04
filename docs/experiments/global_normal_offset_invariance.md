# Normal-offset invariance: registered eligibility correction

Registered before removing the gate. Isolated P3 checkout `/home/gh503/Documents/infra-dev/quench3d-global-normal-replay`; main production untouched.32x16 controlled diagnostic showed force-only320/320coarseaccepted, P3only20accepted atstep1 and300rejected atsteps2..16. Exact reason run formally collectedexit0: all300noncoplanar, firststep2round1floorpair0,1 armnormalspread.000041753054m exceeds old1e-5gate. All observed pivots were healthy (minimum.33333336, fromstep1); no pivot failure explains those rejections. P3step8originalpair3,4 independentSAT.005017632096354641matches archived failure. Old labels pair01/pair12 in trace refer to the explicitly retargeted originalpair3,4/body4,5, not the literal label digits.

Mathematical reason: for sharednormal n, (r+alpha*n) cross n=r cross n. Thus normal Jacobian, true normal operator and force/torque coarse span are unchanged by independent normal arm offsets. Coplanarity of arms is unnecessary for this coarse representation. This is an applicability correction, not tolerance widening or regularization.

Test before fix: twointerfaces/eightcontacts, add different normal offsets equally to ra/rb percontact, keeping rb-ra fixed. Assert true normal product and basis invariant, coarse correction nonNone. Existing gate must successfully compile then fail this test. Friction Jacobian is not invariant under those hypothetical geometry changes; the actual fix changes no contact geometry or friction arithmetic.

Only remove coplanarity eligibility condition. Keep same-normal compatibility, static-root unbranchedchain, dot.999, QRrank/pivot1e-9, trueM/inertia, outer freshPG withoneAp/20rounds, position8, margin/slop.005, originalmaterials/sleep/60Hz. Full acceptance remains4/12/32each1800steps30seconds, allworldsfinish then independentSATpair/floor<=.00501m assertion. First run independent dense and offset tests, then fullprecision. Failure means archive/reject, no performance. Success still requires inherited rollingfriction/serialmultisland/defaultbits/actualparallel/math/cost checks before shipping; main API stays absent until parent authorizes production integration.

Evidence diagnostic jobs `bash-0a1354fbfdd24426` and `bash-2bebc3c9725044fb` both formally collectedexit0. Logs/counters under main target/tmp/round184-wrench-early. These16step observations are not30second acceptance.

## Measured correction: rejected

Old-gate offset test successfully compiled59.43s then failed at coarseNonNone assertion(exit101), after normal_product and basis invariance comparisons passed. Job `bash-4a140ec1ee41480f` formally collected. After deleting the unnecessary coplanarity gate, both offset-invariance and twointerface dense-reference tests passed(exit0;compile64s). Extra offset test assertions independently compare normal Jacobian rows and arbitraryspin normalvelocity exactly for dyadic nY geometry; empirical test uses unitinertia/equalmass, not a full anisotropicCOM suite.

Combined job `bash-f5e49746ff6346f4` formally collectedexit101: fullprecision successfully compiled23.65s and completed all5400publicsteps before assertion. No thresholds, outeriterations, materials,geometry orsleep settings changed.

| Layers | Pair peak mm | Finitefloor peak mm | Precision pass |
|---:|---:|---:|---|
|4|1.354509|1.124463|yes|
|12|1.384938|1.196774|yes|
|32|434.331362|96.197394|no|

12layers first allsleep61 but all1800SATpoststeps run; floorpeak18,pairpeak61, drift.0139384m.32layers first excessstep9pair3,4 depth5.328714104mm withbothfullfootprintsinsidefloor; pairpeak85,floorpeak116,sleep341,drift4.2236355m,whollyoutside0. The eligibility correction is mathematically valid and improves12layers, but does not solve the fullphysicalgoal. The earlier300coplanarityrejections were not a sufficient explanation of the full32layerfailure. No performance or compatibility validation was attempted after rejection. Inherited rolling-friction/serialisland issues remain unresolved and unshipped.

## Archive

[global_normal_offset_invariance.patch](../../tools/experiments/global_normal_offset_invariance.patch), SHA256 `b18effa65591ec31a9e1bf49d3ac0471ec5b4a68aea2a1c7845a595948a7d809`, is incremental against the archived gatedP3 candidate. It includes early16step instrumentation and exactrejection diagnostic history plus the offset test/gate removal. Correct5080da8 isolated application order:

```bash
git apply tools/experiments/global_normal_projection.patch
git apply tools/experiments/global_normal_wrench_basis.patch
git apply --check tools/experiments/global_normal_offset_invariance.patch
git apply tools/experiments/global_normal_offset_invariance.patch
cargo test --release --offline --locked -p quench3d-physics --lib wrench_basis_ -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --test stack_precision \
  global_normal_projection_sustained_precision_target -- --ignored --exact --nocapture
```

Do not separately apply tracepatch: gatedP3 archive alreadyincludes it. Applycheck passed against a reconstructed exact5080da8+candidate+gatedP3baseline. Isolatedfourfiles were then restored byteexact to that gatedP3baseline (original12layer observation), mainproductionsource alwaysunchanged. Evidence `target/tmp/round185-offset-invariance/{stub,math,candidate}.log/.status` and candidate.sha256 remain; unusedhelper/assignment warnings mean no Clippygreen claim. No wholeworkspace gate or productcommit for this failedcandidate.
