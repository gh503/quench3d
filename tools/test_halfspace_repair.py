"""Synthetic minimax repair controls; never loads the actual captured system."""
import unittest
from unittest.mock import patch

import repair_halfspace_endpoint as tool


class RepairControls(unittest.TestCase):
    def test_negative_free_repair_coordinate(self):
        result = tool.solve_repair([[(0, -1.0)]], [1.0], [0.0], [.5])
        self.assertTrue(result['success'])
        self.assertAlmostEqual(result['x'][0], -1.0)
        self.assertAlmostEqual(result['x'][1], .5)
        self.assertEqual(result['rows'], 3)

    def test_minimax_uses_physical_scaling(self):
        result = tool.solve_repair([[(0, 1.0), (1, 1.0)]], [3.0], [0.0, 0.0], [1.0, 2.0])
        self.assertTrue(result['success'])
        self.assertAlmostEqual(result['x'][0], 2.0)
        self.assertAlmostEqual(result['x'][1], 1.0)
        self.assertAlmostEqual(result['x'][2], 2.0)

    def test_strict_rhs_repairs_full_original_row(self):
        result = tool.solve_repair([[(0, 1.0)]], [1.0], [.75], [1.0])
        self.assertTrue(result['success'])
        self.assertAlmostEqual(result['x'][0], .25)
        report = tool.repaired_certificate([[(0, 1.0)]], [1.0], [.75], [1.0], [1.0], result['x'][:-1])
        self.assertTrue(report['certificate']['passes_approximate_certificate'])

    def test_fixed_dual_not_reoptimized(self):
        report = tool.repaired_certificate([[(0, 1.0)]], [1.0], [0.0], [0.0], [1.0], [1.0])
        self.assertEqual(report['original_captured_lambda'], [0.0])
        self.assertTrue(report['certificate']['original_rows']['passes_1e_7_m'])
        self.assertFalse(report['certificate']['passes_approximate_certificate'])
        self.assertIn('stationarity_1e_7', report['certificate']['failed_criteria'])

    def test_corrupted_repair_fails_rows(self):
        report = tool.repaired_certificate([[(0, 1.0)]], [1.0], [0.0], [1.0], [1.0], [0.0])
        self.assertFalse(report['certificate']['criteria']['primal_rows_1e_7_m'])
        with self.assertRaises(ValueError):
            tool.repaired_certificate([[(0, 1.0)]], [1.0], [0.0], [1.0], [1.0], [float('nan')])

    def test_bad_hash_never_calls_optimizer(self):
        with patch.object(tool, 'solve_repair') as solve:
            with self.assertRaises(ValueError):
                tool.analyze(b'not the registered snapshot')
            solve.assert_not_called()


if __name__ == '__main__':
    unittest.main()
