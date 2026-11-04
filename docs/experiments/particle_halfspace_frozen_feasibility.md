# Round221: independent feasibility of the captured step75 system

Status: the sole preregistered solve completed with exit0. HiGHS returned t=0 after1661 iterations, and independent serial original-row checks found a finite numerical feasible witness within the unchanged1e-7m tolerance. Simple f32 storage also passed the registered analytic certificates; the real Rust guard was not executed. This is an offline numerical diagnosis, not a new fluid algorithm or physical trajectory.

## Frozen input

Use only Round219 capture.tsv, SHA256 `6c0f4dca0a1691423dfe62d783b52c8fc856be1ffbc87fad4a82eab7068c982e`, from instrumented source `aa7e29ab4e5a6858898da64461496417a376e0873859d5dd280e6204562fb94f`. It contains the first budget rejection at step75, 125 particles, 375 variables and 8500 rows. Ordered graph hash `53c7bf04228be87f5c68c2cd123438b6da9994943d426150af46ba10715026a5`; input hash `3154433584d7687424ac5e2ade2ed6f5c018a3b94d99559739b7e680cdb85b51`. Verify integrity and hashes before optimization. Do not change row order, coefficients, RHS, mass transform, tolerance or fixture.

## Single registered solve

In the already available isolated system Python environment (`PYTHONNOUSERSITE=1`, NumPy1.21.5/SciPy1.8.0), construct sparse A and b directly from COEFF/ROW. Use HiGHS via scipy.optimize.linprog with method `highs` and fixed primal/dual feasibility tolerances1e-9, iteration limit100000 and time limit60 seconds. No dependency installation or network access. Solve once:

minimize t, subject to -Ay - t <= -b, y unrestricted, t >= 0.

Do not rerun with changed tolerances, solver method, budgets or objective if the result is inconvenient. Save solver version, exact options, status, iterations, objective, full round-trip-safe y/t, elapsed time (diagnostic only), input hashes, stdout/stderr and formal exit status. An optimizer failure is diagnostic failure/Unknown, not infeasibility. This scope does not execute any world step or Rust fluid operator.

## Independent validation and bounded interpretation

Recompute every original row residual b-Ay with serial f64 arithmetic in the captured coefficient order. Retain maximum, worst row and violating pair/wall counts. Require all values finite. A witness satisfying all original rows within the unchanged1e-7m physical certificate is numerical evidence that this particular frozen system is feasible at that tolerance. The optimizer status or its reported t alone is not evidence.

Also save the returned inequality marginals. SciPy reports nonpositive inequality marginals for this minimization convention, so set z = -result.ineqlin.marginals (not the unmodified marginals). If a claim of numerical infeasibility is considered, independently validate z>=0, A-transpose*z≈0, sum(z)<=1 and b-transpose*z>0, reporting actual numerical residuals and no strict mathematical-proof claim. In this phase an ambiguous or residual-dominated result remains Unknown. A positive optimum alone must never be labelled infeasible.

Decode the captured predicted positions and inverse square-root masses. Reconstruct each displacement in f64, cast each displacement component to f32, then add to its original f32 component with f32 rounding. Independently re-evaluate all original frozen rows using this stored displacement, all particle pair distances, and the six analytic inner-face clearances. Report these certificates separately from the f64 linear witness. Preserve the existing thresholds: all frozen row residuals <=1e-7m, all pair distances >=2*radius-1e-4m and all inner-face clearances >=-1e-4m; print the actual extrema and violating counts, not only pass flags. This simple store path is **not** the existing Rust swept/slide/collision guard: no claim of passing the real guard is permitted without a separately reviewed Rust witness-replay test. No second physical trajectory is authorized here.

Only the captured step75 system is addressed. Nothing here changes the rejected fixed128 algorithm, proves later45 rejections feasible/infeasible, establishes trajectory precision, or meets production cost targets.

## Evidence location

Evidence is retained in `target/tmp/round221-frozen-feasibility/`: result.json, solve.log, solve.status(0), result.sha256, tool-source.sha256 and synthetic-tests.log. Solver source841fdc53ff61bebbde46c92f87a7964b74a1a6f0f0f9dbb7cb8b578cf3ecd2da and test source15da1d186264b3051510ccd90e5ddfce43bf8afe80fbeaaad17cc6684ac6d9ca were frozen and independently reviewed; six synthetic controls passed before the sole captured-system solve. Result SHA256 is fa8e7d886a5936a4bdb34d7f9eb763485c7ddb2f6e89258712aa46d4bada83d4.

Actual result: NumPy1.21.5/SciPy1.8.0, success/status0,1661 iterations, t=0. The f64 original-row maximum positive residual is9.293781022545744e-16m (row6544, pair75/95), with zero violating rows. Simple f32 storage has maximum original-row residual3.725290298461914e-8m (row8021, wall45/1), minimum pair distance0.08000243995481666m and minimum inner-face clearance−7.450580596923828e-9m, with zero registered row/pair/wall violations. Returned Phase-I z is all zero and provides no infeasibility evidence. Solver elapsed0.3818s is a one-off offline diagnostic, not an engine performance measurement.

The Phase-I objective minimizes only t, not displacement: the returned witness has maximum particle displacement approximately0.971278m and mass-weighted vector norm3.31657. It is deliberately **not** a minimal-displacement QP solution or a proposed physically suitable correction. Feasibility at this frozen step does not validate the fixed128 method, later rejected systems, the real guard, or a complete simulation trajectory.
