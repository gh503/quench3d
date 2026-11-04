# Global normal rolling friction: compatibility registration

Registered before fix. Isolatedpassing190candidate retained; mainproductionunchanged. Currentglobalhelperomits scalarrolling stage. Fixedcounterexample: staticA/dynamicB invM1/Iidentity,nY,ra0,rb=-Y,linB=-Y,angularB3X,tangentfriction0,rollingcoefficient.2,normalimpulse0,kN1,bias0. Twoouterrounds shouldyieldnormalimpulse1,lin0,angular2.8X;zero rollingcontrol3X. Secondnormalgradient0forcesno-normal-progressroute,whichmuststillapplyrolling. Existingcandidateexpected3X atregistration(beforemeasurement),naivepost-returnfixwouldonly2.9X.

Implementationcopy scalarrollingblockexact into globalwrapper epilogue: global_normal_round calls innernormalattempt thenrollingforeachcontact regardlessinnerreturn. No originalscalarfunctionchanges,dt/radius/rollingcache/newnormaliterations/products. Sharedserial/parallel globaldispatchsamewrapper. Testrollingfixture/control,190directionmath2,188localmath,thenfixed4/12/32each1800actualsteps30secondindependentSAT<=.00501m. Defaultfixture rolling0 shouldretain190precisionresults; reportactual. Noperformanceuntilremainingcompatibility. Fullshippingrequiresmultipleislandparallel/defaultbits/physicalmath/setupcache/cost/fullgate,not justthisstage.

## Actual compatibility and precision result

Stubjob bash-026de5119ae34f8b formallycollectedexit101:successfulcompile55.17s,rolling.2spin3Xfailedexpected2.8X afterzerorollcontrolpassed. Implementedwrapper callsnormalattemptthenexactcopiedscalarrollingblockforeachcontact,soinnerearlyreturncannotbypassrolling. No originalscalarfunctionbodywasmodified.

Finaljob bash-55fe80fd57354fc9 formallycollectedexit0:rolling/controlfixture1passed,directionmemory2passed,additivelocalmath1passed,andfull3world5400publicsteps precisionpassed. Precisioncompile22.11s. Exactreportedpeaks match190:

|Layers|Pair peak mm|Finitefloor peak mm|Precision pass|
|---:|---:|---:|---|
|4|1.349560|1.172217|yes|
|12|1.782923|1.300369|yes|
|32|1.471930|.958301|yes|

Reportedpeaksteps,firstallsleep33/223/38,driftandwhollyoutside0alsoequal190. This verifiesunchangedreportedfixedfixturedatawithrolling0,notageneralbitdigest. All1800poststepsperworldSATchecked. Rollingfixturesecondroundnormalstallsbutspincontinuestodamp2.9→2.8,zerocoefficient3remains. Compatibilitycoverageisthisspecificnormal-progress/stallandzerocontrol,notgeneralrollingangularmomentumsuite.

Candidate source retainedpassing190+rollingcompat,frozenuntilnextstage. Mainproductionunchanged. No actualparallel,defaultoffdigest,non-diagonalinertia/frictioncoverage,setupcache,costbenchmark orfullgateclaim. Existingwarningsremain,noClippygreen.

## Replay archive

[global_normal_rolling_compat.patch](../../tools/experiments/global_normal_rolling_compat.patch),SHA256 `7d66dd50fc868a8b09056fbf72a3f85618442342cad49d8031e4f1daad7c2897`,incrementalagainstpassing190archive. Applycheckpassedonexactreconstructed190baseline. Applyorderin5080da8isolatedcheckout: originalglobal_normal_projection→global_normal_wrench_basis→global_normal_offset_invariance→global_normal_force_local_torque→global_normal_conjugate_direction→thispatch. Do NOTaddseparatetrace/boundaryarchives alreadyincluded. Run:

```bash
cargo test --release --offline --locked -p quench3d-physics --lib global_normal_rolling_two_round_stall -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --lib normal_direction_memory_ -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --lib global_force_local_torque_three_interface_dense_masked -- --nocapture
cargo test --release --offline --locked -p quench3d-physics --test stack_precision \
 global_normal_projection_sustained_precision_target -- --ignored --exact --nocapture
```

Evidence under main target/tmp/round191-rolling-compat:stub.log/status,pattern-specificmathlogs/status,candidate.log/status/sourcehash. Alljobsformallycollected,norunningbuilds. Parentcontrolsproductintegrationafterremainingcompatibilityandcostchecks.
