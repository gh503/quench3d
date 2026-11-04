# Additive globalforce/localtorque:12layer first-excess observation

Diagnostic only: same12layerfixture260actualpublicsteps60Hz20/8, no algorithm/tolerance/material/geometry changes. Detailedwindow225..245,all20rounds,targetoriginalpair3,4/body4,5. IndependentSATpairpeak .005163915172886169 exactlymatches rejected188; first233targetdepth.005023546040626. Fullfloorpeakstep555 is outside this260stepdiagnostic: measuredfloorpeak .0033379679911293425 is not asserted equal to30secondfloorpeak.

Job bash-01a29920f7964221 formallycollectedexit0. Logs/hash/status andsummary.md under main target/tmp/round189-force-local-12trace. All420windowrounds havebeforefriction/freshgradient/afternormal observations,21postrelax/position. Forcecoarse accepted420/420(chain12,pivotratioaround.5). Localterms accepted5040/5040interfaceobservations,allrank2. No eligibilitydrop orfactorfailure occurs. Finiteboundcandidate417rounds,10boundlimited,minratio.05243719198; no nearzero-bound locking in thiswindow.

Step233round20 alphaopt=alpha=.3418311167194077,no boundrestriction. Deepesttargetpoint prepen4.978211mm<5mmslop: beforefrictionvn=-.0027943633,freshvn=-.0028160624,afternormal=-.0027186114,afterrelax=-.002736535m/s;positionpreservesrealvelocity. Scalarlinear estimate4.978211+.002736535/60m≈5.0238199mm,nearindependentSAT5.023546040626mm,withsmallrotation/geometricdifference. GlobalKKTinf fresh.0056488793→normal.00533138→relax/position.0050838483. Anotherpoint inthismanifold hasnegativepenetration/speculativebias andlambda0, but no claim thatthiscauses the remainingclosingmode without a controlledtest.

Conclusion limitedto observations: all intendedcoarse/localtermsoperate,without tiny-bound locking,yet20rounds leave a realpointwise closingresidual at a pointbelowstep-startslop;integration crossesprecisionthreshold. Deleting anothereligibilitygate orwideningtolerance is unsupported. No newalgorithm,extraAp,iterationbudget orperformance test is authorizedbythisrecord. Full30second188remainsrejected; mainproductionsourceunchanged. Historicalpair12/pair01labels inlogsretargetoriginal3,4 explicitly.

[global_normal_force_local_torque_trace.patch](../../tools/experiments/global_normal_force_local_torque_trace.patch) is incremental test-observationonly against188candidate, SHA256 `60a24cc8dc2b8d4ca8e8db86577b4c256f548363085bc48852dbd103497dc2a2`. Applycheck passed against reconstructedexact188baseline. Apply originalcandidate→wrenchbasis→offsetinvariance→force_local_torque→thistracepatch in an isolated5080da8 checkout (do notaddseparate187boundarypatch). Then run:

```bash
cargo test --release --offline --locked -p quench3d-physics --lib \
  global_force_local_torque_12_layer_observation -- --nocapture
```

Source remainsfrozen at188plus189observations in the isolatedcheckout; no newcandidate orperformanceworkstarted.
