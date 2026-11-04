#!/usr/bin/env python3
"""Round223: one bounded frozen QP; no world step or Rust guard execution."""
import argparse
import hashlib
import json
import math
import sys
import time
from pathlib import Path

from check_halfspace_snapshot import decode, serial_sum, verify
from solve_halfspace_snapshot import (SNAPSHOT_HASH, GRAPH_HASH, INPUT_HASH, finite,
                                     parse_snapshot, row_certificate, stored_certificate)

WARM_HASH = 'fa8e7d886a5936a4bdb34d7f9eb763485c7ddb2f6e89258712aa46d4bada83d4'
OPTIONS = {'sparse_jacobian': True, 'factorization_method': 'AugmentedSystem',
           'maxiter': 500, 'gtol': 1e-9, 'xtol': 1e-12, 'barrier_tol': 1e-12}


def motion(y, inverse):
    norms = [math.sqrt(serial_sum((y[i+a]*inverse[i+a])**2 for a in range(min(3, len(y)-i))))
             for i in range(0, len(y), 3)]
    return {'maximum_physical_displacement_m': max(norms),
            'rms_physical_displacement_m': math.sqrt(serial_sum(v*v for v in norms)/len(norms)),
            'mass_weighted_vector_norm': math.sqrt(serial_sum(v*v for v in y))}


def kkt_certificate(rows, rhs, y, multipliers, inverse, kinds=None):
    if len(y) != len(inverse) or len(multipliers) != len(rows):
        raise ValueError('KKT dimensions')
    if not finite(y + multipliers + inverse):
        raise ValueError('nonfinite KKT vector')
    residuals = [b-serial_sum(a*y[i] for i, a in row) for row, b in zip(rows, rhs)]
    transpose = [0.0]*len(y)
    for row, weight in zip(rows, multipliers):
        for i, a in row:
            transpose[i] += a*weight
    stationary = [a-b for a, b in zip(y, transpose)]
    physical_norms = [math.sqrt(serial_sum((stationary[i+a]*inverse[i+a])**2
                                         for a in range(min(3, len(y)-i))))
                      for i in range(0, len(y), 3)]
    p = .5*serial_sum(v*v for v in y)
    d = serial_sum(b*l for b, l in zip(rhs, multipliers))-.5*serial_sum(v*v for v in transpose)
    gap = p-d
    complementarity = max(abs(l*r) for l, r in zip(multipliers, residuals))
    infinity = max(abs(v) for v in stationary)
    all_finite = finite(residuals + transpose + stationary + physical_norms + [p, d, gap, complementarity])
    criteria = {'all_finite': all_finite, 'primal_rows_1e_7_m': max(residuals) <= 1e-7,
                'lambda_nonnegative': min(multipliers) >= 0.0,
                'stationarity_1e_7': infinity <= 1e-7,
                'complementarity_1e_8': complementarity <= 1e-8,
                'gap_tolerance': abs(gap) <= 1e-8*max(1.0, p)}
    support = [i for i, (r, l) in enumerate(zip(residuals, multipliers)) if -r <= 1e-7 or l > 1e-9]
    return {'passes_approximate_certificate': all(criteria.values()), 'criteria': criteria,
            'failed_criteria': [name for name, ok in criteria.items() if not ok],
            'original_rows': row_certificate(rows, rhs, y, kinds), 'row_residuals': residuals,
            'A_transpose_lambda': transpose, 'stationarity': stationary,
            'stationarity_infinity_norm': infinity, 'physical_stationarity_norms': physical_norms,
            'max_physical_stationarity_norm_m': max(physical_norms),
            'max_absolute_complementarity': complementarity, 'primal_objective': p,
            'dual_objective': d, 'gap': gap, 'lambda_min': min(multipliers),
            'negative_multiplier_count': sum(l < 0 for l in multipliers),
            'tolerance_weighted_primal_violation': serial_sum(l*max(r, 0.0) for l, r in zip(multipliers, residuals)),
            'active_support': [{'row': i, 'kind': kinds[i] if kinds else None} for i in support],
            'active_pair_count': sum(kinds[i][0] == 'pair' for i in support) if kinds else None,
            'active_wall_count': sum(kinds[i][0] == 'wall' for i in support) if kinds else None,
            'interpretation': 'Numerical approximate optimality only; tolerance-feasible primal is not a strict bound proof.'}


def solve_qp(rows, rhs, initial, checkpoint=None):
    import numpy as np
    import scipy
    from scipy.optimize import LinearConstraint, minimize
    from scipy.sparse import coo_matrix, eye

    ri, ci, values = [], [], []
    for r, row in enumerate(rows):
        for i, a in row:
            ri.append(r)
            ci.append(i)
            values.append(a)
    matrix = coo_matrix((values, (ri, ci)), shape=(len(rows), len(initial))).tocsr()
    identity = eye(len(initial), format='csr')
    started = time.monotonic()
    history = []
    time_budget_stopped = False

    def callback(x, state):
        nonlocal time_budget_stopped
        elapsed = time.monotonic()-started
        record = {'iteration': int(state.nit), 'elapsed_seconds': elapsed,
                  'objective': float(state.fun), 'optimality': float(state.optimality),
                  'constraint_violation': float(state.constr_violation)}
        history.append(record)
        if checkpoint is not None:
            checkpoint({'scope': 'intermediate evidence only; no minimum-motion certificate',
                        'x': [float(v) for v in x], 'history': list(history), 'options': dict(OPTIONS)})
        time_budget_stopped = elapsed >= 60.0
        return time_budget_stopped

    result = minimize(lambda y: .5*np.dot(y, y), np.asarray(initial), jac=lambda y: y,
                      hess=lambda y: identity, method='trust-constr',
                      constraints=LinearConstraint(matrix, np.asarray(rhs), np.inf),
                      options=dict(OPTIONS), callback=callback)
    return {'numpy_version': np.__version__, 'scipy_version': scipy.__version__,
            'python_version': sys.version, 'method': 'trust-constr', 'options': dict(OPTIONS),
            'success': bool(result.success), 'status': int(result.status), 'message': str(result.message),
            'callback_time_budget_stopped': time_budget_stopped,
            'budget_outcome': 'Unknown/time boundary stop' if time_budget_stopped else ('optimizer completed' if result.success else 'Unknown/optimizer did not converge'),
            'iterations': int(result.nit), 'elapsed_seconds_diagnostic_only': time.monotonic()-started,
            'y': [float(v) for v in result.x],
            'raw_lower_constraint_multipliers': [float(v) for v in result.v[0]],
            'lambda': [-float(v) for v in result.v[0]],
            'lower_constraint_multiplier_convention': 'lambda=-result.v[0], never clamped',
            'history': history}


def analyze(data, warm_bytes, checkpoint=None):
    if hashlib.sha256(data).hexdigest() != SNAPSHOT_HASH or hashlib.sha256(warm_bytes).hexdigest() != WARM_HASH:
        raise ValueError('unregistered frozen snapshot/warm start')
    integrity = verify(data)
    if integrity['input_sha256'] != INPUT_HASH or integrity['ordered_graph_sha256'] != GRAPH_HASH:
        raise ValueError('unregistered graph/input')
    rows, rhs, tags = parse_snapshot(data)
    warm = json.loads(warm_bytes)
    initial = warm['solver']['x'][:-1]
    if len(initial) != 375 or not finite(initial):
        raise ValueError('invalid complete warm start')
    kinds = [r[1:] for r in tags['KIND']]
    if not row_certificate(rows, rhs, initial, kinds)['passes_1e_7_m']:
        raise ValueError('warm start failed original rows')
    inverse = [decode(r[3]) for r in tags['Y']]
    solver = solve_qp(rows, rhs, initial, checkpoint)
    certificate = kkt_certificate(rows, rhs, solver['y'], solver['lambda'], inverse, kinds)
    points = [[decode(v, 32) for v in r[1:]] for r in tags['PREDICTED']]
    radius = decode(tags['ENTRY'][0][3], 32)
    return {'scope': 'frozen step75 only; no Rust guard/world/production acceptance',
            'command_argv': list(sys.argv), 'input_integrity': integrity,
            'warm_start_sha256': WARM_HASH, 'solver': solver, 'certificate': certificate,
            'conclusion': 'Numerical approximate minimum-motion certificate' if solver['success'] and certificate['passes_approximate_certificate'] else 'Unknown/approximate candidate',
            'motion': motion(solver['y'], inverse), 'phase_one_motion': motion(initial, inverse),
            'simple_f32_store': stored_certificate(rows, rhs, solver['y'], points, inverse, radius, kinds)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('snapshot', type=Path)
    parser.add_argument('warm_start', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--checkpoint', type=Path, required=True)
    args = parser.parse_args()

    def checkpoint(report):
        args.checkpoint.write_text(json.dumps(report, indent=2, allow_nan=False)+'\n')

    try:
        report = analyze(args.snapshot.read_bytes(), args.warm_start.read_bytes(), checkpoint)
        args.output.write_text(json.dumps(report, indent=2, allow_nan=False)+'\n')
    except (ValueError, OSError, ImportError, OverflowError, KeyError, IndexError) as error:
        parser.exit(1, 'QP diagnosis FAILED/Unknown: '+str(error)+'\n')
    print(json.dumps({'conclusion': report['conclusion'], 'solver_status': report['solver']['status'],
                      'failed_criteria': report['certificate']['failed_criteria']}, allow_nan=False))
    return 0 if report['solver']['success'] else 1


if __name__ == '__main__':
    sys.exit(main())
