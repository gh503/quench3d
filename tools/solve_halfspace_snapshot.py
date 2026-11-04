#!/usr/bin/env python3
"""One registered offline Phase-I solve; never invokes a fluid world or Rust guard."""
import argparse
import hashlib
import json
import math
import struct
import sys
import time
from pathlib import Path

from check_halfspace_snapshot import decode, serial_sum, verify

SNAPSHOT_HASH = '6c0f4dca0a1691423dfe62d783b52c8fc856be1ffbc87fad4a82eab7068c982e'
GRAPH_HASH = '53c7bf04228be87f5c68c2cd123438b6da9994943d426150af46ba10715026a5'
INPUT_HASH = '3154433584d7687424ac5e2ade2ed6f5c018a3b94d99559739b7e680cdb85b51'
OPTIONS = {'primal_feasibility_tolerance': 1e-9,
           'dual_feasibility_tolerance': 1e-9, 'maxiter': 100000, 'time_limit': 60.0}


def finite(values):
    return all(math.isfinite(float(v)) for v in values)


def row_certificate(rows, rhs, y, kinds=None):
    if not finite(y):
        raise ValueError('nonfinite primal witness')
    residuals = [b - serial_sum(a * y[i] for i, a in row)
                 for row, b in zip(rows, rhs)]
    if not finite(residuals):
        raise ValueError('nonfinite original-row residual')
    worst = max(range(len(rows)), key=lambda i: residuals[i])
    bad = [i for i, r in enumerate(residuals) if r > 1e-7]
    return {'passes_1e_7_m': not bad, 'max_positive_residual_m': max(0.0, residuals[worst]),
            'worst_row': worst, 'worst_kind': kinds[worst] if kinds else None,
            'violations': len(bad),
            'pair_violations': sum(kinds[i][0] == 'pair' for i in bad) if kinds else None,
            'wall_violations': sum(kinds[i][0] == 'wall' for i in bad) if kinds else None}


def dual_certificate(rows, rhs, marginals, dimension):
    z = [-float(v) for v in marginals]
    if len(z) != len(rows) or not finite(z):
        raise ValueError('invalid inequality marginals')
    transpose = [0.0] * dimension
    for row, weight in zip(rows, z):
        for i, a in row:
            transpose[i] += a * weight
    return {'z': z, 'z_min': min(z), 'sum_z': serial_sum(z),
            'max_abs_A_transpose_z': max(abs(v) for v in transpose),
            'b_transpose_z': serial_sum(b * v for b, v in zip(rhs, z)),
            'strict_infeasibility_proof': False,
            'interpretation': 'Numerical diagnostics only; residuals do not establish a strict Farkas proof.'}


def solve_phase_one(rows, rhs, dimension):
    # Imports stay here: validation and negative controls need no optimizer import.
    import numpy as np
    import scipy
    from scipy.optimize import linprog
    from scipy.sparse import coo_matrix

    ri, ci, values = [], [], []
    for r, row in enumerate(rows):
        for i, a in row:
            ri.append(r)
            ci.append(i)
            values.append(-a)
        ri.append(r)
        ci.append(dimension)
        values.append(-1.0)
    matrix = coo_matrix((values, (ri, ci)), shape=(len(rows), dimension + 1)).tocsr()
    objective = np.zeros(dimension + 1)
    objective[-1] = 1.0
    started = time.perf_counter()
    result = linprog(objective, A_ub=matrix, b_ub=-np.asarray(rhs),
                     bounds=[(None, None)] * dimension + [(0.0, None)],
                     method='highs', options=dict(OPTIONS))
    report = {'numpy_version': np.__version__, 'scipy_version': scipy.__version__,
              'python_version': sys.version, 'method': 'highs', 'options': dict(OPTIONS),
              'bounds': 'all y unrestricted; t>=0', 'success': bool(result.success),
              'status': int(result.status), 'message': str(result.message),
              'iterations': int(result.nit), 'elapsed_seconds_diagnostic_only': time.perf_counter() - started,
              'x': None if result.x is None else [float(v) for v in result.x],
              'objective_t': None if result.fun is None else float(result.fun),
              'inequality_marginals': None}
    if getattr(result, 'ineqlin', None) is not None and result.ineqlin.marginals is not None:
        report['inequality_marginals'] = [float(v) for v in result.ineqlin.marginals]
    return report


def f32(value):
    return struct.unpack('<f', struct.pack('<f', value))[0]


def stored_certificate(rows, rhs, y, points, inverse, radius, kinds):
    # Precisely the registered simple store path, NOT sweep/slide/collision.
    stored = [[f32(p[a] + f32(y[3*i+a] * inverse[3*i+a])) for a in range(3)]
              for i, p in enumerate(points)]
    actual_y = [(p[a] - points[i][a]) / inverse[3*i+a]
                for i, p in enumerate(stored) for a in range(3)]
    certificate = row_certificate(rows, rhs, actual_y, kinds)
    distances = [math.sqrt(serial_sum((points_i[a] - points_j[a])**2 for a in range(3)))
                 for i, points_i in enumerate(stored) for points_j in stored[i+1:]]
    clearances = [min(.5-abs(p[0]), .5-abs(p[2]), p[1], 1.0-p[1])-radius for p in stored]
    clearance = min(clearances)
    minimum = min(distances) if distances else None
    return {'path': 'simple f32 displacement cast/add only; Rust guard NOT executed',
            'stored_positions': stored, 'original_rows': certificate,
            'minimum_pair_distance_m': minimum, 'pair_target_m': 2*radius-1e-4,
            'pairs_pass': minimum is None or minimum >= 2*radius-1e-4,
            'pair_distance_violations': sum(d < 2*radius-1e-4 for d in distances),
            'minimum_inner_face_clearance_m': clearance, 'clearance_target_m': -1e-4,
            'particle_wall_clearance_violations': sum(c < -1e-4 for c in clearances),
            'walls_pass': clearance >= -1e-4, 'rust_guard_executed': False}


def parse_snapshot(data):
    tags = {}
    for line in data.decode().splitlines():
        parts = line.split('\t')
        tags.setdefault(parts[0], []).append(parts[1:])
    rows = [[] for _ in tags['ROW']]
    for r, i, a in tags['COEFF']:
        rows[int(r)].append((int(i), decode(a)))
    rhs = [decode(row[1]) for row in tags['ROW']]
    return rows, rhs, tags


def analyze(data):
    if hashlib.sha256(data).hexdigest() != SNAPSHOT_HASH:
        raise ValueError('unregistered snapshot bytes')
    integrity = verify(data)
    if integrity['input_sha256'] != INPUT_HASH or integrity['ordered_graph_sha256'] != GRAPH_HASH:
        raise ValueError('unregistered input or ordered graph')
    rows, rhs, tags = parse_snapshot(data)
    dimension = int(tags['CORE'][0][0])
    report = {'scope': 'captured step75 only; no trajectory/production acceptance',
              'command_argv': list(sys.argv),
              'representation': 'JSON numbers round-trip Python binary64; source input preserves integer bits',
              'integrity': integrity, 'conclusion': 'Unknown', 'solver': solve_phase_one(rows, rhs, dimension)}
    solver = report['solver']
    if not solver['success'] or solver['x'] is None:
        return report
    if len(solver['x']) != dimension+1 or not finite(solver['x']):
        raise ValueError('invalid returned primal vector')
    y, t = solver['x'][:-1], solver['x'][-1]
    report['linear_witness'] = row_certificate(rows, rhs, y, [r[1:] for r in tags['KIND']])
    report['returned_t_nonnegative'] = t >= 0.0
    report['phase_one_max_relaxed_residual'] = max(b-serial_sum(a*y[i] for i, a in row)-t for row, b in zip(rows, rhs))
    if solver['inequality_marginals'] is not None:
        report['dual_diagnostics'] = dual_certificate(rows, rhs, solver['inequality_marginals'], dimension)
    if report['linear_witness']['passes_1e_7_m']:
        report['conclusion'] = 'Numerical feasible witness for captured frozen rows at 1e-7m tolerance'
    points = [[decode(v, 32) for v in r[1:]] for r in tags['PREDICTED']]
    inverse = [decode(r[3]) for r in tags['Y']]
    radius = decode(tags['ENTRY'][0][3], 32)
    report['simple_f32_store'] = stored_certificate(rows, rhs, y, points, inverse, radius,
                                                    [r[1:] for r in tags['KIND']])
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('snapshot', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        report = analyze(args.snapshot.read_bytes())
        args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + '\n')
    except (ValueError, OSError, OverflowError, KeyError, IndexError, ImportError) as error:
        parser.exit(1, 'offline diagnosis FAILED/Unknown: ' + str(error) + '\n')
    print(json.dumps({'conclusion': report['conclusion'], 'solver_status': report['solver']['status']}, allow_nan=False))
    return 0 if report['solver']['success'] else 1


if __name__ == '__main__':
    sys.exit(main())
