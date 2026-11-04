"""不依赖摄像头/MediaPipe 的纯函数测试。

运行：python -m unittest discover -s tools/camera_mocap -p 'test_*.py'
"""

import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

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


class FeedPayloadTests(unittest.TestCase):
    def test_depth_metadata_is_written_for_depth_only_frames(self):
        with tempfile.TemporaryDirectory() as td:
            out = Path(td)
            rgb = np.zeros((2, 2, 3), dtype=np.uint8)
            pose = cm.PoseResult([], [], False, 0.0, None)
            depth = np.full((2, 2), 255, dtype=np.uint8)
            cm.write_feed(
                out,
                5,
                2,
                2,
                30.0,
                "test",
                False,
                rgb,
                pose,
                None,
                depth,
                0.0,
                1.0,
                None,
                3,
            )
            payload = json.loads((out / "latest.json").read_text())
            self.assertFalse(payload["points"]["present"])
            self.assertEqual(payload["depth"]["point_step"], 3)
            self.assertAlmostEqual(payload["depth"]["z_near"], cm.POINT_Z_NEAR)
            self.assertAlmostEqual(payload["depth"]["z_far"], cm.POINT_Z_FAR)
            self.assertEqual(payload["depth"]["origin"], list(cm.POINT_ORIGIN))
            self.assertGreater(payload["intrinsics"]["fx"], 0.0)


class PoseSelfTestTests(unittest.TestCase):
    def test_self_test_initializes_full_pose_without_opening_camera(self):
        estimator = mock.Mock()
        estimator.process.return_value = cm.PoseResult([], [], False, 0.0, None)
        with mock.patch.object(cm, "PoseEstimator", return_value=estimator) as pose, \
                mock.patch.object(cm, "open_source") as source, \
                mock.patch("builtins.print") as output:
            self.assertEqual(cm.main(["--self-test-pose"]), 0)
            pose.assert_called_once_with(model_complexity=1, static_image_mode=True)
            source.assert_not_called()
            self.assertEqual(estimator.process.call_args.args[0].shape, (256, 256, 3))
            estimator.close.assert_called_once()
            payload = json.loads(output.call_args.args[0])
            self.assertEqual(payload["backend"], "mediapipe_pose")
            self.assertEqual(payload["model_complexity"], 1)
            self.assertTrue(payload["ok"])


class PreviewOnlyTests(unittest.TestCase):
    def test_camera_preview_writes_rgb_without_pose_or_depth_initialization(self):
        bgr = np.zeros((2, 3, 3), dtype=np.uint8)
        bgr[:, :, 0] = 17
        cap = mock.Mock()
        cap.isOpened.return_value = True
        cap.read.return_value = (True, bgr)
        with tempfile.TemporaryDirectory() as td, \
                mock.patch.object(cm, "open_source", return_value=(cap, False)), \
                mock.patch.object(cm, "PoseEstimator") as pose, \
                mock.patch.object(cm, "DepthEstimator") as depth:
            result = cm.main([
                "--source", "0", "--preview-only", "--no-display",
                "--width", "3", "--height", "2", "--max-frames", "1", "--out", td,
            ])
            self.assertEqual(result, 0)
            pose.assert_not_called()
            depth.assert_not_called()
            cap.release.assert_called_once()
            out = Path(td)
            payload = json.loads((out / "latest.json").read_text())
            self.assertEqual(payload["schema"], "quench3d.mocap/1")
            self.assertEqual((payload["width"], payload["height"]), (3, 2))
            self.assertFalse(payload["pose"]["present"])
            self.assertFalse(payload["silhouette"]["present"])
            self.assertFalse(payload["depth"]["present"])
            self.assertFalse(payload["points"]["present"])
            self.assertEqual((out / "latest.rgb").read_bytes(), bytes([0, 0, 17]) * 6)

    def test_default_cli_keeps_pose_initialization(self):
        cap = mock.Mock()
        cap.isOpened.return_value = True
        cap.read.return_value = (False, None)
        with tempfile.TemporaryDirectory() as td, \
                mock.patch.object(cm, "open_source", return_value=(cap, False)), \
                mock.patch.object(cm, "PoseEstimator") as pose:
            self.assertEqual(cm.main(["--no-display", "--no-depth", "--out", td]), 0)
            pose.assert_called_once()


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
