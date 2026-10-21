# 摄像头动捕（第一阶段）

用一台普通 RGB 摄像头实时得到：

* **轮廓**：MediaPipe Pose 的人体分割 mask → 最大外轮廓多边形；
* **2D 姿态**：33 个归一化关键点；
* **3D 姿态**：33 个以髋部为原点的世界点（近似米，MediaPipe 输出）；
* **深度**：可选的 MiDaS small 单目**相对逆深度**（0..255，不是米）；
* **点云**：把深度按相机内参反投影成 `latest.points`（缺省每 6 像素取一点）；
  z 被映射到 0.3~3.0 的任意尺度，只用于观察结构；
* **编辑器叠加**：`quench3d-editor` 显示摄像头画面 + 轮廓/骨架，并把 3D 世界点画成
  青绿色骨骼、黄色关节点；勾选「显示点云」后画出深度反投影点。

这是**单帧单人的参考实现**，不是产品级动作捕捉：没有 SMPL/SMPL-X 体型、没有
多相机/多人体、没有米制深度、没有时间同步/IMU 融合、没有 TSDF 网格重建。

## 1. 安装

不要用系统 Python 的旧 OpenCV。建一个 venv：

```bash
cd quench3d
python3 -m venv target/tmp/camvenv
target/tmp/camvenv/bin/python -m pip install -r tools/camera_mocap/requirements.txt
```

系统要求：Linux + V4L2 摄像头（`/dev/video0`）。用户要在 `video` 组里，或者
`sudo usermod -aG video "$USER"` 后重新登录。先确认系统能出图：

```bash
ffmpeg -f v4l2 -i /dev/video0 -frames:v 1 -f null -
```

## 2. 跑起来

```bash
# 真摄像头（0 = /dev/video0）
target/tmp/camvenv/bin/python tools/camera_mocap/camera_mocap.py --source 0
# 不要点云时加 --no-points；改下采样步长用 --point-step 6

# 没有摄像头时，用合成源验证编辑器协议
target/tmp/camvenv/bin/python tools/camera_mocap/camera_mocap.py --source synthetic

# 图片/视频（不需要摄像头）
target/tmp/camvenv/bin/python tools/camera_mocap/camera_mocap.py \
  --source path/to/pose.jpg --no-display --max-frames 1
```

OpenCV 预览窗口的按键：

* `d`：叠加深度图；
* `s`：保存一张快照到输出目录；
* `q` / `Esc`：退出。

采集端默认把最新帧写到 `target/camera/`。编辑器启动时会轮询这里；如果输出目录
不同：

```bash
QUENCH3D_CAMERA_DIR=/path/to/feed cargo run -p quench3d-editor
```

编辑器里的浮动窗口「摄像头动捕（实验）」会显示画面/轮廓/骨架；3D 骨骼固定放在
世界坐标 `(2.2, 1.0, 0.0)`，默认场景相机可见。

## 3. 深度模型

MiDaS small ONNX 约 64 MB，不提交到仓库。需要时显式下载一次：

```bash
target/tmp/camvenv/bin/python tools/camera_mocap/camera_mocap.py --download-depth-model
# 或
curl -L -o target/models/midas_small.onnx \
  https://github.com/isl-org/MiDaS/releases/download/v2_1/model-small.onnx
```

没有模型时工具会打印警告并继续跑姿态/轮廓；加 `--no-depth` 可以静默关闭。

CPU 上 MiDaS small 约 **0.2~0.3 秒/帧**，所以缺省每 3 帧跑一次（`--depth-every`），
预览里的深色图和骨骼仍按相机帧率刷新。

**深度语义**：MiDaS 输出的是**相对逆深度**（值越大越近），每帧被归一化到 0..255。
它没有米制尺度，不能直接用来量距离/做物理碰撞；点云也只是把相对值映射到
0.3~3.0 的任意尺度。下一步做米制点云/TSDF 前必须解决尺度（已知相机内参 + 平面
标定，或 RGB-D 硬件）。

## 4. 输出协议

```
target/camera/
  latest.json    元数据 + 33 个 2D/3D 关键点 + 轮廓多边形 + 深度统计
  latest.rgb     width*height*3 的 RGB24（已按 --no-mirror 决定是否镜像）
  latest.depth   width*height 的 u8 归一化深度（可选）
  latest.points  N*3 的 little-endian f32 点云（可选，缺省开启）
```

`latest.json` 的关键字段：

```json
{
  "schema": "quench3d.mocap/1",
  "version": 1,
  "frame": 42,
  "timestamp_ms": 1790000000000,
  "source": "0",
  "width": 640,
  "height": 480,
  "fps": 29.8,
  "mirrored": true,
  "intrinsics": { "fx": 554.3, "fy": 554.3, "cx": 320.0, "cy": 240.0, "hfov_deg": 60.0 },
  "pose": {
    "present": true,
    "confidence": 0.93,
    "landmarks": [{ "x": 0.5, "y": 0.5, "z": 0.0, "visibility": 0.99 }],
    "world": [{ "x": 0.0, "y": 0.0, "z": 0.0, "visibility": 0.99 }],
    "world_units": "meters",
    "world_axes": "x-right,y-down,z-back"
  },
  "silhouette": { "present": true, "polygon": [[0.1, 0.2]], "bbox": [0.1, 0.2, 0.3, 0.6], "area": 0.12 },
  "depth": {
    "present": true, "kind": "relative_inverse_depth", "path": "latest.depth",
    "width": 640, "height": 480, "raw_min": 0.0, "raw_max": 737.6,
    "note": "MiDaS 相对逆深度，归一化到 0..255；不是米制距离。"
  },
  "points": {
    "present": true, "path": "latest.points", "count": 8560,
    "layout": "xyz_f32_le", "space": "engine", "step": 6,
    "bounds": [0.4679, 0.7813, -3.0, 3.9104, 2.299, -0.3318],
    "note": "由相对逆深度反投影，z 是 0.3~3.0 的任意尺度，不是米制距离。"
  }
}
```

写入顺序是 **rgb/depth 先、json 后**，每个文件都是临时文件 + `os.replace`。读端
（`crates/quench3d-editor/src/camera.rs`）只在 json 合法且像素长度匹配时接收一帧。

坐标约定：

* 2D：图像左上角 (0,0)，右下角 (1,1)，已镜像；
* 3D `world`：MediaPipe 约定，原点=两髋中点，x 向右、y **向下**、z 向后为正；
  编辑器转成引擎坐标时 `y/z` 取反并平移到 `(2.2, 1.0, 0.0)`。

## 5. 编辑器侧

`quench3d-editor` 的 `camera.rs`：

* 后台线程每 33 ms 读一次 `latest.json` + `latest.rgb`；帧号变化才更新；
* egui 浮动窗口：RGB + 2D 骨架 + 轮廓；可选深度图与点云；
* `add_lines`：把 33 个 world 点画成 35 根骨 + 关节小球；勾选点云时把
  `latest.points` 画成小十字；数据停更 2 秒自动消失；
* Web 端是空实现（浏览器没有本地文件轮询；Web 摄像头是另一条管线）。

回归测试（不需要摄像头）：

```bash
cargo test -p quench3d-editor --lib camera
target/tmp/camvenv/bin/python -m unittest discover -s tools/camera_mocap -p 'test_*.py'
```

Rust 侧测了协议解析、像素/点云长度校验、坐标翻转、35 根骨 + 33 个关节的线段数量、
每个点 3 条十字线段；Python 侧测了深度归一化、点云反投影、轮廓提取、关键点连接表。

## 6. 隐私与许可

* 采集、姿态、分割、深度**全部在本机**；脚本没有上传路径，唯一联网的地方是显式
  `--download-depth-model`（从 GitHub Release 拉模型）。
* 不要提交 `target/camera/` 或 `target/models/`；它们都在 `.gitignore` 的 `target/` 下。
* MediaPipe 是 Apache-2.0；MiDaS 模型/代码的许可以上游仓库为准。仓库不附带模型权重，
  使用者自己下载并遵守上游许可。

## 7. 常见问题

| 现象 | 处理 |
| --- | --- |
| `无法打开输入源：0` | 检查 `/dev/video0`、用户在 `video` 组、`ffmpeg` 能读到 |
| 编辑器一直「还没有收到帧」 | 先看输出目录是否有 `latest.json`；`QUENCH3D_CAMERA_DIR` 是否一致 |
| 有画面但没有骨架 | 光线/全身入镜；MediaPipe Pose 需要可见的肩/髋/膝 |
| 深度窗口全黑/很慢 | 没模型、CPU 推理慢；这是相对深度，不是夜视 |
| 骨架位置和场景对不上 | 第一阶段固定 `(2.2,1,0)`；后续会加放置/缩放控制 |
