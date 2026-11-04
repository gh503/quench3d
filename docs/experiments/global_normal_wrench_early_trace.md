#32layer early force/wrench comparison

Diagnostic only: samefixed32layer fixture16publicsteps60Hz20/8, independentf64allpair/floorSAT. No acceptance/performance claim. Mainproduction unchanged. Force-only runs isolated quench3d-round182-global-normal-replay; P3 runs quench3d-global-normal-replay. Onebuilder serially compiled both variants under main target/tmp/round184-wrench-early/build.lock.

| Observation | Force-only | P3 with original coplanarity gate |
|---|---:|---:|
|Coarse accepted rounds|320|20(step1)|
|Coarse rejected rounds|0|300(steps2..16)|
|Bound-limited rounds|2|12|
|Minimum alpha/alphaopt among finite-bound records|.937639108|.020839898|
|Step8 originalpair3,4 SAT mm|1.281617003137|5.017632096355|
|16step pair peak mm|1.360434312|10.881215321|
|16step floor peak mm|1.054557298|8.895126670|

All320rounds in each variant have beforefriction/freshgradient/afternormal stages;16postrelax/position. P3pivot1920entries allstep1,minimumratio.33333336; no failedfactor is observed. Exact rejection followup confirms all300rejections are noncoplanarity before factorization. Firststep2round1floorpair0,1 maxnormalarmspread=.000041753054m on botharms, beyond1e-5gate. No claim that boundary restrictions never matter:12P3rounds were boundlimited and minratio~.02. Step8finalround itself is not limited.

Atstep8round20, P3target deepest prepared point4.2347102mm is below5mmslop; normalstepclosing=.084846064m/s,relaxclosing=.046967894m/s,positionrealvelocityunchanged.4.2347102+.046967894/60m predicts5.017508mm,near independentSAT5.017632096mm. Force-only same target has closing near.000156m/s afterrelax. P3therefore loses itscoarse contribution early; this evidence does not show that fully applied P3longchain algebra is intrinsically worse. Normaloffsetinvariance motivates a separatelyregistered applicability correction, not threshold tuning.

Historical trace labels pair01/pair12 are stale names: probes explicitly select originalpair3,4/body4,5 and poses[3]/poses[4]. ROUND184_STEP8 prints that pair precisely. P3firstexcess matches the archived full30second candidate at1e-12; these16steps do not replace fullacceptance.

Jobs bash-0a1354fbfdd24426(two phases) and bash-2bebc3c9725044fb(exactreason) formally collectedexit0. Logs/hash/status,summary.md and reason-summary.md under main target/tmp/round184-wrench-early. No solverparameter,geometry,iterationbudget ornormal arithmetic changed for theseobservations. Exactreason simply labels existing returnbranch. No mainproduction orperformance work was performed.
