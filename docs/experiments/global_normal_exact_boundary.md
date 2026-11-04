# Exact active-boundary update: registered candidate

Registered before algorithm change. Isolated P3withoutcoplanaritygate checkout quench3d-global-normal-replay; main production unchanged. Prior correctedP3failed32layer30secondprecision although coarse accepted throughfirstfailure and blockpivots healthy. Earlytrace shows165boundlimitedrounds/237finiteboundrecords,minratio7.358688580048791e-18. Witness step5round9 lambda=1.0842021724855044e-19,p=-.02188263444880072,g=-.06959611922502518; coupledSPDdirection may be negative despite negativegradient, with exactzero-only feasibleclip permitting a tinypositivecoordinate to throttle the globalstep.

Candidate changes ONLY line update at a mathematically active bound: when alpha equals computed -lambda_i/p_i withp_i<0, explicitly store lambda_i=0, including alphaopt==bound and exact floating equality ties. This is not epsilon thresholding or arbitrarysmalllambda removal. Othercoordinates retain original max(0,lambda+alpha*p) arithmetic and f32storage; actualdelta=storednew-storedold applies throughtrueM/worldinertia. No extraAp,inneriterations,backtracking,material/geometry/friction changes. Outer20rounds/position8/relax1,60Hzmargin/slop.005 remain unchanged.

Before fix extract shared line-update helper preservingoldmaparithmetic and addactualrecord normal-bound arithmetic counterexample that compiles then fails exactzeroassert due positivecancellation residue. After fix run offset/dense/boundarymath then ALL4/12/32layers each1800actualpublicsteps30seconds, everypoststepindependentallpair/floorSAT<=.00501m, allworldsfinishbeforeassert. Local mathsuccess or16steptrace cannot replace fullacceptance. If fail archive/reject; if pass inheritedrollingfriction/serialmultisland/defaultbits/actualparallel/physicalmath/costverification stillrequired before shipping. No performance beforeprecisionandcompatibility.

## Actual result: rejected

Counterexample inputs are drawn from trace step2round5:lambda=.028098056092858315(bits0x3f9cc5bca0000000),direction=-.04463107489057702(bits0xbfa6d9e25e14a96c). The test chooses alpha=-lambda/p=.6295626121877398 to exercise the mathematical boundary; that recorded originalround had ratio1 and did NOT actually hit thisbound. Thus this is a counterexample using real recorded inputs, not a claim that this exact event generated the later tinyimpulse. Oldhelper stores3.46944695e-18; doublingbothinputs gives6.9388939e-18 for an exacttie. A halfstep control preserves the old arithmetic.

Stub job bash-16fe5c9c68a84359 successfully compiled56.09s and formally collectedexit101 due exactzeroassert(left[3.469447e-18,6.938894e-18],right[0,0]). Afterfix, boundarytest passed(exit0,compile56.44s), and twooffset/dense tests passed(exit0,incremental.08s). Combined job bash-d9dd39c4121d44a0 formally collectedexit101 after successfulprecisioncompile21.04s and all5400steps completed.

|Layers|Pair peak mm|Finitefloor peak mm|Precision pass|
|---:|---:|---:|---|
|4|1.354509|1.124463|yes|
|12|1.384938|1.196774|yes|
|32|370.175996|155.610624|no|

4/12results equal the preceding no-gatecandidate.32layers first exceedsstep7,originalpair25,26 depth21.427479431mm,bothfullfootprintsinsidefloor; peakpair109/floor108,final30sleep2awake,centerdrift121.0466m,3317whollyoutsidebody-step samples. Firstexcess is earlier than priorstep9; exactboundary correction does not solve fullphysics. This observation does not establish that tiny-boundary residuals were the sole cause of the priorfailure. No newpreconditioner,threshold,sweep orperformance test was attempted after failure. Inheritedrollingfriction/serialisland/physicalmath/compatibility limitations remain unresolved; mainproduction is untouched.

## Replay archive

[global_normal_exact_boundary.patch](../../tools/experiments/global_normal_exact_boundary.patch),SHA256 `973d8ee1d4cd85117892da4fa1919e55f46317b12150c2b25d14580b0bcd0b9e`,incremental against archived no-gateoffsetcandidate. Includes boundaryhelper/test and16stepprobe matching assertion from subsequentobservation; the16steptest is diagnostic for the old no-boundary trajectory and may intentionally fail with this changedalgorithm, so use the commands below for this candidate's measuredmath/precision replay.

```bash
# Isolated5080da8 checkout, patches copied from main archive.
git apply tools/experiments/global_normal_projection.patch
git apply tools/experiments/global_normal_wrench_basis.patch
git apply tools/experiments/global_normal_offset_invariance.patch
git apply --check tools/experiments/global_normal_exact_boundary.patch
git apply tools/experiments/global_normal_exact_boundary.patch
cargo test --release --offline --locked -p quench3d-physics --lib projected_line_exact_active_boundary -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --lib wrench_basis_ -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --test stack_precision \
  global_normal_projection_sustained_precision_target -- --ignored --exact --nocapture
```

Do not additionallyapply separate tracepatch: wrencharchive includes it. Reconstructed5080da8+candidate+wrench+offsetbaseline acceptedincremental applycheck. Afterarchiving, isolatedfourfiles restoredbyteexactto that no-gatearchivedbaseline. Sourcehash/log/status under main target/tmp/round187-exact-boundary. Alljobs formally collected,no runningbuilds. Unusedhelper/assignment warnings remain; no Clippy/fullgate/productcommit claim.
