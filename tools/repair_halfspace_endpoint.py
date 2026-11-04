#!/usr/bin/env python3
"""One registered minimax repair; no global solver/world/Rust guard execution."""
import argparse
import hashlib
import json
import math
import sys
import time
from pathlib import Path

from check_halfspace_snapshot import decode, serial_sum, verify
from solve_halfspace_snapshot import (SNAPSHOT_HASH, GRAPH_HASH, INPUT_HASH, OPTIONS,
                                     finite, parse_snapshot, stored_certificate)
from solve_halfspace_qp import kkt_certificate, motion


def solve_repair(rows, rhs, reference, inverse):
    import numpy as np
    import scipy
    from scipy.optimize import linprog
    from scipy.sparse import coo_matrix

    dimension = len(reference)
    if len(inverse) != dimension or not finite(reference + inverse) or min(inverse) <= 0:
        raise ValueError('invalid reference/inverse dimensions')
    residuals = [b-serial_sum(a*reference[i] for i, a in row) for row, b in zip(rows, rhs)]
    ri, ci, values = [], [], []
    for r, row in enumerate(rows):
        for i, a in row:
            ri.append(r)
            ci.append(i)
            values.append(-a)
    upper = [-r for r in residuals]
    for i, scale in enumerate(inverse):
        for sign in (1.0, -1.0):
            r = len(upper)
            ri.extend([r, r])
            ci.extend([i, dimension])
            values.extend([sign*scale, -1.0])
            upper.append(0.0)
    matrix = coo_matrix((values, (ri, ci)), shape=(len(upper), dimension+1)).tocsr()
    objective = np.zeros(dimension+1)
    objective[-1] = 1.0
    started = time.monotonic()
    result = linprog(objective, A_ub=matrix, b_ub=np.asarray(upper),
                     bounds=[(None, None)]*dimension+[(0.0, None)],
                     method='highs', options=dict(OPTIONS))
    return {'success': bool(result.success), 'status': int(result.status),
            'message': str(result.message), 'iterations': int(result.nit),
            'method': 'highs', 'options': dict(OPTIONS),
            'numpy_version': np.__version__, 'scipy_version': scipy.__version__,
            'python_version': sys.version, 'rows': len(upper), 'variables': dimension+1,
            'elapsed_seconds_diagnostic_only': time.monotonic()-started,
            'x': None if result.x is None else [float(v) for v in result.x],
            'objective_t_m': None if result.fun is None else float(result.fun),
            'inequality_marginals': None if result.ineqlin.marginals is None else [float(v) for v in result.ineqlin.marginals]}


def repaired_certificate(rows, rhs, reference, multipliers, inverse, delta, kinds=None):
    if len(delta) != len(reference) or not finite(delta):
        raise ValueError('invalid repair vector')
    repaired = [a+b for a, b in zip(reference, delta)]
    certificate = kkt_certificate(rows, rhs, repaired, multipliers, inverse, kinds)
    physical = [d*s for d, s in zip(delta, inverse)]
    l2 = [math.sqrt(serial_sum(v*v for v in physical[i:i+3])) for i in range(0, len(physical), 3)]
    return {'ynew': repaired, 'original_captured_lambda': list(multipliers),
            'certificate': certificate, 'max_physical_coordinate_repair_m': max(abs(v) for v in physical),
            'max_physical_particle_repair_m': max(l2),
            'rms_physical_particle_repair_m': math.sqrt(serial_sum(v*v for v in l2)/len(l2)),
            'original_motion': motion(reference, inverse), 'repaired_motion': motion(repaired, inverse)}


def analyze(data):
    if hashlib.sha256(data).hexdigest() != SNAPSHOT_HASH:
        raise ValueError('unregistered snapshot')
    integrity = verify(data)
    if integrity['input_sha256'] != INPUT_HASH or integrity['ordered_graph_sha256'] != GRAPH_HASH:
        raise ValueError('unregistered graph/input')
    rows, rhs, tags = parse_snapshot(data)
    reference = [decode(r[2]) for r in tags['Y']]
    inverse = [decode(r[3]) for r in tags['Y']]
    multipliers = [decode(r[3]) for r in tags['ROW']]
    kinds = [r[1:] for r in tags['KIND']]
    solver = solve_repair(rows, rhs, reference, inverse)
    report = {'scope': 'frozen step75 repair only; not exact QP or Rust guard/trajectory acceptance',
              'command_argv': list(sys.argv), 'integrity': integrity, 'solver': solver,
              'reference_y': reference, 'captured_lambda': multipliers,
              'conclusion': 'Unknown/approximate candidate'}
    if not solver['success'] or solver['x'] is None:
        return report
    if len(solver['x']) != len(reference)+1 or not finite(solver['x']):
        raise ValueError('invalid returned LP witness')
    delta, t = solver['x'][:-1], solver['x'][-1]
    repaired = repaired_certificate(rows, rhs, reference, multipliers, inverse, delta, kinds)
    report['repair'] = repaired
    report['delta'] = delta
    report['t_m'] = t
    report['t_nonnegative'] = t >= 0.0
    report['max_physical_coordinate_bound_residual_m'] = max(abs(d*s)-t for d, s in zip(delta, inverse))
    report['objective_change'] = repaired['certificate']['primal_objective']-.5*serial_sum(v*v for v in reference)
    if repaired['certificate']['passes_approximate_certificate']:
        report['conclusion'] = 'Numerical approximate-QP certificate after independent minimax repair; not exact QP solve'
    points = [[decode(v, 32) for v in r[1:]] for r in tags['PREDICTED']]
    radius = decode(tags['ENTRY'][0][3], 32)
    report['simple_f32_store'] = stored_certificate(rows, rhs, repaired['ynew'], points, inverse, radius, kinds)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('snapshot', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        report = analyze(args.snapshot.read_bytes())
        args.output.write_text(json.dumps(report, indent=2, allow_nan=False)+'\n')
    except (ValueError, OSError, ImportError, OverflowError, KeyError, IndexError) as error:
        parser.exit(1, 'Repair diagnosis FAILED/Unknown: '+str(error)+'\n')
    print(json.dumps({'conclusion': report['conclusion'], 'solver_status': report['solver']['status']}, allow_nan=False))
    return 0 if report['solver']['success'] else 1


if __name__ == '__main__':
    sys.exit(main())
