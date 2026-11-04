# One previous direction: registered candidate

Registered before implementation. Isolated quench3d-global-normal-replay retains188globalforce+localtorque additiveB and exactactiveboundary. Main production unchanged. Per-step/per-solve memory owns previousfinalfeasibledirectionp,Ap,curvature,andfreemask. Freshafterfriction z=-Bg; candidate d=z-(z dot previousAp/previouscurvature)previousp. This is one-direction A-orthogonalization with restarts, not guaranteedlinearCG under changingfriction/joint right-hand sides.

Fixedreset rules: newstep resets; freemaskchange resets beforedirection; any feasibilityclip of anycoordinate invalidates previousmemory and uses originalz/diagonaldescentfallback; nondescent/nonfinite candidate resets and fallsbackoriginalz/diagonal. Activeboundhit includingequality resets nextmemory. Only finalunclippedstrictdescentfinite direction withpositivecurvature and no activeboundhit is retained. Eachroundcomputes atmostonecurrentAp; priorAp isreused, no extraoperatorproduct oriteration. Defaultfalse scalar arithmetic unchanged. Stateperstep/island,neverstaticglobal/crossstep. Parallelno-joint chunk solveall localstate persists rounds; joint path must carry chunkstate across interleaved rounds, resetonprepare. Existingserialmultislandalphadiscrepancy remains compatibilityblocker pendingprecision.

Mathfirst: nontrivial2x2SPD diagonalpreconditioner2round unboundedreference residualzero; oldsteepestbaselinefails. Changedrhsdescent/restart andfeasibleboundtest. Fullacceptance remainsALL4/12/32layers each1800actualpublicsteps60Hz30seconds,20velocity/8position/1relax,margin/slop.005,originalmaterials/sleep/geometry. Everypoststep independentallpair/floorSAT<=.00501m andallworldsfinishbeforeassert. No extrapasses exceptdirectiondot/vectorwork; setupcosthonest. If failarchive/rejectrestore, no performance. If passstillrequiresrollingfriction/actualparallel/defaultbits/physicalmath/costbefore product. Mathematical2stepCGpropertydoesnot guarantee fulltrajectoryprecision.

## Actual result: allthree precision targets pass

Oldsteepeststub successfullycompiled54.79s then failed independent2x2SPD residual[-.04082125603864739,-.08164251207729466],job bash-7bf5ab0811934479 formallycollected101. Firstimplementedmath run bash-066cb655738a4538 compiled54.38s and passedtwo-stepSPDtest but failed an incorrectlyspecifiedrestartfixture (itsorthogonalization actuallyremoved the negativeboundarycomponent); the testfixture was corrected to prevp[0,1],prevAp[1,3],z[-1,-1],whichgivescandidate[-1,1/3] andcorrectlytriggersboundaryclip/restart. No solveralgorithm changed to satisfy thatfixture. Finalmath has2passed0failed.

Finaljob bash-50c740572bf84ce2 formallycollectedexit0: math2passed,thenfullprecision successfullycompiled19.29s and completed all5400publicsteps. Fixedcriteria unchanged:

|Layers|Pair peak mm|Finitefloor peak mm|Precision pass|
|---:|---:|---:|---|
|4|1.349560|1.172217|yes|
|12|1.782923|1.300369|yes|
|32|1.471930|.958301|yes|

Firstallsleep33/223/38steps respectively; ALL1800poststepsperworldstillranindependentallpair/floorSAT. Whollyoutside samples0inallthree. Horizontalcenterdrift.002919194/.037973004/.007648316m respectively. Pairpeaksteps9/81/8,floor14/19/32. Testwalltime1.44s is diagnostic,notperformanceevidence.

This is the firstexperiment inthissequence to pass allthree registered30secondprecisiontargets together. It is not a deliveredproductfeature. Knownrollingfrictionomission,serialmultipleislandcommonalpha mismatch,actualparallel/defaultoffbitwisechecks,independenttrueM/non-diagonalinertia/frictionmathcoverage,andperroundsetupallocation/costlimits remain unresolved. No benchmark,wholeworkspacegate,packaging orproductcommit wasrun. Nativeparallelplumbing compiled but actualmultiislandexecution/equivalence was not tested. Serialsingle32towerprecision remains valid.

Memory belongs toContactSolver and isresetonprepare,orperislandChunk resetwhenlayoutrebuilds. Parallelworkerclosuresmutablyborrowdisjointchunks,withsamepersistentstate acrossjoint-interleavedrounds. Existingdefaultfalsescalarloopsequencewaspreserved,notyetdigestverified. CachedpreviousAp is reused; no secondnormal_product call wasadded. Warnings inheritedcopiedfrictionassignment/deadhelpers remain,no Clippygreen.

## Archive and nextchecks

[global_normal_conjugate_direction.patch](../../tools/experiments/global_normal_conjugate_direction.patch),SHA256 `6318972e8421b911efadedeaddc4391d2d8f3132723dbc03bc9220c7f1248d6d`,is incremental against archived188additivecandidate and includes189observationchanges plusdirectionmemory. Applycheck passedagainst reconstructedexact188baseline. Isolatedpassingsource isretained,frozen,notrestoredasrejected. Mainproductionsourcealwaysunchanged.

```bash
# Isolated5080da8 checkout with archivesavailable.
git apply tools/experiments/global_normal_projection.patch
git apply tools/experiments/global_normal_wrench_basis.patch
git apply tools/experiments/global_normal_offset_invariance.patch
git apply tools/experiments/global_normal_force_local_torque.patch
git apply --check tools/experiments/global_normal_conjugate_direction.patch
git apply tools/experiments/global_normal_conjugate_direction.patch
cargo test --release --offline --locked -p quench3d-physics --lib normal_direction_memory_ -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --test stack_precision \
  global_normal_projection_sustained_precision_target -- --ignored --exact --nocapture
```

Do not separatelyapply189traceor187boundarypatch:188and190archivesalreadyinclude thosechanges. Old189boundedobservationtestassertionsdescribe theprevious188trajectoryandmayfailundernewdirectionmemory; they are historicaldiagnostics,not thiscandidate'sacceptance command. Evidence main target/tmp/round190-conjugate-direction/{stub,math,math2,candidate}.log/.status andcandidate2.sha256. Alljobsformallycollected; sourcefrozenuntilnextauthorizedcompatibilitystage.
