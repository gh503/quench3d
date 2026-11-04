# Round223: bounded minimum-motion QP on the frozen step75 system

Status: sole registered QP completed as Unknown/approximate candidate, with formal exit1. The60-second iteration-boundary callback stopped it after2 iterations (elapsed63.248442s, status3); no retry or budget/method change occurred. Independent numerical certificates failed. Earlier Phase-I feasibility and real Rust guard compatibility of its separate large-motion witness remain valid; this QP result supplies no minimum-displacement solution.

## Frozen problem and one solve

Use the unchanged Round219 snapshot SHA2566c0f4dca0a1691423dfe62d783b52c8fc856be1ffbc87fad4a82eab7068c982e and verified Round221 result SHA256fa8e7d886a5936a4bdb34d7f9eb763485c7ddb2f6e89258712aa46d4bada83d4 as warm start. Verify snapshot integrity and hashes before optimization. Keep all8500 original rows and375 free coordinates in original order; do not filter an active set or modify tolerances/masses/RHS/fixture.

Solve min 0.5*y-transpose*y subject to A*y>=b once with isolated system Python (`PYTHONNOUSERSITE=1`, SciPy1.8.0/NumPy1.21.5), scipy.optimize.minimize(method='trust-constr'). Use CSR A, LinearConstraint(A,b,+infinity), jac=y, constant sparse identity Hessian, no variable bounds, and the full existing Phase-I y initial vector. Fixed options: sparse_jacobian=True, factorization_method='AugmentedSystem', maxiter=500, gtol=1e-9, xtol=1e-12, barrier_tol=1e-12. A callback stops at the first iteration boundary after60 seconds of monotonic elapsed time. This is not a strict wall-clock bound on an individual iteration; the parent managed command additionally uses a120-second termination limit, saving intermediate evidence where available. No fallback method, rerun, support filtering or tolerance/budget change in this stage.

## Independent numerical certificates

Save exact round-trip-safe y, all8500 returned constraint multipliers, input hashes, versions/options, optimizer status/message/iterations/elapsed, and full original-order row residuals. For SciPy lower LinearConstraint, lambda=-result.v[0]; confirm convention using a tiny synthetic problem before the sole captured solve. Do not silently clamp negative multipliers.

Independently recompute primal residual r=b-Ay in serial f64 original coefficient order, finite values, maximum positive residual and pair/wall violating counts. The unchanged feasibility threshold is1e-7m. Independently compute A-transpose*lambda, stationarity infinity norm ||y-A-transpose*lambda||inf, per-particle physical stationarity norms using captured inverse masses, maximum absolute complementarity |lambda*r|, P=0.5||y||², D=b-transpose*lambda-0.5||A-transpose*lambda||² and gap=P-D. Report actual minimum lambda, negative count, sum(lambda*max(r,0)) and all metrics.

Predeclared approximate minimum-motion certificate: all finite; all original rows within1e-7m; lambda>=0; stationarity infinity norm<=1e-7; max absolute complementarity<=1e-8; |gap|<=1e-8*max(1,P). If any condition fails, report a numerical candidate/Unknown and exact failed criteria, even if optimizer reports success. Because primal feasibility is tolerance based, this is numerical approximate optimality evidence, not a strict analytic upper/lower bound proof. Do not relabel an optimizer timeout as infeasibility.

Report maximum/RMS physical particle displacement and mass-weighted vector norm versus the Phase-I witness. Active support defined by slack<=1e-7 or lambda>1e-9 is diagnostic only; save its row identities and pair/wall counts without solving a reduced graph. Redundant active rows need not be full rank and may have nonunique multipliers; the strictly convex objective still defines a unique exact primal optimum if feasible.

## Storage and scope limits

Apply the same simple f32 displacement-cast then position-add path and independently check original rows<=1e-7m, pair distance>=2radius-1e-4m and750 inner-face clearances>=-1e-4m. Save all stored coordinates and actual extrema/violating counts. This stage does not execute the real Rust guard; a later separately reviewed single replay may do so. A guard-modified output does not inherit QP optimality automatically.

No world step, changed production algorithm, network access or dependency installation. No claims about the other rejected states,120-step trajectory, default bits, production cost or full product completion. Budget exhaustion or numerical certificate failure remains Unknown/approximate candidate.

## Evidence

Evidence is retained under target/tmp/round223-minimum-motion/: tool-source.sha256, inputs.sha256, environment.log, synthetic-parent-tests.log, checkpoint.json, solve.log/status1 and result.json/result.sha256. Parent and independent source review preceded the sole captured-system QP invocation, and six synthetic controls passed. Tool SHA256 is3a9c703c4aaa0914d3287c4398ac911a240d0b712006cc01e6bb2a54f9c6c1fc; tests SHA256c05c306bab733e842fe6a6ebf2dd14d0a3bf9c4ce91558b31ff8ac9fe38079b0. Result SHA256124f41eb40de9f69635f3ec65cc3e4bf40af757bdaf059f681d28de039ab043e.

Actual bounded failure: the optimizer requested callback termination after2 iterations and63.248442s. No hard outer timeout was required. All values are finite, but maximum original-row residual is0.1029090863480758m at wall row8142, with136 violating wall rows and zero violating pair rows. Stationarity infinity norm is0.34741211309504394, maximum absolute complementarity0.23599907438864987, P=5.866028905619398, D=−30.124321273632606 and gap35.990350179252005. Lambda minimum is−0.2347829502418837 with2817 negative values. All five non-finiteness-independent acceptance criteria fail. The saved candidate's maximum physical displacement is0.975006m; simple f32 storage still violates136 original wall rows and110 particles' inner-face clearances. It was not passed to Rust guard or production.

This is a failure of the registered bounded offline method on this input, not evidence of infeasibility (Round221 already provides a verified feasible witness), and not authorization to extend the budget or retune. The candidate and checkpoint remain available for read-only diagnosis. Independent stdlib verification (independent.json) reproduced all8500 residuals,375 transpose/stationarity components and physical stationarity norms exactly, verified lambda=-raw values, recomputed P/D/gap/motion and confirmed the five failed criteria. The two recorded callback times were31.69448s and63.21294s; the latter is the first boundary above60s. No optimizer, guard or world was rerun. All33 halfspace synthetic controls passed afterward.
