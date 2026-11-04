# Global force plus local torque: registered candidate

Registered before implementation. Isolated quench3d-global-normal-replay from5080da8 plus archived global candidate/wrench/offset corrections, with mathematically exact active-boundary handling retained from rejected187. Main production unchanged.

Fixed additiveSPD preconditioner: D^-1 + M_F Pforce Tforce^-1 Pforce^T M_F + sum_i M_F Q_i (Q_i^T A_ii Q_i)^-1 Q_i^T M_F. Q_i contains only up to2torque columns of orthonormal interfacewrenchbasis after constantcolumn, never the forcecolumn. Globalforcechain is exactly original force-onlycorrection, added once; no duplicate diagonal. Fullgeometry factors and free-mask bothsides; no regularization. A failed localpositivefactor skips onlythatlocalterm, retaining globalforce and diagonal. Ineligibleglobalchain retains diagonal/localterms; allcontacts remain intrueA. Variable localrank0..2, positivepivot1e-9maxlocaldiag. No scale-dependent switch.

Retain exactbound equality handling from187, samefreshPG/oneApperround/20rounds,position8,relax1,trueM/worldinertia,friction,normalcompatibility/supportdot.999,slop/margin.005,materials/sleep/geometry/60Hz. Setup remainsperround, no performanceclaim. No newinnerloops or backtracking.

First threeinterface mixedrank/equalnormal but unequalmass test independently assembles local A_ii and uses analytic1x1/2x2 inverses to isolate the localtorquedelta with free masks. The unchangedglobalforce output is reused to check additivepositivity and masks; this is not an independentdense validation of the entire additiveB. Stub missinglocalterms must compile then fail. After implementation mathgreen then ALL4/12/32each1800actualpublicsteps30seconds,everypoststepindependentallpair/floorSAT<=.00501m,allworldsfinishbeforeassert. Failure archive/reject/restore,no performance; success stillneeds inherited rollingfriction,serialmultisland,defaultbits,actualparallel,completephysicalmath andconditionalcostgates before shipping. Localmath is not fullprecisionacceptance.

## Actual result: rejected

Stub job bash-6059224f2f5e4385 successfullycompiled54.66s then failed missinglocalterm mismatch1.6(exit101). Implemented localterms call the same production-enabled helper tested in math; maskedQRtorquecolumns only, positivefactorfallback skipslocalalone, globalforce originalarithmetic retained. Combined job bash-5d6e1c20c8a74ddb formallycollectedexit101: independentlocalmath1pass0, thenfullprecision successfullycompiled19.11s and all5400steps completed beforeassertion. ExistingdeadoldP3helper/unusedassignmentwarnings remain; no Clippygreen.

|Layers|Pair peak mm|Finitefloor peak mm|Precision pass|
|---:|---:|---:|---|
|4|3.847989|1.160017|yes|
|12|5.163915|3.631626|no|
|32|1.564975|1.117618|yes|

32layers recoveredtrue30secondpass:firstallsleep38,pair/floorpeaks38,drift.008964849m,whollyoutside0,all1800poststepsstillSATchecked.12layersfirstexcess233originalpair3,4 depth5.023546041mm withbothfullfootprintsinsidefloor;pairpeak241,floor555,sleep1200,drift.269803693m,whollyoutside0. This fails the unchanged5.01mmstandard despitekeeping32stable.4layerspairpeak35,floor9,sleep38.

Overallreject. No tolerance/iteration/material/sleep/geometrychange,no extrapreconditionerweights,no performance test orcompatibilityclaim. Exactactiveboundaryhandling is retained in this candidate but fullphysics stillfails. Rollingfriction/serialmultiisland/etc remainunresolved; mainproductionneverchanged.

## Archive and restoration

[global_normal_force_local_torque.patch](../../tools/experiments/global_normal_force_local_torque.patch),SHA256 `058637c8bb864ab0c07fc88b764098f222dd5cd180da3f362d5d5f31818eceab`,is incremental against archived no-gateoffsetcandidate and includes exactboundaryhelperimplementation itself. Do NOT apply rejected187boundarypatch too.

```bash
# Isolated5080da8 checkout with archives available.
git apply tools/experiments/global_normal_projection.patch
git apply tools/experiments/global_normal_wrench_basis.patch
git apply tools/experiments/global_normal_offset_invariance.patch
git apply --check tools/experiments/global_normal_force_local_torque.patch
git apply tools/experiments/global_normal_force_local_torque.patch
cargo test --release --offline --locked -p quench3d-physics --lib \
  global_force_local_torque_three_interface_dense_masked -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --test stack_precision \
  global_normal_projection_sustained_precision_target -- --ignored --exact --nocapture
```

Applycheck passed on reconstructed no-gatebaseline. Isolatedfourfiles thenrestoredbyteexactto thatbaseline. Mainproductionunchanged,alljobsformallycollected. Logs/hash/status under main target/tmp/round188-global-force-local-torque. The inheritedoldP3dense helper tests concern unusedfullP3coarse logic, so the currentactual188mathcommand above is the relevant enabledpreconditioner test. No defaultbits/actualparallel/rolling/cost/fullgate orproductcommit wasrun forrejected188.
