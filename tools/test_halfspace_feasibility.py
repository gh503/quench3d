"""Synthetic controls only. Never reads or solves the captured fluid system."""
import math
import unittest
from unittest.mock import patch

import solve_halfspace_snapshot as tool


class FeasibilityControls(unittest.TestCase):
    def test_unrestricted_negative_primal(self):
        rows, rhs = [[(0, -1.0)]], [1.0]
        result = tool.solve_phase_one(rows, rhs, 1)
        self.assertTrue(result['success'])
        self.assertEqual(result['objective_t'], 0.0)
        self.assertLessEqual(result['x'][0], -1.0)
        self.assertTrue(tool.row_certificate(rows, rhs, result['x'][:-1])['passes_1e_7_m'])

    def test_inconsistent_positive_phase_one_and_dual_sign(self):
        rows, rhs = [[(0, 1.0)], [(0, -1.0)]], [1.0, 1.0]
        result = tool.solve_phase_one(rows, rhs, 1)
        self.assertTrue(result['success'])
        self.assertAlmostEqual(result['objective_t'], 1.0)
        self.assertFalse(tool.row_certificate(rows, rhs, result['x'][:-1])['passes_1e_7_m'])
        dual = tool.dual_certificate(rows, rhs, result['inequality_marginals'], 1)
        self.assertGreaterEqual(dual['z_min'], 0.0)
        self.assertAlmostEqual(dual['sum_z'], 1.0)
        self.assertAlmostEqual(dual['max_abs_A_transpose_z'], 0.0)
        self.assertGreater(dual['b_transpose_z'], 0.0)
        self.assertFalse(dual['strict_infeasibility_proof'])

    def test_corrupted_witness_fails_independent_rows(self):
        rows, rhs = [[(0, 1.0)]], [1.0]
        self.assertFalse(tool.row_certificate(rows, rhs, [0.0])['passes_1e_7_m'])
        with self.assertRaises(ValueError):
            tool.row_certificate(rows, rhs, [math.nan])

    def test_simple_store_separate_from_guard(self):
        rows = [[(0, -1.0), (3, 1.0)]]
        report = tool.stored_certificate(rows, [.02], [-.01, 0, 0, .01, 0, 0],
                                        [[0, .5, 0], [.06, .5, 0]], [1.0]*6, .04,
                                        [['pair', '0', '1']])
        self.assertTrue(report['original_rows']['passes_1e_7_m'])
        self.assertTrue(report['pairs_pass'])
        self.assertTrue(report['walls_pass'])
        self.assertEqual(report['pair_distance_violations'], 0)
        self.assertEqual(report['particle_wall_clearance_violations'], 0)
        self.assertFalse(report['rust_guard_executed'])

    def test_stored_geometric_violation_counts(self):
        report = tool.stored_certificate([[(0, 1.0)]], [-1.0], [0.0]*6,
                                        [[0.0, .0, 0.0], [.01, .5, 0.0]], [1.0]*6, .4,
                                        [['wall', '0', '0']])
        self.assertEqual(report['pair_distance_violations'], 1)
        self.assertEqual(report['particle_wall_clearance_violations'], 1)

    def test_bad_snapshot_never_calls_optimizer(self):
        with patch.object(tool, 'solve_phase_one') as solve:
            with self.assertRaises(ValueError):
                tool.analyze(b'unregistered snapshot')
            solve.assert_not_called()


if __name__ == '__main__':
    unittest.main()
