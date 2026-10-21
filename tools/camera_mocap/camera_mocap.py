#!/usr/bin/env python3
"""Quench3D 摄像头动捕采集端（第一阶段参考实现）。

它做四件事，并把结果写成一个编辑器能轮询的**最新帧**协议：

1. 采集：OpenCV `VideoCapture`（Linux 下通常走 V4L2）；
2. 姿态：MediaPipe Pose，33 个 2D 关键点 + 33 个以髋部为原点的 3D 世界点；
3. 轮廓：MediaPipe Pose 自带的人体分割 mask → 最大外轮廓多边形；
4. 深度：可选的 MiDaS small ONNX（单目相对逆深度，不是米制深度）。

输出目录（缺省 `target/camera`）：

    latest.json   本帧的元数据 + 姿态 + 轮廓 + 深度统计
    latest.rgb    原始 RGB24 像素，长度 = width * height * 3
    latest.depth  归一化到 0..255 的深度图（只有启用深度时存在）
    latest.points 深度反投影点云，xyz_f32_le（可选，缺省开启）

写入顺序是 **rgb/depth/points 先、json 后**，且每个文件都是临时文件 + `os.replace`，
读端看到合法 json 时像素文件已经完整。编辑器不会依赖 Python 进程的 stdout。

诚实边界：
* 单目深度是相对逆深度，不是米；不能直接当真实距离用。点云只是把 0..255 的
  相对值反投影到 0.3~3.0 的**任意尺度**，没有米制含义。
* 没有做时间同步/IMU 融合；RGB、姿态、深度来自同一帧，但深度可能隔几帧才更新。
* 不包含 SMPL/SMPL-X 体型拟合；3D 骨骼是 MediaPipe 的 33 点世界坐标。
* 所有推理在本机完成，不把画面发到网络；深度模型需要你手动下载一次。
"""

from __future__ import annotations

import argparse
import json
import math
import os
import queue
import sys
import threading
import time
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterator

import cv2
import numpy as np

# MediaPipe Pose 的 33 点连接。编辑器里有一份同样的表（Rust 侧），
# 修改时两边要一起改；Python 单测会检查索引范围。
POSE_CONNECTIONS: list[tuple[int, int]] = [
    (0, 1), (1, 2), (2, 3), (3, 7),
    (0, 4), (4, 5), (5, 6), (6, 8),
    (9, 10),
    (11, 12), (11, 13), (13, 15), (15, 17), (15, 19), (15, 21),
    (17, 19),
    (12, 14), (14, 16), (16, 18), (16, 20), (16, 22),
    (18, 20),
    (11, 23), (12, 24), (23, 24),
    (23, 25), (24, 26), (25, 27), (26, 28),
    (27, 29), (28, 30), (29, 31), (30, 32),
    (27, 31), (28, 32),
]

KEY_VISIBILITY = (11, 12, 23, 24, 25, 26)  # 肩/髋/膝，用于姿态置信度

DEFAULT_DEPTH_URL = (
    "https://github.com/isl-org/MiDaS/releases/download/v2_1/model-small.onnx"
)

# 点云落在编辑器骨架同一块区域；z 只是把相对逆深度映射到 0.3~3.0 的任意尺度，
# 不是米制深度。原点与 Rust 侧 MOCAP_ORIGIN 保持一致。
POINT_ORIGIN = (2.2, 1.0, 0.0)
POINT_Z_NEAR = 0.3
POINT_Z_FAR = 3.0


def depth_to_u8(depth: np.ndarray) -> tuple[np.ndarray, float, float]:
    """把任意范围的深度图归一化到 uint8。

    返回 `(u8, raw_min, raw_max)`。如果输入是常数，返回全 0，避免 0/0。
    """
    d = np.asarray(depth, dtype=np.float32)
    finite = np.isfinite(d)
    if not finite.any():
        return np.zeros(d.shape, dtype=np.uint8), 0.0, 0.0
    lo = float(d[finite].min())
    hi = float(d[finite].max())
    if hi - lo < 1e-9:
        return np.zeros(d.shape, dtype=np.uint8), lo, hi
    # NaN/Inf 用最近的有效下界填，避免 astype(uint8) 的未定义转换警告。
    clean = np.where(finite, d, lo)
    out = (clean - lo) / (hi - lo)
    out = np.clip(out * 255.0, 0, 255).astype(np.uint8)
    return out, lo, hi


def depth_to_points(
    depth_u8: np.ndarray,
    intrinsics: dict[str, float],
    step: int = 6,
) -> np.ndarray:
    """把 u8 相对逆深度反投影成引擎坐标点云 `(N,3) float32`。

    深度 255（最近）→ z = `POINT_Z_NEAR`；0（最远）→ `POINT_Z_FAR`。这是**任意
    尺度**，只用来观察结构，不是米。像素按 `step` 下采样；坐标已经加上
    `POINT_ORIGIN`，并转成引擎的 x 右 / y 上 / z 向屏幕外（相机朝 -z 看）。
    """
    d = np.asarray(depth_u8, dtype=np.uint8)
    if d.ndim != 2:
        raise ValueError("depth_u8 必须是二维")
    h, w = d.shape
    step = max(1, int(step))
    ys = np.arange(0, h, step, dtype=np.int32)
    xs = np.arange(0, w, step, dtype=np.int32)
    grid = d[np.ix_(ys, xs)].astype(np.float32) / 255.0
    z = POINT_Z_NEAR + (1.0 - grid) * (POINT_Z_FAR - POINT_Z_NEAR)
    fx = max(float(intrinsics.get("fx", 1.0)), 1e-6)
    fy = max(float(intrinsics.get("fy", 1.0)), 1e-6)
    cx = float(intrinsics.get("cx", (w - 1) * 0.5))
    cy = float(intrinsics.get("cy", (h - 1) * 0.5))
    gx = (xs.astype(np.float32)[None, :] - cx) / fx * z
    gy = -(ys.astype(np.float32)[:, None] - cy) / fy * z
    ox, oy, oz = POINT_ORIGIN
    out = np.stack(
        [
            (gx + ox).ravel(),
            (gy + oy).ravel(),
            (-z + oz).ravel(),
        ],
        axis=1,
    )
    return np.ascontiguousarray(out, dtype="<f4")


def largest_contour_polygon(
    mask: np.ndarray, min_area_ratio: float = 0.005
) -> dict[str, Any] | None:
    """从 0/255 二值 mask 提取最大外轮廓，并归一化坐标。

    返回 `{polygon:[[x,y],...], bbox:[x,y,w,h], area:比例}`；没有足够大的轮廓返回 None。
    """
    if mask is None or mask.size == 0:
        return None
    h, w = mask.shape[:2]
    m = (mask > 127).astype(np.uint8) * 255
    contours, _ = cv2.findContours(m, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
    if not contours:
        return None
    c = max(contours, key=cv2.contourArea)
    area = float(cv2.contourArea(c))
    if area < float(w * h) * min_area_ratio:
        return None
    epsilon = max(1.0, 0.004 * cv2.arcLength(c, True))
    approx = cv2.approxPolyDP(c, epsilon, True)
    xs = approx[:, 0, 0].astype(np.float32) / max(w - 1, 1)
    ys = approx[:, 0, 1].astype(np.float32) / max(h - 1, 1)
    x, y, bw, bh = cv2.boundingRect(c)
    return {
        "present": True,
        "polygon": [[round(float(a), 6), round(float(b), 6)] for a, b in zip(xs, ys)],
        "bbox": [
            round(x / w, 6),
            round(y / h, 6),
            round(bw / w, 6),
            round(bh / h, 6),
        ],
        "area": round(area / float(w * h), 6),
    }


def approximate_intrinsics(width: int, height: int, hfov_deg: float = 60.0) -> dict[str, float]:
    """没有标定时的近似内参：只用于把 2D 点抬到 3D 时做展示，不是测量值。"""
    fx = (0.5 * width) / math.tan(math.radians(hfov_deg) * 0.5)
    return {
        "fx": round(fx, 3),
        "fy": round(fx, 3),
        "cx": round(width * 0.5, 3),
        "cy": round(height * 0.5, 3),
        "hfov_deg": hfov_deg,
    }


def atomic_write(path: Path, data: bytes) -> None:
    """同目录临时文件 + os.replace；读端不会读到半个文件。"""
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_bytes(data)
    os.replace(tmp, path)


@dataclass
class PoseResult:
    landmarks: list[dict[str, float]]
    world: list[dict[str, float]]
    present: bool
    confidence: float
    mask: np.ndarray | None


class PoseEstimator:
    """MediaPipe Pose 的薄包装；没有 MediaPipe 时给出可执行的错误。"""

    def __init__(self, model_complexity: int = 1, static_image_mode: bool = False):
        try:
            import mediapipe as mp
        except Exception as exc:  # pragma: no cover - 依赖未安装时才会走
            raise RuntimeError(
                "需要 mediapipe：python -m pip install -r tools/camera_mocap/requirements.txt"
            ) from exc
        self._mp = mp
        self._pose = mp.solutions.pose.Pose(
            static_image_mode=static_image_mode,
            model_complexity=model_complexity,
            smooth_landmarks=True,
            enable_segmentation=True,
            min_detection_confidence=0.5,
            min_tracking_confidence=0.5,
        )

    def process(self, rgb: np.ndarray) -> PoseResult:
        h, w = rgb.shape[:2]
        result = self._pose.process(rgb)
        if result.pose_landmarks is None:
            return PoseResult([], [], False, 0.0, None)
        landmarks = []
        for lm in result.pose_landmarks.landmark:
            landmarks.append(
                {
                    "x": round(float(lm.x), 6),
                    "y": round(float(lm.y), 6),
                    "z": round(float(lm.z), 6),
                    "visibility": round(float(lm.visibility), 6),
                }
            )
        world = []
        if result.pose_world_landmarks is not None:
            for i, lm in enumerate(result.pose_world_landmarks.landmark):
                vis = landmarks[i]["visibility"] if i < len(landmarks) else 0.0
                world.append(
                    {
                        "x": round(float(lm.x), 6),
                        "y": round(float(lm.y), 6),
                        "z": round(float(lm.z), 6),
                        "visibility": round(float(vis), 6),
                    }
                )
        conf = 0.0
        if landmarks:
            vals = [landmarks[i]["visibility"] for i in KEY_VISIBILITY if i < len(landmarks)]
            conf = sum(vals) / len(vals) if vals else 0.0
        mask = None
        if result.segmentation_mask is not None:
            m = np.clip(result.segmentation_mask, 0.0, 1.0)
            mask = (m * 255.0).astype(np.uint8)
        return PoseResult(landmarks, world, True, conf, mask)

    def close(self) -> None:
        self._pose.close()


class DepthEstimator:
    """MiDaS small ONNX → 相对逆深度（0..255）。CPU 上约 0.2~0.3 秒/帧。"""

    def __init__(self, model_path: Path, input_size: int = 256):
        if not model_path.exists():
            raise RuntimeError(
                f"深度模型不存在：{model_path}\n"
                f"下载：python tools/camera_mocap/camera_mocap.py --download-depth-model\n"
                f"或：curl -L -o {model_path} {DEFAULT_DEPTH_URL}"
            )
        self.net = cv2.dnn.readNetFromONNX(str(model_path))
        self.input_size = input_size

    def process(self, rgb: np.ndarray) -> tuple[np.ndarray, float, float]:
        h, w = rgb.shape[:2]
        blob = cv2.dnn.blobFromImage(
            rgb,
            1.0 / 255.0,
            (self.input_size, self.input_size),
            (123.675, 116.28, 103.53),
            swapRB=False,
            crop=False,
        )
        self.net.setInput(blob)
        out = self.net.forward()
        d = np.asarray(out, dtype=np.float32).reshape(self.input_size, self.input_size)
        u8, lo, hi = depth_to_u8(d)
        # 最近邻/线性都行：深度只用于显示和后续点云，不做边缘精确匹配。
        u8 = cv2.resize(u8, (w, h), interpolation=cv2.INTER_LINEAR)
        return u8, lo, hi


class DepthWorker:
    """把慢速深度推理挪到后台线程，摄像头/姿态预览不被 0.3s 的推理卡住。

    队列只保留最新一帧：深度是"看趋势"的，追不上就丢旧帧，不积压。
    """

    def __init__(self, estimator: DepthEstimator):
        self._est = estimator
        self._q: "queue.Queue[np.ndarray | None]" = queue.Queue(maxsize=1)
        self._lock = threading.Lock()
        self._latest: tuple[np.ndarray, float, float] | None = None
        self._error: str | None = None
        self._thread = threading.Thread(target=self._run, name="depth-worker", daemon=True)
        self._thread.start()

    def _run(self) -> None:
        while True:
            rgb = self._q.get()
            if rgb is None:
                return
            try:
                out = self._est.process(rgb)
                with self._lock:
                    self._latest = out
            except Exception as exc:  # 推理失败不应把采集主循环打死
                with self._lock:
                    self._error = str(exc)

    def submit(self, rgb: np.ndarray) -> None:
        # 丢旧帧：只保留最新画面，避免深度越跑越落后。
        try:
            self._q.get_nowait()
        except queue.Empty:
            pass
        try:
            self._q.put_nowait(rgb)
        except queue.Full:
            pass

    def process_sync(self, rgb: np.ndarray) -> tuple[np.ndarray, float, float]:
        """图片模式用：同步跑一次，不经过队列。"""
        return self._est.process(rgb)

    def get(self) -> tuple[np.ndarray, float, float] | None:
        with self._lock:
            return self._latest

    def error(self) -> str | None:
        with self._lock:
            return self._error

    def close(self) -> None:
        # 清空积压，保证线程能拿到哨兵退出。
        while True:
            try:
                self._q.get_nowait()
            except queue.Empty:
                break
        try:
            self._q.put_nowait(None)
        except queue.Full:
            pass
        self._thread.join(timeout=2.0)


def synthetic_frame(t: float, width: int, height: int) -> tuple[np.ndarray, PoseResult, dict[str, Any]]:
    """无摄像头/无模型时的合成动捕帧，用来验证编辑器的协议和叠加。

    生成一个左右摆动的小人：2D 骨架、一个椭圆轮廓、以及一个上远下近的假深度。
    """
    img = np.zeros((height, width, 3), dtype=np.uint8)
    # 地面渐变
    for y in range(height):
        k = y / max(height - 1, 1)
        img[y, :, :] = (18 + int(18 * k), 20 + int(24 * k), 28 + int(30 * k))
    cx, cy = width * 0.5, height * 0.60
    sway = 0.055 * math.sin(t * 1.2)
    # 2D 关键点：只填常用点，其余放在髋部；编辑器只画可见连接。
    pts = np.full((33, 2), [cx, cy], dtype=np.float32)
    def put(i: int, dx: float, dy: float) -> None:
        pts[i] = [cx + dx * width + sway * width, cy + dy * height]
    put(0, 0.0, -0.30)
    put(1, -0.012, -0.315)
    put(2, -0.024, -0.325)
    put(3, -0.034, -0.335)
    put(4, 0.012, -0.315)
    put(5, 0.024, -0.325)
    put(6, 0.034, -0.335)
    put(7, -0.045, -0.335)
    put(8, 0.045, -0.335)
    put(9, -0.020, -0.285)
    put(10, 0.020, -0.285)
    put(11, -0.070, -0.17)
    put(12, 0.070, -0.17)
    put(13, -0.095, -0.07)
    put(14, 0.095, -0.07)
    put(15, -0.105, 0.03)
    put(16, 0.105, 0.03)
    put(17, -0.105, 0.10)
    put(18, 0.105, 0.10)
    put(19, -0.105, 0.03)
    put(20, 0.105, 0.03)
    put(21, -0.105, 0.10)
    put(22, 0.105, 0.10)
    put(23, -0.035, 0.01)
    put(24, 0.035, 0.01)
    put(25, -0.045, 0.12)
    put(26, 0.045, 0.12)
    put(27, -0.050, 0.23)
    put(28, 0.050, 0.23)
    put(29, -0.055, 0.34)
    put(30, 0.055, 0.34)
    put(31, -0.055, 0.39)
    put(32, 0.055, 0.39)
    landmarks = []
    world = []
    for i, (x, y) in enumerate(pts):
        nx, ny = float(x / width), float(y / height)
        # 2D 的 z 在 MediaPipe 里是相对深度；这里给一个很小的值。
        landmarks.append({"x": round(nx, 6), "y": round(ny, 6), "z": 0.0, "visibility": 0.99})
        # 世界坐标：x 向右、y 向下（MediaPipe 约定），z 向后为正；单位近似米。
        wx = (nx - 0.5) * 1.8
        wy = (ny - 0.60) * 1.9
        wz = 0.02 * math.sin(t * 0.9 + i * 0.3)
        world.append(
            {
                "x": round(wx, 6),
                "y": round(wy, 6),
                "z": round(wz, 6),
                "visibility": 0.99,
            }
        )
    # 画 2D 骨架
    for a, b in POSE_CONNECTIONS:
        pa = (int(pts[a][0]), int(pts[a][1]))
        pb = (int(pts[b][0]), int(pts[b][1]))
        cv2.line(img, pa, pb, (80, 220, 140), 2, cv2.LINE_AA)
    for x, y in pts:
        cv2.circle(img, (int(x), int(y)), 3, (255, 210, 80), -1, cv2.LINE_AA)
    # 轮廓：一个包住躯干/腿的椭圆，模拟分割 mask 的外轮廓
    contour = cv2.ellipse2Poly(
        (int(cx + sway * width), int(cy - 0.02 * height)),
        (int(width * 0.115), int(height * 0.40)),
        0, 0, 360, 12,
    )
    cv2.polylines(img, [contour], True, (60, 180, 255), 2, cv2.LINE_AA)
    poly = [[round(float(p[0] / width), 6), round(float(p[1] / height), 6)] for p in contour]
    silhouette = {
        "present": True,
        "polygon": poly,
        "bbox": [round(cx / width - 0.115, 6), round(cy / height - 0.42, 6), 0.23, 0.80],
        "area": 0.12,
    }
    # 假深度：上方远、下方近（只是为了让编辑器有东西显示）
    yy = np.linspace(0.0, 1.0, height, dtype=np.float32)[:, None]
    depth = np.repeat(yy, width, axis=1)
    depth_u8, dlo, dhi = depth_to_u8(depth)
    return img, PoseResult(landmarks, world, True, 0.99, None), {
        "silhouette": silhouette,
        "depth_u8": depth_u8,
        "depth_lo": dlo,
        "depth_hi": dhi,
    }


def render_preview(
    bgr: np.ndarray,
    pose: PoseResult,
    silhouette: dict[str, Any] | None,
    depth_u8: np.ndarray | None,
    show_depth: bool,
    status: str,
) -> np.ndarray:
    out = bgr.copy()
    h, w = out.shape[:2]
    if pose.present and pose.landmarks:
        for a, b in POSE_CONNECTIONS:
            if a >= len(pose.landmarks) or b >= len(pose.landmarks):
                continue
            la, lb = pose.landmarks[a], pose.landmarks[b]
            if la["visibility"] < 0.2 or lb["visibility"] < 0.2:
                continue
            pa = (int(la["x"] * w), int(la["y"] * h))
            pb = (int(lb["x"] * w), int(lb["y"] * h))
            cv2.line(out, pa, pb, (80, 230, 140), 2, cv2.LINE_AA)
        for lm in pose.landmarks:
            if lm["visibility"] < 0.2:
                continue
            cv2.circle(out, (int(lm["x"] * w), int(lm["y"] * h)), 3, (255, 220, 80), -1, cv2.LINE_AA)
    if silhouette and silhouette.get("polygon"):
        poly = np.array(
            [[int(x * w), int(y * h)] for x, y in silhouette["polygon"]], dtype=np.int32
        )
        if len(poly) >= 3:
            cv2.polylines(out, [poly], True, (60, 180, 255), 2, cv2.LINE_AA)
    if show_depth and depth_u8 is not None:
        inset = cv2.applyColorMap(depth_u8, cv2.COLORMAP_TURBO)
        inset = cv2.resize(inset, (w // 3, h // 4), interpolation=cv2.INTER_NEAREST)
        out[0 : h // 4, w - w // 3 : w] = inset
    cv2.rectangle(out, (0, 0), (w, 26), (0, 0, 0), -1)
    cv2.putText(out, status, (8, 18), cv2.FONT_HERSHEY_SIMPLEX, 0.55, (230, 230, 230), 1, cv2.LINE_AA)
    return out


def write_feed(
    out_dir: Path,
    frame_index: int,
    width: int,
    height: int,
    fps: float,
    source: str,
    mirrored: bool,
    rgb: np.ndarray,
    pose: PoseResult,
    silhouette: dict[str, Any] | None,
    depth_u8: np.ndarray | None,
    depth_lo: float | None,
    depth_hi: float | None,
    points: np.ndarray | None,
    point_step: int,
) -> None:
    """写一帧。先写像素，最后写 json；读端以 json 为准。"""
    atomic_write(out_dir / "latest.rgb", np.ascontiguousarray(rgb).tobytes())
    if depth_u8 is not None:
        atomic_write(out_dir / "latest.depth", np.ascontiguousarray(depth_u8).tobytes())
    if points is not None:
        atomic_write(out_dir / "latest.points", np.ascontiguousarray(points).tobytes())
    payload: dict[str, Any] = {
        "schema": "quench3d.mocap/1",
        "version": 1,
        "frame": frame_index,
        "timestamp_ms": int(time.time() * 1000),
        "source": source,
        "width": width,
        "height": height,
        "fps": round(fps, 2),
        "mirrored": mirrored,
        "intrinsics": approximate_intrinsics(width, height),
        "pose": {
            "present": pose.present,
            "confidence": round(pose.confidence, 4),
            "landmarks": pose.landmarks,
            "world": pose.world,
            "world_units": "meters",
            # MediaPipe 世界点：原点=两髋中点；x 向右、y 向下、z 向后为正。
            "world_axes": "x-right,y-down,z-back",
        },
        "silhouette": silhouette
        if silhouette is not None
        else {"present": False, "polygon": [], "bbox": None, "area": 0.0},
        "points": {
            "present": points is not None and len(points) > 0,
            "path": "latest.points" if points is not None and len(points) > 0 else None,
            "count": int(len(points)) if points is not None else 0,
            "layout": "xyz_f32_le",
            "space": "engine",
            "step": int(point_step),
            "bounds": (
                [
                    round(float(points[:, 0].min()), 4),
                    round(float(points[:, 1].min()), 4),
                    round(float(points[:, 2].min()), 4),
                    round(float(points[:, 0].max()), 4),
                    round(float(points[:, 1].max()), 4),
                    round(float(points[:, 2].max()), 4),
                ]
                if points is not None and len(points) > 0
                else None
            ),
            "note": "由相对逆深度反投影，z 是 0.3~3.0 的任意尺度，不是米制距离。",
        },
        "depth": {
            "present": depth_u8 is not None,
            "kind": "relative_inverse_depth",
            "path": "latest.depth" if depth_u8 is not None else None,
            "width": width,
            "height": height,
            "raw_min": round(depth_lo, 6) if depth_lo is not None else None,
            "raw_max": round(depth_hi, 6) if depth_hi is not None else None,
            "note": "MiDaS 相对逆深度，归一化到 0..255；不是米制距离。",
        },
    }
    atomic_write(out_dir / "latest.json", json.dumps(payload, ensure_ascii=False).encode("utf-8"))


def download_depth_model(dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    print(f"下载 MiDaS small（约 64 MB）→ {dest}")
    tmp = dest.with_name(dest.name + ".part")
    urllib.request.urlretrieve(DEFAULT_DEPTH_URL, tmp)
    os.replace(tmp, dest)
    print("完成")


def open_source(source: str, width: int, height: int, fps: int) -> tuple[cv2.VideoCapture | None, bool]:
    """返回 (capture, is_image)；`synthetic` 返回 (None, False)。"""
    if source == "synthetic":
        return None, False
    if source.isdigit():
        cap = cv2.VideoCapture(int(source), cv2.CAP_V4L2)
        if not cap.isOpened():
            cap = cv2.VideoCapture(int(source))
        cap.set(cv2.CAP_PROP_FRAME_WIDTH, width)
        cap.set(cv2.CAP_PROP_FRAME_HEIGHT, height)
        cap.set(cv2.CAP_PROP_FPS, fps)
        return cap, False
    path = Path(source)
    if path.suffix.lower() in {".jpg", ".jpeg", ".png", ".bmp", ".webp"}:
        # 图片走 VideoCapture 也能读，但只读一帧；用 imread 更稳。
        return cv2.VideoCapture(str(path)), True
    return cv2.VideoCapture(str(path)), False


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Quench3D 摄像头动捕采集端（第一阶段）")
    ap.add_argument("--source", default="0", help="摄像头编号（0）、视频/图片路径，或 synthetic")
    ap.add_argument("--out", default="target/camera", help="输出目录（编辑器轮询这里）")
    ap.add_argument("--width", type=int, default=640)
    ap.add_argument("--height", type=int, default=480)
    ap.add_argument("--fps", type=int, default=30)
    ap.add_argument("--model-complexity", type=int, default=1, choices=[0, 1, 2])
    ap.add_argument("--no-display", action="store_true", help="不弹 OpenCV 预览窗口")
    ap.add_argument("--max-frames", type=int, default=0, help="跑 N 帧后退出；0 = 不限")
    ap.add_argument("--no-depth", action="store_true", help="关闭单目深度")
    ap.add_argument("--depth-model", default="target/models/midas_small.onnx")
    ap.add_argument("--depth-every", type=int, default=3, help="每 N 帧跑一次深度")
    ap.add_argument("--no-points", action="store_true", help="不写深度反投影点云")
    ap.add_argument("--point-step", type=int, default=6, help="点云像素下采样步长，缺省 6")
    ap.add_argument("--download-depth-model", action="store_true")
    ap.add_argument("--no-mirror", action="store_true", help="不水平翻转画面（缺省镜像，像照镜子）")
    args = ap.parse_args(argv)

    if args.download_depth_model:
        download_depth_model(Path(args.depth_model))
        return 0

    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)

    cap: cv2.VideoCapture | None = None
    is_image = False
    if args.source != "synthetic":
        cap, is_image = open_source(args.source, args.width, args.height, args.fps)
        if cap is None or not cap.isOpened():
            print(f"无法打开输入源：{args.source}", file=sys.stderr)
            return 2

    pose_est: PoseEstimator | None = None
    depth_worker: DepthWorker | None = None
    if args.source != "synthetic":
        pose_est = PoseEstimator(args.model_complexity, static_image_mode=is_image)
        if not args.no_depth:
            try:
                depth_worker = DepthWorker(DepthEstimator(Path(args.depth_model)))
            except RuntimeError as exc:
                print(f"警告：{exc}\n先继续跑姿态/轮廓；加 --no-depth 可静默关闭。", file=sys.stderr)
                depth_worker = None

    frame_index = 0
    last_depth: tuple[np.ndarray | None, float | None, float | None] = (None, None, None)
    t_start = time.time()
    t_last = t_start
    show_depth = False
    try:
        while True:
            if args.source == "synthetic":
                bgr, pose, extra = synthetic_frame(time.time() - t_start, args.width, args.height)
                silhouette = extra["silhouette"]
                depth_u8 = extra["depth_u8"]
                depth_lo, depth_hi = extra["depth_lo"], extra["depth_hi"]
            else:
                assert cap is not None
                ok, bgr = cap.read()
                if not ok:
                    break
                if args.width and args.height:
                    bgr = cv2.resize(bgr, (args.width, args.height), interpolation=cv2.INTER_AREA)
                if not args.no_mirror:
                    bgr = cv2.flip(bgr, 1)
                rgb = cv2.cvtColor(bgr, cv2.COLOR_BGR2RGB)
                assert pose_est is not None
                pose = pose_est.process(rgb)
                silhouette = None
                if pose.mask is not None:
                    silhouette = largest_contour_polygon(pose.mask)
                depth_u8, depth_lo, depth_hi = None, None, None
                if depth_worker is not None:
                    if is_image:
                        # 图片只跑一帧：同步等结果，避免循环退出时深度还没算完。
                        last_depth = depth_worker.process_sync(cv2.cvtColor(bgr, cv2.COLOR_BGR2RGB))
                    elif frame_index % max(args.depth_every, 1) == 0:
                        depth_worker.submit(cv2.cvtColor(bgr, cv2.COLOR_BGR2RGB))
                    result = depth_worker.get()
                    if result is not None:
                        last_depth = result
                    depth_u8, depth_lo, depth_hi = last_depth
                    err = depth_worker.error()
                    if err:
                        print(f"深度推理失败（继续姿态/轮廓）：{err}", file=sys.stderr)

            if args.source == "synthetic":
                rgb = cv2.cvtColor(bgr, cv2.COLOR_BGR2RGB)

            points = None
            if not args.no_points and depth_u8 is not None:
                points = depth_to_points(
                    depth_u8,
                    approximate_intrinsics(rgb.shape[1], rgb.shape[0]),
                    args.point_step,
                )

            now = time.time()
            fps = 1.0 / max(now - t_last, 1e-6)
            t_last = now
            write_feed(
                out_dir,
                frame_index,
                rgb.shape[1],
                rgb.shape[0],
                fps,
                args.source,
                not args.no_mirror,
                rgb,
                pose,
                silhouette,
                depth_u8,
                depth_lo,
                depth_hi,
                points,
                args.point_step,
            )
            frame_index += 1

            if not args.no_display:
                status = (
                    f"frame {frame_index}  {fps:4.1f} fps  pose={pose.confidence:.2f}  "
                    f"depth={'on' if depth_u8 is not None else 'off'}  "
                    f"[s]保存 [d]深度 [q]退出"
                )
                preview = render_preview(bgr, pose, silhouette, depth_u8, show_depth, status)
                cv2.imshow("Quench3D camera mocap", preview)
                key = cv2.waitKey(1) & 0xFF
                if key == ord("q") or key == 27:
                    break
                if key == ord("d"):
                    show_depth = not show_depth
                if key == ord("s"):
                    snap = out_dir / f"snapshot-{frame_index:06d}.jpg"
                    cv2.imwrite(str(snap), preview)
                    print(f"已保存 {snap}")

            if args.max_frames and frame_index >= args.max_frames:
                break
    finally:
        if cap is not None:
            cap.release()
        if pose_est is not None:
            pose_est.close()
        if depth_worker is not None:
            depth_worker.close()
        if not args.no_display:
            cv2.destroyAllWindows()
    print(f"已写 {frame_index} 帧到 {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
