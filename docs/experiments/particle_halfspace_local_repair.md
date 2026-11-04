# Round224: one minimax physical repair of the frozen dual endpoint

Status: the sole registered repair LP returned numerical failure, formal exit1/status4/success=false, so the result remains Unknown. HiGHS reported an optimal scaled model but an unconfirmed unscaled-model status; no retry, option change or budget increase occurred. This was a new offline LP objective, not a retry of Round223 or an increase to the fixed128 budget. Production algorithms remain unchanged.

## Evidence and frozen input

Use Round219 snapshot SHA2566c0f4dca0a1691423dfe62d783b52c8fc856be1ffbc87fad4a82eab7068c982e only, with integrity/input/graph hashes checked. The reference y is the captured reconstructed A-transpose-lambda vector, not the incremental vector or the large Phase-I witness. Captured lambda remains fixed for independent approximate-QP certificates; do not optimize or clamp it.

Read-only analysis found P=3.770324882092686e-5, D=3.7705337407794e-5, gap=−2.0885868671349913e-9, max complementarity9.083151371066193e-10 and stationarity0 (the latter follows from the captured y=A-transpose-lambda construction). Maximum physical displacement is0.003278720960514516m. Two rows still violate1e-7m, so these small metrics do not establish minimum motion. Full8500 rows contain7750 pair and750 wall rows; diagnostic support has269 rows but must not replace the full graph.

## Single registered LP

Let r=b-A*yref, using original-order serial f64 arithmetic. Minimize t subject to A*delta>=r and -t<=inverse_i*delta_i<=t for every375 coordinate, with delta free and t>=0. Thus A_ub blocks are [-A,0]<=-r, [diag(inverse),-1]<=0 and [-diag(inverse),-1]<=0, totaling9250 inequalities and376 variables. Use strict original b (do not subtract the physical tolerance). t is maximum physical coordinate repair in meters; maximum particle L2 repair can reach sqrt(3)*t.

Use system isolated SciPy1.8.0/NumPy1.21.5 and scipy.optimize.linprog(method='highs') once, fixed primal/dual feasibility tolerances1e-9, maxiter100000 and time_limit60 seconds. Parent additionally terminates after120 seconds. Save complete delta/t, all marginals, options/versions/status/iterations/elapsed and input/source hashes. No fallback, second solve, alternate objective, enlarged budget, reduced graph or changed tolerances.

## Independent acceptance

Set ynew=yref+delta once in f64. Save ynew and original captured lambda. Recompute every original row serially and require finite values and original maximum residual<=1e-7m. Evaluate the same Round223 numerical approximate-QP criteria using the unchanged captured lambda: nonnegative lambda, stationarity infinity norm<=1e-7 in mass-scaled coordinates, maximum |lambda*rnew|<=1e-8 and |Pnew-Dold|<=1e-8*max(1,Pnew). Report physical stationarity norms, tolerance-weighted primal violation, full residuals, actual repair coordinate/L2 maxima and objective t. A passing certificate is tolerance-based approximate optimality evidence, not an exact QP solve or rigorous analytic bound. If optimizer fails or any criterion fails, report Unknown/approximate candidate and the failed criteria; do not repair again.

Independently apply simple f32 delta-cast/add from ynew and original predicted positions, checking original rows<=1e-7m, all7750 pair distances>=2radius-1e-4m and750 face clearances>=-1e-4m. Report counts and extrema separately from the f64 QP certificate. This scope does not execute the Rust guard or any world step. A separately registered replay may later test the actual safety path; stored/guarded values do not inherit optimality automatically.

The strict frozen problem is already numerically feasible by Round221. An LP failure cannot reverse that result. Nothing here validates later rejected states,120-step trajectory, production cost or product completion.

## Evidence

Evidence is retained under target/tmp/round224-local-repair/: inputs.sha256, tool-source.sha256, synthetic-parent-tests.log, solve.log/status1, result.json/result.sha256 and subsequent read-only independent validation. Parent and independent source review preceded the single real repair solve; six synthetic controls covering explicit free repair coordinates, minimax scaling/sign, strict original-row RHS, fixed-dual certificate failure and bad input not reaching the optimizer passed. Tool SHA2563bb8cd5cc79980be65869768db15e7672a283a3e70b9c4996fb520f718e6417d; test SHA256ab5102a15efa1ba45668c42c9e5b21740c7d054a69baafcf9d8260e7e328f549.

The sole result SHA256 is4eaaced240d2394c1eaf74b1421f64d86e90a5630cfeac07bc95907200bcfddb. Solver status4 message: “An optimal solution to the scaled model was found but was HiGHS Status Code0: HighsModelStatusNOTSET in the unscaled model.” It returned1036 iterations, elapsed0.226735s, objective t=2.3660422498580628e-7m, a376-component x and9250 inequality marginals. The implementation correctly stops the acceptance path on success=false; no repair/approximate-QP certificate was issued. Read-only analysis may inspect the retained vector but must preserve Unknown and the unsuccessful solver provenance; it is not permission to rerun with diagnostic or altered solver options.

Independent saved-vector diagnosis (independent.json) reproduced the original hashes/options/status and found a candidate with f64 row maximum2.0742513488075653e-9m and zero violating rows. However, fixed-dual stationarity is2.3614690100099605e-7, above the registered1e-7 threshold; thus it would fail the approximate-QP certificate even aside from the unsuccessful optimizer status. Complementarity is3.2619921168494976e-10 and gap2.528633824653041e-9. Maximum physical coordinate repair is2.3729824981985805e-7m (LP bound residual6.94024834e-10m); maximum particle L2 repair is4.098105389608741e-7m. Simple f32 storage has row maximum1.8230579840172994e-8m, minimum pair distance0.07999998331069946m and inner-face clearance−7.450580596923828e-9m, with zero registered row/pair/face violations. Those numerical candidate facts do not override either failed acceptance condition. No Rust guard, optimizer or world was rerun. All39 halfspace synthetic controls passed.
