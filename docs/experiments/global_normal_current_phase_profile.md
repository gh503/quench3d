# Current coarse diagonal memory solver phase diagnosis registration

Round212 diagnosticONLY,baseexactretained209(after210/211rejectedandallfive209hashesrestored). W8/resetcounters/M120,12/32legacyfalse/globaltrue,same195worldfixtures/nojoint/60Hz/velocity20/position8. Wholeworld.stepelapsedplusglobalnormalphasecounts/ns;notacceptancebenchmark,no5ABBA/no ratio-gateclaim.

Phases:geometry(forcebuildinclusiveendpoints/grouping/LDLsetup),preconditionapply(diag/forceLDL/scatterexcludinglazygeometrybuild),friction,freshgradient/residual/diagfill,direction(includingcache-zcopy/memory-feasibility/descent),trueAp+curvature,lineincludingnext/finiteguard,historycommit,impulseapply. Preconditiongeometryseparatelytimedinsideapply;callerpreconditioninclusivealsorecordedandmustnotdoublecount. Directiondividesbefore/afterpreconditionintervaltoexcludeapply. Rollingoutsideglobal_normal_attemptnotincluded(wholeworldstepincludesit). Legacyphasenszerounlessexistingworldtimingsadded;wholeworldtimingstillvaliddiagnosticcomparison.

InstrumentInstantperphase(no percontact timer);many~2400callsaddsmeasurableoverhead,sofigurescannotbeusedregisteredcostcomparison. Sourcefreeze/singlebuilder/archivepatch+binaries/rawsourcehashes,thenrestoreexact209includingtemporaryWorldAPI/examplebeforefuturecandidate. Mainproductionunchanged.

## Actual exclusive/inclusive phase diagnosis

Job bash-6335081d7588444e formallycollected0,compile18.39s. [global_normal_current_phase_profile.patch](../../tools/experiments/global_normal_current_phase_profile.patch),SHA256 `6aff900c8c8c6d82e0a2d3f72f67150ed82000ca9afb465243c8bbd4727bca33`,applycheck0againstrestored209. PrepfirstscriptfailedEXIT1ontargetloopanchorbeforewritinganysource;readactualanchorandcorrectedscript,no benchmarkraninfailedprep.

|Phase ms|12 global|32 global|
|---|---:|---:|
|Wholeworld.step|24.632808|17.612141|
|Legacywholeworld.step|19.118289|50.401127|
|Geometry inclusive endpoints/grouping/forceLDL|.909219|.734112|
|Preconditioner inclusive geometry|2.040603|1.442442|
|Preconditioner exclusive geometry (subtraction)|1.131384|.708330|
|Friction|9.467170|6.567363|
|Freshgradient/residual/diagonalfill|1.126191|.898887|
|Direction|.600179|.463309|
|TrueAp and curvature|1.969998|1.303623|
|Line/next/finiteguard|.996893|.687514|
|Historycommit|.075400|.063854|
|Normalimpulseapply|1.451091|1.099189|

12geometry120calls,allotherphases2400;32geometry31calls,otherphases620. Timersexcludeearly-returnphasecompletion;thisfixtureallrecordedphasescountsame. Preconditioncallerincludesgeometryandzcopy;doNOTsumgeometryplusinclusiveprecondition. Exclusivepreconditionreportedbydifference. Rolling/bodylayout/etcandtimer-bookkeepingremainwholeworldonly. Legacyphasecountersallzero,notbreakdownoflegacyvelocity.

OnlydiagnosticW8/resetM120singlefalse/trueperscale;no5ABBA/no ratioacceptanceclaim. Intrusivetimersaddoverhead;figuresarenotcostbenchmark. 12entiregeometry<.91msandmemorycommit.0754mscannotindividuallyclose~1.6msremaining209registeredgap. Friction/trueAp/normalimpulsearelargeropportunitiesbutmustpreserveactualoperator/frictionscalarexpressionsorregistercontrolledmathwithallphysicsgates.

Binarymanifest/source/rawunder /home/gh503/Documents/infra-dev/quench3d/target/tmp/round212-current-profile/. Archivedthenrestoredexact209solver/world,temporaryAPI/exampledeletedafterabsolute-targetcheck,allfive209costsourcehashchecksPASS. Mainproductionunchanged/alljobsformallycollected/no runningbuilder/performancegoalstillunmet.
