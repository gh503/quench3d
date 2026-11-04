"""Synthetic Round223 controls; never loads the captured frozen QP."""
import unittest
from unittest.mock import patch

import solve_halfspace_qp as tool


class QpControls(unittest.TestCase):
    def test_lower_constraint_sign_and_known_minimum(self):
        result = tool.solve_qp([[(0, 1.0)]], [1.0], [2.0])
        self.assertTrue(result['success'])
        self.assertAlmostEqual(result['y'][0], 1.0, delta=.001)
        self.assertGreater(result['lambda'][0], 0.0)
        self.assertAlmostEqual(result['y'][0], result['lambda'][0], delta=1e-7)

    def test_unrestricted_negative_coordinate(self):
        result = tool.solve_qp([[(0, -1.0)]], [1.0], [-2.0])
        self.assertTrue(result['success'])
        self.assertAlmostEqual(result['y'][0], -1.0, delta=.001)
        self.assertGreater(result['lambda'][0], 0.0)

    def test_independent_exact_kkt_known_solution(self):
        report = tool.kkt_certificate([[(0, 1.0)]], [1.0], [1.0], [1.0], [1.0])
        self.assertTrue(report['passes_approximate_certificate'])
        self.assertEqual(report['gap'], 0.0)
        self.assertEqual(report['stationarity_infinity_norm'], 0.0)

    def test_nonminimal_feasible_candidate_fails_kkt(self):
        report = tool.kkt_certificate([[(0, 1.0)]], [1.0], [2.0], [2.0], [1.0])
        self.assertTrue(report['original_rows']['passes_1e_7_m'])
        self.assertFalse(report['passes_approximate_certificate'])
        self.assertIn('complementarity_1e_8', report['failed_criteria'])

    def test_negative_multiplier_not_clamped(self):
        report = tool.kkt_certificate([[(0, 1.0)]], [1.0], [1.0], [-1e-12], [1.0])
        self.assertFalse(report['criteria']['lambda_nonnegative'])
        self.assertEqual(report['negative_multiplier_count'], 1)

    def test_invalid_witness_and_bad_input_never_solve(self):
        with self.assertRaises(ValueError):
            tool.kkt_certificate([[(0, 1.0)]], [1.0], [float('nan')], [1.0], [1.0])
        with patch.object(tool, 'solve_qp') as solve:
            with self.assertRaises(ValueError):
                tool.analyze(b'not a snapshot', b'not a warm start')
            solve.assert_not_called()


if __name__ == '__main__':
    unittest.main()
