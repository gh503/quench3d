"""不依赖摄像头/MediaPipe 的纯函数测试。

运行：python -m unittest discover -s tools/camera_mocap -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import camera_mocap as cm  # noqa: E402


class DepthTests(unittest.TestCase):
    def test_constant_depth_is_all_zero(self):
        u8, lo, hi = cm.depth_to_u8(np.full((4, 4), 3.5))
        self.assertTrue(np.all(u8 == 0))
        self.assertAlmostEqual(lo, 3.5)
        self.assertAlmostEqual(hi, 3.5)

    def test_depth_is_normalized_monotonically(self):
        src = np.array([[10.0, 20.0], [30.0, 40.0]], dtype=np.float32)
        u8, lo, hi = cm.depth_to_u8(src)
        self.assertEqual((lo, hi), (10.0, 40.0))
        self.assertEqual(int(u8[0, 0]), 0)
        self.assertEqual(int(u8[1, 1]), 255)
        self.assertLess(int(u8[0, 1]), int(u8[1, 0]))

    def test_depth_nan_is_not_a_crash(self):
        src = np.array([[1.0, np.nan], [2.0, 3.0]], dtype=np.float32)
        u8, _, _ = cm.depth_to_u8(src)
        self.assertEqual(u8.shape, src.shape)


class PointCloudTests(unittest.TestCase):
    def test_deprojects_center_and_flips_y(self):
        intr = {"fx": 1.0, "fy": 1.0, "cx": 1.0, "cy": 1.0}
        pts = cm.depth_to_points(np.zeros((3, 3), dtype=np.uint8), intr, 1)
        self.assertEqual(pts.shape, (9, 3))
        self.assertEqual(pts.dtype, np.float32)
        # 中心像素（y=1,x=1）在主轴：x/y 都是 0，深度 0 → 最远 z=-3。
        center = pts[4]
        self.assertAlmostEqual(float(center[0]), 2.2, places=5)
        self.assertAlmostEqual(float(center[1]), 1.0, places=5)
        self.assertAlmostEqual(float(center[2]), -3.0, places=5)
        # 左上像素：相机 y 向下，引擎 y 向上，所以应 +1y（不是 -1）。
        top_left = pts[0]
        self.assertAlmostEqual(float(top_left[0]), 2.2 - 3.0, places=5)
        self.assertAlmostEqual(float(top_left[1]), 1.0 + 3.0, places=5)

    def test_near_depth_and_step(self):
        intr = {"fx": 1.0, "fy": 1.0, "cx": 1.0, "cy": 1.0}
        near = np.full((3, 3), 255, dtype=np.uint8)
        pts = cm.depth_to_points(near, intr, 1)
        self.assertAlmostEqual(float(pts[0][2]), -cm.POINT_Z_NEAR, places=5)
        coarse = cm.depth_to_points(near, intr, 2)
        self.assertEqual(coarse.shape[0], 4)  # 2x2 下采样

    def test_bad_shape_is_rejected(self):
        with self.assertRaises(ValueError):
            cm.depth_to_points(np.zeros((3,), dtype=np.uint8), {}, 1)


class ContourTests(unittest.TestCase):
    def test_rectangle_contour_is_normalized(self):
        mask = np.zeros((100, 200), dtype=np.uint8)
        mask[20:80, 50:150] = 255
        c = cm.largest_contour_polygon(mask)
        self.assertIsNotNone(c)
        self.assertGreaterEqual(len(c["polygon"]), 4)
        self.assertAlmostEqual(c["bbox"][0], 0.25, places=2)
        self.assertAlmostEqual(c["bbox"][2], 0.5, places=2)
        for x, y in c["polygon"]:
            self.assertGreaterEqual(x, 0.0)
            self.assertLessEqual(x, 1.0)
            self.assertGreaterEqual(y, 0.0)
            self.assertLessEqual(y, 1.0)

    def test_empty_mask_returns_none(self):
        self.assertIsNone(cm.largest_contour_polygon(np.zeros((10, 10), dtype=np.uint8)))

    def test_tiny_contour_is_ignored(self):
        mask = np.zeros((100, 100), dtype=np.uint8)
        mask[0:2, 0:2] = 255
        self.assertIsNone(cm.largest_contour_polygon(mask))


class PoseTableTests(unittest.TestCase):
    def test_connections_are_valid_indices(self):
        for a, b in cm.POSE_CONNECTIONS:
            self.assertGreaterEqual(a, 0)
            self.assertGreaterEqual(b, 0)
            self.assertLess(a, 33)
            self.assertLess(b, 33)

    def test_intrinsics_land_on_principal_point(self):
        intr = cm.approximate_intrinsics(640, 480)
        self.assertAlmostEqual(intr["cx"], 320.0)
        self.assertAlmostEqual(intr["cy"], 240.0)
        self.assertGreater(intr["fx"], 0.0)


if __name__ == "__main__":
    unittest.main()
