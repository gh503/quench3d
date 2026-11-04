# Default false canonical state compatibility registration

Round193, registered before baseline generation. Frozen baseline0409ac5(main production physics same5080da8), candidate passing190+191+192 isolated implementation. Common cfgtest-only exporter/fixtures must be byte-identical in both source trees, with identical ContactCache test access logic and mixed_world visibility. No common-code reference to candidate-only flag or direction memory. Implicit default false. Baseline first generates actual expected binary state; candidate compares complete per-step bytes, never replaces expected on mismatch.

Fivecases from existing fixtures: six columns×six boxes, serial/parallel2 each240steps; twenty columns×five boxes plus bottom-to-layer2 Distance percolumn, serial/parallel2 each180steps; existingmixed_world400steps/defaultsleep/mixedshapes+Hinge/Distance. Native hardware/actual workers>=2 mandatory for each parallel fixture, skip is notpass. Same Rust/toolchain/lock/features/profile/environment, separate targetdirectories, one builder sequentially. Exact binaries/source/commonexport hashes recorded.

Canonical little-endian explicit f32::to_bits preserves signedzero and NaNpayloads. Field tags and record lengths allow first differing step/field/word diagnosis. Export legacy physical body state including current/previous transforms, actual/pseudo velocities,forces/inertia/material/sleep/handles/generations; public contacts/manifolds/fullsolvercontacts; ContactCache keys/ranges/impulses; jointslot cachedimpulses; orderedevents/sensorevents/previous touch/sensorpairs/dropcounters/free slots. Exclude added candidate flags/direction memory, parallel workspaces/capacities/timings. Fullbytes comparison, not finalFNV/digest-only. No performance/cache/product change authorized in this stage; source testinstrumentation only, main production unchanged.

## Instrumentation and measured baseline

Commonmodule SHA256 `bc4c3b6ba628117ef0b6608efc5ba2d675504d06bb4785bd5f7fb9f6c5e1291b` identicalinbothisolatedtrees;ContactCacheaccessoridenticalandsamevisibilitychangeexistingdigestmixed_world. Exporter/test-onlyarchive [global_normal_default_bits.patch](../../tools/experiments/global_normal_default_bits.patch),SHA256 `da6d0aad661ac3f19650f92d269a148b93efec50f28375194edd21275f2dfe72`,reverseapplycheckpassedoninstrumented0409ac5baseline. Patchcontextsoriginatebaseline;candidateintegrationusesthesameaccessor/modulelogicwhileworld/solverlinecontextsdifferdueto192. No physicalalgorithmchanges.

InitialbaselinecompilefailedmissingchildmoduleBodyBuilderimport,formallycollected101;fixedsameimportbothsources. Thiswasinstrumentationerror,notbehaviorred. Baselinegenerationjob bash-fccba87f16d24769 formallycollectedexit0,compile52.29s/test5.04s,hardware4. Actualpayload:

|Case|Public steps|State bytes|Actualparallelsteps|Max threads|
|---|---:|---:|---:|---:|
|Sixcolumns serial|240|152146634|0|0|
|Sixcolumns parallel|240|152146634|235|2|
|Distancecolumns serial|180|279025068|0|0|
|Distancecolumns parallel|180|279025068|177|2|
|Mixedshapes/joints/defaultsleep|400|69273244|0|0|

Total1240stepstates,931616648payloadbytesplus8byteperstepframing. Candidatecomparisonisrecordedbelowonlyafterformalcompletion. Expectedfilesweregeneratedonceafterinstrumentationcompiled;candidateopensreadonlyandcomparesfullbytesperstepplusEOF,neverregeneratesexpectedonmismatch.

Explicitcoveragebodycurrent/prevpositionrotation/scales,linear/angular/pseudovelocities,force/torque,mass/inversemass/localinertias/localCOM,materialfloats/combineenums,CCD/sensor/type/userdata/sleepflags/timer/submerged,generations/presence/freevectors;allSolverContactfields,publiccontactrepresentatives/manifoldpoints,ContactCachekeys/ranges/impulses,jointslotimpulses/breakthresholds/clearance/collisionflag,orderedPhysicsEvents/sensorevents/previouspairarrays/droppedcounter,worldstep/accumulator/version/gravity/islandmembership/stats. Staticshape/compoundconfiguration,collisiongroupsandjointdefinitionsarefixedbyidenticalfixture/commoninputsource,notcanonicalexportedfieldclaims;otherprivateworkbuffers/configflags/newnormalmemoryareexcluded. This suiteprovesdefaultpathcompatibilityforthesefiveinputs,nottheopt-inphysicaloperatororallpossibleworldstates.

Evidenceabsolute root `/home/gh503/Documents/infra-dev/quench3d/target/tmp/round193-default-bits/`:baseline2.log/status/sourcehash,candidate.log/status/sourcehash,manifest.json,expected/*.bin. Separatefrozenbinarytargetsdefault-bits-baseline-193/default-bits-candidate-193,sameRust1.99release/defaultfeatures/offlinelocked/Cargo.lock/RUSTFLAGS. Manifestrecordssource/commonarchive/expectedhashesandbinarypaths. Mainproductionunchanged.

## Actual candidate comparison

Candidatejob bash-20157cd7f12a4e5c formallycollectedexit0,compile56.43s/test4.66s,hardware4. Allfivecases/all1240per-stepcanonicalstatesmatchthefrozenbaseline's931616648payloadbytesexactly,includingrawsignedzerobits. Parallelcoverageexactlymatchesbaseline:columns235actualparallelsteps/maxthreads2,joints177/maxthreads2;no skip. Thisisa completebytecomparisonofeachexportedrecord,notdigest-onlyverification.

Commonmodulehashidentical,samecacheaccessorandmixedfixturevisibility,bothcompiledsourcehashfilesverifiedaftercompletion. Manifestnowincludesexactbaseline/candidateexecutablepathsandSHA256,allphysicsRustsourcefileSHA256s/Cargo.lockforeachsource,expectedfileframinglength/SHA256,toolchain/env/profile/features. Noexpectedregenerationaftercandidatecomparison;noarithmeticfixwasneeded. Sourcepreservedpassing192+cfgtest193instrumentation;mainproductionunchanged. All193jobsformallycollected,norunningbuilder. Opt-inphysicaloperatorcoverage,finiteatomicityissue,setupcache/cost/wasm/fullgate remainfuturestages.
