# CLI 与 MCP 使用参考

[返回项目入口](../README.md)。本页包含详细操作与接口说明；机器可读参数以 `quench3d schema --json` 为准。

## 选择 agent 接口

本页描述文件／内联 JSON 驱动的 CLI 和 stdio MCP 工具。要控制已打开编辑器中的实时场景，请读[编辑器 AI 工具契约](ai_agent.md#current-tools-and-contracts)，不要混用两套工具名。

先执行 `quench3d schema --json` 获取当前版本的机器可读能力，校验后再仿真／渲染。CLI `chat --max-rounds` 的轮次限制与编辑器助手的持续请求机制相互独立。自然语言生成与工具调用均需要兼容模型服务，物理仿真本身不需要模型或 GPU。

## 6. 命令行工具（面向 agent 与自动化）

`crates/quench3d-cli` 提供 `quench3d` 可执行文件：**渲染 / 纯物理仿真 / 模型检查 / 大模型生成场景 /
MCP 工具服务器**。它的设计目标只有一个——让 agent 与脚本能可靠地驱动 3D 引擎，所以：

* 每条命令都支持 `--json`，输出**单行** JSON 信封；
* 退出码固定语义（0 成功 / 2 输入不合法 / 3 环境不可用 / 4 网络与大模型问题）；
* 诊断带 **JSON 路径**与修复建议，且**一次返回全部问题**；
* **物理路径完全不需要 GPU**（agent 的高频循环跑 `simulate`，只有出图才需要显卡）。

### 6.1 安装与自检

```bash
tools/install_cli.sh                    # Linux / macOS（--local 只在本仓库构建）
powershell -File tools\install_cli.ps1  # Windows（-Local 同上）
cargo install --path crates/quench3d-cli # 等价的手动方式

quench3d doctor --json                   # 平台 / CPU / GPU 适配器 / 后端 / 是否软件光栅
quench3d schema --json                   # 能力清单：命令、参数、场景语法、退出码（agent 入口）
```

`doctor` 的真实输出（本机只有软件 Vulkan）：

```json
{"ok":true,"command":"doctor","elapsed_ms":110,"data":{"os":"linux","arch":"x86_64","cpu_threads":4,
 "gpu_available":true,"adapter":"llvmpipe (LLVM 15.0.7, 256 bits)","backend":"Vulkan","device_type":"Cpu",
 "software_rasterizer":true,"notes":["软件光栅化：渲染可用但较慢，建议分辨率不超过 1280×800"]}}
```

### 6.2 命令一览

| 命令 | 作用 | 需要 GPU |
| --- | --- | --- |
| `schema` | 输出能力清单（命令 / 参数 / 场景语法 / 单位 / 退出码 / 示例） | 否 |
| `doctor` | 环境自检 | 否 |
| `info <模型>` | glTF/GLB/OBJ/STL：网格、材质、贴图、顶点、三角形、包围盒 | 否 |
| `validate <场景>` | 校验场景，返回全部诊断（带 JSON 路径） | 否 |
| `export <场景> [模型.obj\|模型.gltf\|模型.glb]` | 图元物体 → OBJ + MTL；扩展名 `.gltf` 导出文本 glTF 2.0（base64 内嵌几何/贴图/动画），`.glb` 导出二进制 GLB（BIN chunk），本引擎均可重新导入 | 否 |
| `simulate <场景>` | 只跑物理，报告每个刚体的位置 / 速度 / 位移 / 休眠 / 接触，可按 `--trace` 采集轨迹 | 否 |
| `record <场景> <文件>` | 跑物理并把刚体位姿录成版本化 Q3RP 回放（位置/旋转/缩放，带校验和） | 否 |
| `replay <文件>` | 校验 Q3RP 回放并输出帧数/时长/刚体数摘要；可配 `--scene`/`--at`/`--render` 出该时刻 PNG | 渲染时是 |
| `query <场景>` | 几何查询：`--ray` 射线（`--all` 给沿路全部命中）/ `--overlap` 任意形状重叠，可配层掩码与传感器开关 | 否 |
| `render <场景>` | 无窗口渲染成 PNG（可先步进物理） | 是 |
| `movie <场景>` | 录制仿真视频：PNG 序列 + `trace.jsonl`（附编码命令），或 `--encoder gif\|apng` 直接出单文件 | 是 |
| `example` | 输出可直接运行的示例场景 JSON | 否 |
| `ask "<需求>"` | 大模型把自然语言变成场景，校验后可接着渲染 / 仿真 | 渲染时需要 |
| `chat [需求]` | 命令行 agent：模型自己调用校验/仿真/渲染工具，多轮直到给出结论 | 调用渲染时 |
| `mcp` | 以 MCP 服务器运行（stdio），把上面这些暴露成工具 | 调用渲染工具时 |

```bash
quench3d example --out scene.json
quench3d validate scene.json --json
quench3d export scene.json model.obj                    # 图元 → OBJ/MTL
quench3d export scene.json model.gltf                   # 图元 → 单文件 glTF 2.0
quench3d export scene.json model.glb                    # 同一份场景 → 二进制 GLB
quench3d export scene.json model.stl                    # 图元 → 二进制 STL（3D 打印）
quench3d simulate scene.json --steps 600 --json
quench3d simulate scene.json --trace-all --json            # 附带每条轨迹的结论
quench3d record scene.json replay.q3r                        # 刚体位姿回放（Q3RP）
quench3d replay replay.q3r --json                            # 校验并汇总回放
quench3d replay replay.q3r --scene scene.json --at 0.5 --render replay.png  # 出回放某一刻
quench3d render scene.json --out scene.png --width 960 --height 600
quench3d movie scene.json --frames 300 --frames-dir frames   # 录 5 秒（60fps，1 步/帧）
quench3d query scene.json --ray 0,5,0,0,-1,0 --json          # 这条射线上有什么（最近一个）
quench3d query scene.json --ray 0,1,0,1,0,0 --all --mask 5   # 沿路全部命中，只看第 0/2 层
quench3d query scene.json --overlap "box:2,1,2@0,1,0~0,45,0" # 斜 45° 的探针盒罩住了谁
```

`query` 是**只读**的：不渲染、不写盘、不需要显卡，`data` 里有 `hits[]`（名字 / 类型 /
距离 / 点 / 法线 / 层 / 是否传感器）与 `tested`（本次实际检查了几个刚体）。
层掩码是**单向**判据（只看被查方在不在掩码里），触发区用 `--no-sensors` 排除；
`--steps N` 可以先跑 N 步物理再查（"落地之后那个箱子在哪"），
`--index` 换用查询加速结构（单次查询反而更贵，见 §9 边界）。

**仿真视频（`movie`）**：逐帧推进物理并渲染。三条路：PNG 序列交给 `ffmpeg`、
用**内置纯 Rust 编码器**直接出 GIF/APNG（不装任何外部工具），或 `--encoder mp4`
让引擎调外部 `ffmpeg` 直接封装 MP4。加 `--replay <Q3RP>` 则**不推进物理**，
按 `record` 录下的固定步长写回刚体位姿再渲染（句柄必须属于同一场景）：

```bash
# 路线 A：PNG 序列 + ffmpeg（长视频、高压缩率）
quench3d movie scene.json --frames 300 --frames-dir frames --json
# data.frames_dir / data.trace / data.digest / data.ffmpeg / data.probes（写了 probes 才有）
ffmpeg -framerate 60 -i frames/frame_%05d.png -c:v libx264 -preset slow -crf 18 \
       -pix_fmt yuv420p -movflags +faststart movie.mp4

# 路线 C：引擎直接调 ffmpeg 编 MP4（需要 PATH 里有 ffmpeg；CRF 18 / yuv420p）
quench3d movie scene.json --frames 300 --encoder mp4 --out clip.mp4 --json

# 预检：只校验 + 跑物理（几毫秒），不花 GPU 时间、不写任何文件
quench3d render scene.json --dry-run --json
# data.dry_run / data.would_write / data.simulate（物理是真跑的，渲染统计全为 0）

# 多 agent 并行：输出已存在就报错（退出码 2，error.kind = output_exists），互相不覆盖
quench3d render scene.json --out mine.png --no-clobber --json

# 路线 B：内置编码器直接出可播放文件（README 演示、几十帧的短循环）
quench3d movie scene.json --frames 60 --encoder gif  --out demo.gif   # 256 色
quench3d movie scene.json --frames 60 --encoder apng --out demo.png   # 无损

# 回放录制好的刚体位姿（不跑物理；只回放位姿，不含速度/流体/关节）
quench3d movie scene.json --replay replay.q3r --frames 120 --frames-dir replay
```

内置编码器是流式写入（不把整片缓存进内存），**换编码器不改变 `data.digest`**——
像素没变，只是封装变了（端到端测试钉住了这条）。GIF 的格式限制会明说：
帧延迟只有 10 ms 粒度，`--fps 60` 会被量化成 50 fps，`data.notes` 里直接算出
「0.40 s → 0.48 s」，不让你对着变长的视频纳闷。

三条设计约定，让它与「屏幕录像」不是一回事：

* **帧数由物理步决定，不看墙钟**：`时长 = frames × steps-per-frame × dt`，
  所以视频时长严格等于仿真时间（本地实测：300 帧 → ffprobe 报 `duration=5.000000`、`nb_frames=300`）。
* **可复现**：同一场景两次录制逐像素一致，报告的 `data.digest` 就是断言依据
  （`fnv1a64:…`，端到端测试已钉住这条）。
* **可续录**：`--frames-dir` 里已存在的帧会跳过（`data.skipped`），长视频中断后重跑即可接上。

每帧还会写一行 `trace.jsonl`（帧号、累计步数、仿真时间、绘制统计），视频因此可以被程序核对。
`--warmup` 丢弃预热帧，`--fps` 只改播放速度（调小=慢动作），`--steps-per-frame` 才改仿真粒度。

**轨迹采样（`--trace`）**：agent 经常要回答「会不会倒、滑多远、弹多高」这类问题，
光看最终位置不够。`--trace 名字[,名字]` / `--trace-all` 会逐步记录刚体状态并给出结论：

```text
轨迹 塔2  : 起点 (3.00,1.80,0.00) → 终点 (0.42,0.30,0.23)
            下坠 1.50 m · 爬升 0.00 m · 位移 2.99 m · 路程 3.23 m · 最高速 4.58 m/s
            形状 box · 最大倾斜 90.5° · 翻倒 · 最后运动在第 125 步
            高度曲线 ██████████████▇▇▆▅▄▃▃▂▂▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁（0.30 ~ 1.80 m）
```

JSON 里对应 `data.trajectories[]`：`samples[{step,time,position,speed,displacement,tilt_deg,asleep}]`
加派生指标 `fall` / `rise` / `travel` / `path_length` / `max_speed` / `max_tilt_deg` /
`tipped_over` / `last_motion_step`。球体与平面会随意自转，此时 `tilt_meaningful` 为 `false`，
`tipped_over` 恒为 `false`（不会把「球滚了」误判成「球倒了」）。

`simulate` 的人可读输出长这样（示例场景：斜面 + 两个箱子 + 一个球）：

```text
步数      : 180（dt = 0.01667s，重力 [0.0, -9.81, 0.0]）
刚体      : 5（动态 3，休眠 2）
接触/关节 : 1 / 0
最大速度  : 0.000 m/s
NaN 刚体  : 0
  球             dynamic   位置 (   2.50,   0.35,  -1.50) 速度   0.00 位移   3.65 休眠
  箱子1          dynamic   位置 (  -0.46,   1.71,  -0.00) 速度   0.00 位移   0.90 休眠
  箱子2          dynamic   位置 (   3.49,   0.40,   1.16) 速度   0.00 位移   4.54
```

### 6.3 场景文件就是 agent 与引擎的契约

场景是一个 JSON（`format: "quench3d.scene"`，当前 `version: 2`），**所有字段都有缺省值，`{}` 就是合法场景**：

```json
{
  "gravity": [0, -9.81, 0],
  "ground": { "size": 40, "material": "floor" },
  "waters": [{ "center": [0, 0], "size": [10, 6], "level": 0.0 }],
  "fluids": [{ "center": [0, 2, 0], "size": [1.6, 1.6, 1.6], "spacing": 0.1 }],
  "water": { "color": [0.06, 0.28, 0.48], "clarity": 1.0, "wave_strength": 1.0,
             "foam": 1.0, "refraction": 1.0 },
  "materials": {
    "floor": { "color": [0.55, 0.58, 0.62], "roughness": 0.9 },
    "wood":  { "color": [0.78, 0.48, 0.26], "roughness": 0.55 },
    "steel": { "color": [0.70, 0.72, 0.76], "metallic": 0.9, "roughness": 0.28 },
    "格子":  { "color": [1, 1, 1], "base_color_texture": "checker.png", "uv_scale": 4,
                "uv_offset": [0.5, 0.0], "uv_rotation": 45 },
    "玻璃":  { "color": [0.7, 0.85, 0.9], "alpha": 0.35, "alpha_blend": true, "roughness": 0.1 },
    "灯罩":  { "color": [1, 0.95, 0.85], "emissive": 3, "emissive_color": [1, 0.9, 0.7], "unlit": true }
  },
  "objects": [
    { "name": "斜坡", "shape": { "type": "box", "size": [4, 0.4, 4] },
      "position": [0, 1, 0], "rotation_deg": [0, 0, -12], "body": "static" },
    { "name": "箱子", "shape": { "type": "box", "size": [0.8, 0.8, 0.8] },
      "position": [-0.6, 2.6, 0], "material": "wood", "body": "dynamic",
      "density": 400, "restitution": 0.15, "friction": 0.7,
      "linear_damping": 0.05, "angular_damping": 0.05, "allow_sleep": true },
    { "name": "气球", "shape": { "type": "sphere", "radius": 0.4 },
      "position": [1.2, 3.2, 0], "material": "玻璃", "body": "dynamic",
      "gravity_scale": 0.15, "mass": 0.05, "angular_velocity": [0, 0, 2] },
    { "name": "灯柱", "shape": { "type": "cylinder", "radius": 0.08, "half_height": 1.5 },
      "position": [3, 1.5, 0], "body": "static" },
    { "name": "灯头", "parent": "灯柱", "shape": { "type": "sphere", "radius": 0.25 },
      "position": [0, 1.6, 0], "rotation_deg": [0, 0, 0], "material": "灯罩" }
  ],
  "joints": [
    { "type": "hinge", "a": "门", "b": "框", "axis": [0, 1, 0], "anchor_b": [0.5, 0, 0],
      "limits": [-1.57, 0], "motor_velocity": 1.5 },
    { "type": "hinge", "a": "框", "b": "闸门", "axis": [0, 1, 0],
      "motor_target_deg": 90.0, "motor_stiffness": 8.0, "motor_max_impulse": 40.0,
      "break_force": 2000.0 },          // 位置伺服：开到 90° 停住；受力超 2000 N 就断
    { "type": "ball", "a": "躯干", "b": "手臂", "cone_limit": { "half_angle_deg": 60.0 } }
  ],
  "actions": [
    { "type": "impulse", "target": "箱子", "at_time": 2.0, "value": [0, 800, 0] },
    { "type": "force", "target": "气球", "at_step": 30, "steps": 30, "value": [0, 0.5, 0] },
    { "type": "teleport", "target": "箱子", "at_step": 120, "position": [0, 4, 0] },
    { "type": "wake", "target": "*", "at_step": 150 }
  ],
  "probes": [
    { "name": "落点", "shape": "sphere", "position": [0, 0.5, 0], "radius": 1.2 },
    { "name": "门口", "shape": "box", "position": [3, 1, 0], "size": [2, 2, 2], "watch": ["箱子"] }
  ],
  "models": [ { "path": "chair.glb", "position": [2, 0, 0], "scale": 1, "collide": true } ],
  "assets": { "search_paths": ["../shared/assets"] },
  "camera": { "position": [7, 4.5, 8], "look_at": [0, 1.2, 0], "fov_deg": 50,
              "near": 0.05, "far": 5000 },
  "lighting": { "preset": "studio", "sun_direction": [-0.4, -1, -0.35], "shadows": true,
                "point_lights": [ { "position": [0, 3, 0], "intensity": 30, "radius": 25 } ],
                "fog": { "color": [0.36, 0.46, 0.63], "density": 0.03 },
                "shadow_quality": { "strength": 0.85, "radius": 28, "bias": 0.75 },
                "sky_colors": { "zenith": [0.22, 0.42, 0.78], "horizon": [0.62, 0.74, 0.88] },
                "bloom": { "intensity": 0.6, "threshold": 1.0 } },
  "physics": { "mode": "normal", "friction_combine": "min",
               "restitution_combine": "max", "static_friction_speed": 0.02 },
  "simulate": { "steps": 180, "dt": 0.0166667 },
  "render": { "output": "scene.png", "width": 960, "height": 600, "msaa": 4 }
}
```

单位：长度**米**、时间**秒**、质量由密度×体积推导、角度用**度**（字段名以 `_deg` 结尾）；
`shape.size` 是**边长不是半长**；材质颜色是 0~1 的 **sRGB** 分量。
完整字段表用 `quench3d schema --json` 读（`data.scene` 就是语法本身）。
`physics.mode` 可选 `normal`（缺省）、`drive`（无重力/接触，PD 关节回放）或
`fk`（同 drive，但铰链/滑块子树每步直接到目标角/位移，无 PD 收敛；只支持
hinge/slider）。

几处容易踩的语义（都有校验/警告兜着）：

* **贴图路径相对场景文件解析**（再回退 `assets` 搜索路径），相对路径不会跟着你的 shell 目录跑；
  支持 5 个槽位：`base_color_texture` / `normal_texture` / `metallic_roughness_texture` /
  `emissive_texture` / `ao_texture`（AO 只采样 R 通道，`ao_strength` 0~1 控制压暗程度）。
  贴图字节支持 PNG/JPEG，以及 **KTX2** 的未压缩 RGBA8 与 BC1（CPU 解成 RGBA8，
  只读 level 0）；BasisLZ/UASTC/BC7 与 GPU 块压缩上传仍未支持。
* `alpha_cutoff` 与 `alpha_blend` **同时给时裁剪优先**（会给出警告，避免你以为在混合）。
* `lighting.point_lights` 最多 8 盏生效，**不写这个字段 = 保留预设自带的补光**，写 `[]` 才是显式清空。
* `lighting.bloom` / `lighting.fog` **不写 = 关闭**（像素与关闭时逐位相同）；写了就是打开
  （fog 的 `density` 缺省 0.02，写 0 等于关）。
* `lighting.grade` 是 tonemap **之后**的调色：`saturation` / `contrast` 缺省 1（0~4），
  `temperature` 缺省 0（-1 偏冷、+1 偏暖）；不写 = 全中性、像素与旧版一致。
* `lighting.sun_color` / `ambient_color` 是**线性空间** RGB；不写 = 跟随 preset。
* `lighting.shadow_quality.resolution` 是阴影贴图边长（512~4096，缺省 2048，取偶数）；
  聚光图取一半、点光每面再取一半。显存约为 `resolution² × 4 B` 的量级。
* `objects[].static_friction` 缺省**跟随 `friction`**（只管动摩擦的老场景行为逐位不变）；
  想表达"推得动、但推住之后不会慢慢滑"就把它写得比 `friction` 大。
* `objects[].rolling_friction` 是滚动阻力（米，缺省 0 = 关闭），只对设置了它的
  球/轮子/圆柱生效；编辑器属性面板也能改。
* `objects[].friction_anisotropy`（0~0.95，缺省 0 = 各向同性）配合
  `objects[].friction_axis`（**本地**主轴，缺省 `[1,0,0]`）做方向性摩擦：沿轴
  `μ(1+a)`，垂直方向 `μ(1-a)`，静/动摩擦同步缩放。接触时取两端各向异性较大的
  那个（相同取 A），所以地面保持 0 也能让滑板/冰刀生效；旧场景逐位不变。
* `physics.friction_cone`（缺省 false）打开后两切向冲量按**向量模长**夹到 `μN`；
  缺省仍是历史方形摩擦（对角线可达 √2·μ），旧场景逐位不变。
* `models[].animation` 是**视觉**动画：`{ "name": "走路", "start": 0, "loop_start": 0.5,
  "speed": 1, "loop": true, "blend": { "name": "跑步", "loop_start": 0.25, "weight": 0.5,
  "fade": 0.25 } }`。`loop_start` 是循环子区间起点（秒，缺省 0 = 从片段开头循环），
  编辑器「动画」面板的「循环起点」可直接改并写回场景，普通片段和状态机当前状态都支持。名字在资产里存不存在**只有装载后才知道**
  （`validate` 不读资产），所以 `validate` 只查结构（名字非空、`speed > 0`、`start >= 0`、
  `weight` 在 0~1、`fade >= 0`）；真正的对账在 `simulate`/`render`/`movie` 报告的 `models[]`
  （`found` / `duration` / `tracks` / `blend` + 资产里可用的 `clips`）——名字写错只给警告，
  几何保持绑定姿态，不会让场景非法。`loop` 缺省 **true**（与 `animations[].loop` 缺省 false
  相反：模型片段通常是循环的）；`loop: false` 播完停在最后一帧。
  `models[].animator.parameters` 是**输入参数**（`[{ "name": "speed", "default": 0.0 }]`），
  过渡用 `conditions: [{ "parameter": "speed", "op": "greater", "value": 0.1 }]` 引用；
  `op` 取 `greater/greater_or_equal/less/less_or_equal/equal/not_equal`，条件与
  `after`/`on_finished` 是**与**关系。过渡还可写 `"on_event": "跳跃"`，由
  `Scene::fire_animator_event` 触发，忽略 `after`/`on_finished`、但 `conditions` 仍要满足；
  事件过渡优先于同状态的时间过渡。参数由宿主写（编辑器面板/`Scene::set_animator_parameter`），
  场景里的 `default` 只是初值；`validate` 会查参数重名、名字为空、条件引用不存在的参数、
  空事件名。
  `lighting.environment_map` 是**等距柱状环境图**：路径相对场景文件解析，支持
  `.hdr`（Radiance `32-bit_rle_rgbe`）/`.png`/`.jpg`；`environment_intensity` 缺省 1，
  `environment_rotation_deg` 缺省 0。装载时生成 64px、7 级 mip 的 cubemap，按粗糙度
  取样；不写就保留解析式天空。`validate` 会检查文件存在与强度非负。边界：只支持
  `-Y height +X width` 朝向；预滤波为锥形平均，不是精确 GGX/PMREM；不会从环境图
  自动生成太阳或局部光。
  `models[].lods` 是**手工 LOD**（只支持静态模型）：`[{ "path": "tree_low.glb",
  "min_distance": 25 }]`；基础模型从距离 0 显示到第一个 `min_distance`，第 j 个替换层
  显示到 `lods[j+1].min_distance`，最后一层到无穷远。距离必须 > 0 且严格递增；
  所有层共用基础模型的位姿/缩放，碰撞仍用基础模型。每帧由 `Scene::update_lods`
  按相机距离切可见层级（10% 滞回）；编辑器的「模型」面板会显示每层距离。
  `models[].ik` 是**两骨 IK**（需要同时有 `animation` 或 `animator`）：数组里每条链写
  `root/mid/tip` 三个 glTF 节点名、`target` 三维目标、可选 `pole` 方向与 `weight`
  权重（0~1，IK/FK 混合）；名字不存在或
  父子关系不成立时整条跳过，几何保持原姿态。`validate` 只查结构（名字非空、三者不同、
  目标/极向量有限、权重在 0~1、必须有动画段）；资产里到底有没有这些节点由报告/实际效果决定。
  `blend` 是**第二条 clip 的交叉淡化**：两条 clip 各自推进自己的时间，再对局部 TRS
  插值（旋转 slerp），最后统一累乘父子链；`weight` 缺省 0.5，0 = 只用第一条，1 = 只用第二条。
  `fade` 缺省 0 = 立即到 `weight`；写成正数时从第一帧的 0 **smoothstep** 升到 `weight`，
  用于"走路→跑步"这类一次性过渡。没有任意权重曲线；状态机见 §1.1 的
  `models[].animator`（按停留时间或 clip 播完过渡）。第二条名字写错时
  **两条都不登记**（保持绑定姿态），不会偷偷只播第一条。`collide: true` 的静态碰撞体**不跟着动**。可用名字用
  `quench3d info 模型.glb` 看。
* `waters` 是**解析浮力环境**，不是流体求解器：`{ "center": [x, z], "size": [x, z], "level": y,
  "density": 1000, "drag": 6, "flow": [0, 0] }`。`size` 是水面范围（**半空间**，没有池底），
  动态刚体按**排开体积**受浮力、按浸没比例受线性阻尼；密度小于水会浮、大于水会沉。
  `flow` 是 XZ 水流速度（m/s，缺省 `[0,0]` = 静水）：浸没刚体被拖向该速度，垂直速度
  仍走原阻尼。睡眠体**不受浮力**（否则浮力会把它一直唤醒——这是性能取舍，水面变了要显式唤醒）。
  `validate` 只查参数；浮力结果看 `simulate` 的 `states[].submerged`（0~1）与顶层
  `waters` 计数，人读输出有"浸没 %"。水面物体只是半透明平面，**没有波浪 / 折射**；
  `flow` 是刚体拖曳，不是 PBF 粒子流场。编辑器的「渲染 → 水域物理」逐块调
  flow/水面/密度/阻尼，也可新增/删除水域；改完立即写回世界与场景底稿，
  空白场景新增的水域在存盘时会从世界补全，NaN 水流会被 `is_valid` 拒绝。
* `fluids` 是 **CPU PBF 粒子流体**（M2 求解器 + M3 双向耦合）：`{ "center": [x, y, z],
  "size": [x, y, z], "spacing": 0.1, "radius": 0.05, "rest_density": 1000,
  "viscosity": 0.05, "surface_tension": 0, "vorticity": 0,
  "velocity": [0,0,0], "drain_y": -2.0,
  "emit_rate": 0, "emit_position": [0,1,0], "emit_radius": 0.1,
  "emit_velocity": [0,1,0], "emit_spread": 0.0, "drag": 6,
  "flow_coupling": 0, "iterations": 4,
  "max_particles": 4000, "foam": false, "contact_friction": 0,
  "particle_collision": false, "bubbles": false,
  "bubble_rate": 20, "max_bubbles": 128, "bubble_radius": 0.03,
  "bubble_buoyancy": 1.8, "bubble_drag": 2.0, "bubble_expansion": 0,
  "bubble_pressure": 0, "bubble_momentum_coupling": 0,
  "surface_mesh": false, "surface_resolution": 32 }`。粒子从长方体里
  按晶格发射，被静态/动态刚体推开，自身用密度约束 + XSPH 粘性求解；粒子撞到动态刚体时
  反推冲量，粒子自由表面还给刚体**浮力 + 阻尼**（双向）。
  `flow_coupling`（1/s，缺省 0）把浸没刚体的 XZ 速度拖向刚体 AABB 内粒子的
  平均速度；周围没有粒子时完全跳过。它是**局部流场速度耦合**，不是逐点无滑移
  边界，也不是严格两相流。
  粒子质量按晶格静止密度校准（不是随便取 1），所以静止时密度比≈1；`simulate` 报告
  `fluids[]` 的 `particles` / `avg_density_ratio` / `max_speed` / `y` 范围。
  `surface_tension`（0~5）把表面粒子往邻居重心拉，减少"摊成薄膜"；`vorticity`
  （0~5）加限幅涡量约束，保留小尺度涡。两者缺省 0 = 完全不执行，旧场景逐位不变。
  `velocity` 给初始水柱一个喷射速度；`drain_y` 是开放排水面，粒子 y 低于它即移除
  （不写 = 不排水），用来做无底水池/瀑布。`emit_rate`（粒子/秒）配
  `emit_position`/`emit_radius`/`emit_velocity`/`emit_spread` 是连续发射器：
  黄金角确定撒点、不用随机数，排水后也能继续补水；缺省 0 逐位不变。
  `foam = true`（缺省 false）从高速粒子甩出**被动泡沫/飞沫**：每颗有寿命、重力/阻尼，
  最多 `max_foam` 颗（缺省 256，上限 4096），渲染成小白球；它不参与 PBF 密度、
  不给刚体反作用力，但会用与流体粒子相同的最近表面投影**挡住地板/墙/刚体**，
  法向速度消掉并轻微回弹、切向摩擦，所以水花不会掉到地下。
  `simulate --json` 的 `fluids[]` 会多一个 `foam` 计数（0 时省略）。
  `particle_collision = true`（缺省 false）在每步 PBF 位置迭代后把距离小于
  粒子直径的邻居沿连线推开，再重新投影到刚体边界，代价是每步一次邻居遍历。
  后续粒子对及边界修正可能重新压近先前粒子对；这项开关不保证全部粒子同时达到直径间距，
  封闭容器强分离目标仍未通过，见[实际失败证据](experiments/sealed_particle_separation.md)。
  `contact_friction`（0~1，缺省 0 = 关闭）给粒子与刚体接触加切向库仑摩擦：
  水会拖着动态刚体走，也会被静态地面拖慢；静置接触用重力法向支持力近似载荷。
  `bubbles = true`（缺省 false）生成**轻质气泡/气相粒子**：反向重力浮升、阻尼、
  与刚体/地面碰撞，并把气泡体积内的液体粒子径向推开（气泡受反作用）；两颗接触的
  气泡按体积守恒合并（`r = (r1³+r2³)^(1/3)`，位置/速度按体积加权），
  合并/膨胀后的大泡按 `√r` 更快上浮；浮出液面会
  破裂并在液面留下一颗被动泡沫（需同时开 `foam`），寿命结束则直接消失。`bubble_rate`（颗/秒，缺省 20）、`max_bubbles`（缺省 128，上限
  4096）、`bubble_radius`（缺省 0.03）、`bubble_buoyancy`（缺省 1.8 倍重力）、
  `bubble_drag`（缺省 2）、`bubble_expansion`（缺省 0 = 半径不变；>0 时按指数
  长大并封顶到初始半径 4 倍，模拟上升减压；`bubble_compressibility > 0` 时按
  `pV≈const` 让深处气泡收缩，到浅处恢复；`bubble_max_radius > 0` 时超过上限的
  气泡按体积守恒破碎成多颗；`bubble_wake > 0` 时上浮气泡把上方液体向上拖成
  尾流；`bubble_pressure > 0` 时按 `p0·(r_rest/r)³` 把过压转成液体径向加速度；
  `bubble_momentum_coupling`（0~2，缺省 0）把液体从排开/过压/尾流获得的总动量
  按 0.5·ρ·V 的附加质量反推给气泡，减少单向注入动量，仍是近似而不是独立气相
  动量方程）。CLI 把气泡画成低模球，`simulate`
  报告 `bubbles` 计数
  （0 时省略）。局部气体体积分数还会折算进刚体浮力：气泡占据的体积不再排开液体，
  浮力密度乘以 `1 - gas_fraction`（上限 0.9），所以曝气的水托不住同样的重物。
  气泡默认也参与 PBF 密度约束（`bubble_density_coupling`，缺省 true）：局部气体
  体积分数把有效静止密度降为 `rho0 * (1 - gas)`，液体在气泡周围膨胀；关闭后退回
  纯径向排开。**仍不是**严格两相流：没有独立气相压力/相变/表面张力，`gas` 是
  核支持体积上的体积分数近似，不是可压缩气体状态方程。
  4000 级粒子（4913 颗）在 release 原生端约 14.4 ms/步，低于 60 Hz 预算；邻域用
  每步重建的稠密网格（与旧哈希邻居顺序一致），密度/lambda 在原生端按区间并行，
  wasm 仍走单线程。这只是 CPU PBF 的性能优化，不改变物理参数或旧场景结果。
  **边界**：表面张力不是曲率/真实 mN/m 模型，涡量是视觉/稳定性约束；泡沫是单相
  视觉白水（不是气泡/两相流，仍不做双向耦合）；
  粒子间碰撞只保证最小分离，不模拟压力/粘性；
  敞开的平地上水仍会摊成薄层（这是"液体在无限平面上的解"，不是穿地）；想看"一池水"请用四堵静态墙围起来（`examples/fluid_pool.json`）。
  四种粒子形状/拖尾/自转/颜色渐变的完整场景见 `examples/particles_showcase.json`：
  `quench3d render examples/particles_showcase.json --out particles.png`。
  `TriMesh` 静态网格对粒子做**逐三角形最近点**碰撞（薄壳不会漏）；凸包用
  GJK/EPA、圆锥用底面圆盘 + 侧面线段投影求精确最近表面（都不再拿 AABB 近似）。
  粒子**渲染**（M4）：每颗粒子画成低模球（8×6），速度越快颜色越偏青白。`surface_mesh =
  true` 时不再重复画这些球，而是用 **Surface Nets** 从粒子标量场提取连续自由表面
  （真实三角形、顶点法线来自场梯度、拓扑每帧可整体替换）；`surface_resolution`
  控制最长轴格数，越大越细也越慢。缺省 false 时保留旧行为：一张跟着最高粒子走的
  半透明平面。
  屏幕空间水（M4.2）：主 pass 的第二颜色附件写水面 mask（`MaterialDesc.water_mask`，
  `Rgba16Float`），r = 覆盖、g = 世界高度；后处理用屏幕射线与 g 求交重建水面世界坐标，据此生成
  程序化波面法线，再做屏幕空间折射、吸收、菲涅耳、太阳高光与波峰/边缘泡沫；
  `waters` / `fluids` 都接，不需要读深度纹理（GL/WebGL2 也能用）。
  顶层 `water` 段可调 `color` / `clarity` / `wave_strength` / `foam` / `refraction`
  （缺省值见 §6.3；只影响画面，不影响物理）。**边界**：吸收厚度固定、折射仍是
  屏幕空间偏移而不是光线追踪、水面几何仍是平面；相机在水下时重建不成立；
  没有焦散/水下体积光/真实波浪。
  `waters` 与 `fluids` 可以同时写，互不替换；两者都给刚体浮力时
  取较大的浸没占比，不会叠加浮力（M3 口径）。
* `particles` 是 **CPU 公告板粒子**（纯视觉，不参与物理）：`{ "count": 64,
  "position": [0, 1, 0], "radius": 0.1, "velocity": [0, 1.2, 0], "spread": 0.4,
  "enabled": true, "lifetime": 2.0, "size": 0.12, "size_end": 0.4, "spread": 0.4,
  "drag": 0.5, "rotation": 0, "rotation_end": 180, "alignment": "velocity",
  "gravity": [0, -1, 0], "shape": "box", "box_size": [2, 0.2, 2],
  "color": [1, 0.65, 0.2, 0.8], "color_end": [0.3, 0.05, 0.02, 0.0],
  "texture": "tex/smoke.png" }`（`size_end`/`shape`/`box_size`/`color_end`/`texture`
  可省略）。
  每颗粒子是一个面向相机的四边形（`MeshData::quad`），按寿命二次淡出，寿命到点回到
  发射器附近重生；`update_particles` 每帧做老化/重力/速度积分/朝向。没有贴图的发射器
  共用一个白色 unlit、alpha 混合、双面材质；写 `texture` 时按贴图 id 各建一份材质，
  颜色仍作为实例色乘上去（适合烟/火/光斑 sprite）。`color_end` 让 RGB/alpha 随寿命
  线性插值到寿命终点，再乘二次淡出；`size_end` 让四边形边长从 `size` 线性缩放到
  寿命终点；`shape` 选 `"sphere"`（默认，半径 `radius`）、`"box"`（全尺寸
  `box_size`）、`"disc"`（XZ 平面圆盘，半径 `radius`）或 `"cone"`（+Y 圆锥，
  半径 `radius`、高 `box_size.y`），决定初始位置分布；`drag`（1/s）让速度按 `exp(-drag·dt)` 衰减；
  `rotation`/`rotation_end`（度）让公告板绕朝向相机的轴自转，寿命内线性插值；
  `alignment` 选 `"billboard"`（默认，面向相机）或 `"velocity"`（局部 +Z 对齐速度）。**边界**：没有软粒子；粒子按实例 AABB 中心到相机的距离参与全局透明排序，
  不是逐像素深度排序；没有 GPU 粒子/曲线编辑；编辑器的属性面板**只读**列出粒子
  发射器，并可直接编辑数量、形状、朝向、颜色、寿命、尺寸、收束、阻尼、自转、
  半径/高度、颜色/终点色和「启用」开关；修改写回 `particles`，保存后保持。不创建/删除发射器。
  属性面板也会汇总底稿 `decals`，位置/法线/尺寸/旋转/颜色可直接改；贴图可从磁盘应用并同步材质。
* `decals` 是**平面贴花**（纯视觉）：`{ "position": [0, 0.01, 0], "normal": [0, 1, 0],
  "size": [2, 1.5], "rotation": 90, "color": [1, 0.2, 0.1, 0.8], "texture": "tex/logo.png" }`。
  四边形按 `normal` 朝向、沿法线抬 1cm 防 z-fight；unlit、alpha 混合、双面，颜色/贴图
  按实例色相乘。**边界**：不是深度投影贴花——曲面/斜坡上会悬空或穿插，只适合地面
  标记/箭头/污渍这类平面；没有贴花裁剪、图集或编辑器创建入口。
* 顶层 `water` 是**屏幕空间水外观**（不是第二个水域）：`{ "color": [r,g,b],
  "clarity": 0~1, "wave_strength": 0~4, "foam": 0~2, "refraction": 0~2 }`。
  所有 `waters` / `fluids` 共用这份风格；只影响后处理画面，浮力/粒子结果逐位不变。
  不写 = 渲染器缺省；非法值在 `validate` 报 `water.*`。编辑器左侧「屏幕空间水」
  折叠面板可直接调这五项，存盘写回同一个 `water` 段。
* `friction_combine` / `restitution_combine` 可写在世界级 `physics` 段（缺省）或单个物体上
  （覆盖缺省）。缺省规则是**几何平均**（摩擦）与**取大**（恢复）——正是老行为；
  改成 `average`/`min`/`max`/`multiply` 会改变已有场景的观感，属于**有意**调整。
  两端都显式写了不同规则时，取 slot 序号靠前的那个（宽相输出按序号有序，所以与线程数无关）。
* `lighting.point_lights.color` / `lighting.fog.color` / `lighting.sky_colors.*` 都是
  **线性空间**数值（跟 `materials.*.color` 的 sRGB 约定不同）——想要暖光就写
  `[1, 0.92, 0.85]` 这种数，写 `[1, 235, 220]` 那种 0~255 的整数会过曝或全黑。
* `lighting.shadow_quality.bias` 是阴影深度偏移（单位：纹素，缺省 0.75，0~4）：
  太小会自阴影麻点，太大会让阴影脱离物体；编辑器里叫“偏移”。
* `lighting.shadow_quality` 与开关 `lighting.shadows` 是两件事：开关管"投不投"，
  这一节管"投得好不好"（`strength` 0~1、`radius` 米）。**阴影贴图分辨率不在场景文件里**。
  `cascades`（1~4，缺省 1）决定分几级：1 = 单级（与老场景逐像素一致，覆盖范围看 `radius`），
  2~4 = 按相机视锥切段、近处细远处粗；每级只分到半张贴图，所以**加级数不如提高分辨率**来得直接。
  `split_lambda`（0~1，缺省 0.5）调"对数 ↔ 均匀"切分。`cascades > 1` 时 `radius` 不再起作用。
  `threshold` 是**线性**亮度（Rec.709），太阳圆盘/自发光可以远大于 1，所以 1.0 通常只让它们外溢；
  bloom 加在 tonemap **之前**，所以亮区外溢不会过曝成白块。
* `lighting.ssao` 同样**不写 = 关闭**（像素与关闭时逐位相同）；写 `{}` 就是打开。
  `radius` 单位是**米**（视空间），物体尺度 1~2 米时给 0.5~1.0；`samples` 缺省 16。
  它是**深度-only** 的（没有法线缓冲，法线由视空间位置的屏幕导数重建）、结果乘在整幅
  HDR 颜色上（不区分直接光/间接光）——这是后处理式 SSAO 的固有简化；
  平坦区域在掠射角下仍可能暗 1~2 个色阶（已知边界，见 §9）。
  `denoise: true` 会做 4 邻域双边去噪（按深度差保护边缘，AO 计算量 ×5，缺省关）；
  实测接触区粗糙度约减半，见 `post_pipeline` 的 SSAO 像素测试。
* `lighting.taa` 同样**不写 = 关闭**；写 `{}` 就是打开。它用上一帧历史 + 当前深度
  重投影累积，相机每帧做亚像素抖动；`feedback` 0~0.98（缺省 0.9），越大越平滑也越容易
  拖影。**没有运动向量**（运动物体靠 3×3 邻域 clamp 抑制拖影），只在能读深度且
  `render.msaa = 1` 的原生后端生效。
* `lighting.fxaa` 同样**不写 = 关闭**（像素与关闭时逐位相同）；写 `{}` 就是打开。
  它工作在 tonemap **之后**的画面上（阈值按 0~1 的**感知**亮度定，不是线性光强），
  找边缘做一次定向模糊：平坦区域逐位不变，斜边/细边变柔和。
  与 `render.msaa` **不冲突也不重复**：MSAA 在几何边缘上以更高采样率着色，FXAA 负责剩下的
  锯齿与着色噪声，代价是细节略微变软。
* 材质的 `color`/`emissive_color` 是 **sRGB**；点光源的 `color` 是**线性**值（引擎内部约定，别照抄材质的数值）。
* `mass` 覆盖 `density`×体积；`linear_damping`/`gravity_scale`/`allow_sleep`/**`angular_velocity`**
  这些只对 `body: "dynamic"` 生效——写在别的刚体上会**警告"会被忽略"**，不会静默失效。
* `parent` 用**父物体的名字**引用（父物体必须显式写 `name`），而且可以排在子物体后面。
  给了 `parent` 之后 `position` / `rotation_deg` 就是**相对父物体**的局部量；
  子物体继承父物体的**位置与朝向，不继承缩放**（`scale` 是"这个物体自己多大"，不是"这一组多大"）；
  **带刚体的物体不能有父物体**（位姿由物理决定，写了会被校验器拦下，不会静默失效）。
* `joints[].anchor_a`/`anchor_b` 是**两端各自的局部坐标**，错开它们才能表达门轴、悬臂、偏心摆；
  `axis` 同样是局部轴（物体一旦 `rotation_deg` 非零，世界轴向会跟着转）——**两端朝向不同时
  必须按端写** `axis_a` / `axis_b`（各自刚体的局部轴；**不写 = 回落 `axis`**，所以老场景逐位不变）。
  例：门框不转、门板 `rotation_deg: [90,0,0]` 时，世界 Y 在门板局部是 Z，于是
  `"axis_a": [0,1,0], "axis_b": [0,0,1]`。两端朝向相差 >10° 却只写 `axis` 会得到
  `suspicious_value` 警告——那种写法的求解器会先把门板**扭平**（现象是"门装上去自己弹回原位"）。
* `joints[].frame_a`/`frame_b`（**只有 `fixed` 关节读**）是两端的**参考姿态**（欧拉角 XYZ，**度**）：求解器把两端扭到「参考姿态在世界中重合」（`q_a⊗frame_a == q_b⊗frame_b`），用来表达「让 B 的某个特征朝向跟 A 的特征朝向对齐」——两端各自还有朝向时，靠初始 `rotation_deg` 手算世界朝向是算不准的。**不写、或两端都写 0 = 老行为**：锁定创建那一刻的相对姿态（逐位不变）；两端都写 0 会得到 `ineffective` 提醒（「等于没写」）；写在非 `fixed` 关节上同样提醒。
* `joints[].radial_clearance`（米，缺省 0 = 理想关节）给 fixed / ball / hinge 的**点约束**
  加径向自由间隙：两锚点距离不超过间隙时完全自由，超过后只挡**径向分离**，旋转/限位/
  马达不受影响。用 `0.0002` 表达 0.2 mm 舵机/轴承回差；求解器用预测式速度上限，
  不会出现"高速一步穿过间隙、下一步才被拉回"的穿透。distance / slider 上没有点约束，
  写了会给 `ineffective` 提醒；旧场景逐位不变。
* **关节的单位有两套，别混**：`limits` 是**弧度**（铰链）/ 米（滑块），`motor_velocity` 是
  **弧度/秒**；而 `motor_target_deg`、`cone_limit.half_angle_deg` 是**度**（名字里带 `_deg`）。
  校验器会对"限位 > 6.5 弧度""伺服目标 > 3600 度"这类明显写错单位的地方报警告。
* `templates` 是"可复用的一段物体描述"（字段集与物体完全相同，包括 `shapes`），
  物体用 `template: "名字"` 引用；**物体自己写的字段覆盖模板**。模板里写
  `name`/`parent`/`position`/`rotation_deg` 没有意义（警告并忽略）。
* `objects[].instances: [ { name?, position?, rotation_deg?, scale? } ]` 一次摆多个副本，
  名字缺省 `本名#1`、`本名#2`…（只有一个副本且没写 `name` 时保持本名，老引用不受影响）。
  **每个副本是一个独立物体/刚体**：不是共享刚体，也不是 GPU 实例化——所以关节/动作要写
  `本名#序号`，副本多了就是真实的物理开销（>64 个给警告）。
  校验会拦：模板不存在、`instances: []`、展开后名字重复、实例里的未知字段。
* `include: ["parts/wall.json", "../shared/common.json"]` 把场景**拆成多个文件**（跨文件复用
  与跨文件 prefab）。相对路径以**写这一行的那个文件**所在目录为基准，候选位置与贴图/模型同源
  （文件目录 → `assets.search_paths` → `--asset-path` → 工作目录 → 引擎搜索路径）。
  片段只能贡献**可合并的段**：`materials`/`templates`/`cameras`（具名表，**重名 = `duplicate_name`
  错误**，不静默覆盖）与 `objects`/`models`/`joints`/`actions`/`probes`/`animations`（数组，
  按声明顺序追加：**本文件自己的内容在前**）。根独有的段（`ground`/`camera`/`lighting`/
  `simulate`/`render`/`gravity`/`physics`/`name`/`assets`）写在片段里是**错误**——
  这些段只有一个实例，谁赢没有答案。
  片段里的贴图/模型相对路径按**片段自己的目录**解析（装载时重定基一次，文件搬到哪都跟着自己走）；
  片段可以再 `include`（最多 16 层）；同一个文件在一次解析里只并入一次（菱形包含给一条
  `ineffective` 警告），成环 = `include_cycle` 错误。**不写这个字段 = 行为与以前逐字节相同**。
  跨文件 prefab 的推荐写法：片段只放 `templates`，根场景用 `objects[].template` + `instances`
  实例化。
* 命名相机：`cameras: { 名字 → 相机对象 }`（缺省空 = 只渲 `camera` 段那台）。校验拦空名与
  保留名 `default`。`--camera <名字>`（`render`/`simulate`/`movie` 都支持，未知名退出码 2、
  `error.kind = unknown_camera`）已可用；多输出（`render.outputs[]`）是下一步。
* 机器可读的 JSON Schema：`quench3d schema --json-schema`（draft 2020-12，可直接喂给
  编辑器补全 / CI 校验 / agent 自检）。**schema 管"结构"（哪些键、嵌套、不许有额外字段），
  `quench3d validate` 管"语义"（范围、互斥、交叉引用）**——schema 过了不等于场景能跑。
  版本策略：当前格式版本 2；版本 1 的文件把 `version` 改成 2 即可（1→2 的唯一差别是
  `objects[].parent` 在 v2 才生效，没有 parent 的文件语义完全一样）。
* `objects[].shape.type = "rounded_box"`：**圆角盒**（`size` 是外接**边长**，`radius` **必填**、
  ≤ 最短边长的一半）。几何上是"盒子 ⊕ 半径 `radius` 的球"：`radius` 越小越像箱子，
  等于最短边长的一半（三边相等时）就是球。物理支持函数是精确的，惯量用确定性网格采样
  （误差 < 1%，见 `docs/production_readiness.md` §17）；渲染网格按尺寸生成（同样按内容哈希缓存），
  所以 `render_scale` 恒为 1。
* `objects[].shape.type = "heightfield"`：**地形高度场**。`heights` 是行主序的二维数组
  （`heights[行][列]`，行沿 Z、列沿 X），`cell_size` 是格子边长（米，缺省 1）；网格在 XZ 平面
  **以物体原点为中心**，高度是**相对原点**的。物理上它是**静态三角网**，所以只能 `body: "static"`
  （写 `dynamic`/`kinematic` 会报错：三角网没有质量属性，硬写会被引擎静默降级）。
  渲染网格由**同一份**高度数据生成 —— "看到的"就是"踩到的"；格点数上限 65536
  （≈13 万个三角形），窄相逐三角形（没有层次结构），地形大就切块。
* `objects[].shape` 与 `objects[].shapes` **二选一**：单形状用 `shape`（老写法不变），
  一个刚体多个形状用 `shapes: [ { shape, position?, rotation_deg? } ]`。
  部件的 `position` / `rotation_deg` 是**相对刚体原点**的局部位姿；两个都写、空数组、
  部件里的未知字段都是 error；半空间当部件、只写一个部件是 warning（能跑但多半写错了）。
* `joints[].cone_limit`（只有 `ball` 有）把两端两根局部轴的夹角限制在 `half_angle_deg` 内：
  管"摆的方向"，**绕轴自转仍然自由**；它是速度级约束，半秒内压回锥面，不是硬夹住。
* `joints[].motor_target_deg` 一给就进**位置伺服**（同时给 `motor_velocity` 会警告"速度被忽略"）；
  伺服不改变最终停在哪的只有 `motor_stiffness`（缺省 8）与 `motor_damping`（缺省 1，越大越柔）。
* `joints[].break_force`（N）/ `break_torque`（N·m）用的是"本步累积冲量 ÷ dt"的**量级估计**，
  不是精确约束力，也没有疲劳/累计损伤；超限就把关节**移除**（`simulate` 报告里 `joints_broken`）。
  阈值写 0/负数 会报错（"不设"的正确写法是**不写这个字段**）。
* **关节两端缺省不互相碰撞**（`collide_connected` 缺省 `false`，与 Bullet / PhysX 一致）：
  铰链、球窝的两端几何上就重合，参与碰撞等于让门和门轴互相挤（"转到一半卡住"）。
  要"连起来还能互相弹开"就显式写 `"collide_connected": true`。
* **被马达驱动的刚体不会自动睡眠**（伺服到位时速度正好是 0，否则会被冻结在"还没到位"的角度）：马达删掉之后恢复正常睡眠判定。
* `camera.orthographic_height` 一给就切正交投影（`fov_deg` 失效），适合工程/制图/2.5D 视角。
* `actions[].at_step` 是"第 N 步**开始前**"（第 1 步是第一次物理步进，0 = 仿真开始前），
  `at_time` 是秒数写法（`round(t/dt)`），两者互斥；`movie` 的预热帧也算在步号里。
* **质量缺省是 density×体积**（边长 1 的箱子 = 1000 kg），所以"6 N·s 的冲量"通常什么也推不动——
  推不动就显式给 `mass`，或把冲量/力给大一点。
* 往静态刚体（含地面、`collide: true` 的模型）上打冲量不会有任何效果，校验会**警告**你。
* `wake` 动作的 `target` 可以写 `"*"`：一步唤醒**所有**睡着的刚体（别的动作写 `"*"` 会报错）——
  搬动过一批物体、或从快照恢复之后，用它保证没有东西以睡着的姿态停在半空。
  `allow_sleep: false` 的刚体是"永远清醒"（手动 `sleep()` 也睡不住），适合等外力/等关节推动的物体。
* 动作写在已睡眠的刚体上也有效（引擎内部会先唤醒）；`probes[]` 是**只读**的——
  同一场景多挂一个探针，物理结果逐位不变（有回归测试钉着）。
* 探针判定用刚体的**世界包围盒**与区域求交；地面是半空间、没有有限包围盒，
  所以探针永远监测不到地面本身。
* 上面例子里的 `checker.png` / `chair.glb` / 关节两端的 `门`、`框` 都是**占位引用**——
  直接跑会报"找不到文件/物体"，把引用换成你自己的资源就能跑（校验器会把缺什么、去哪儿找都列出来）。
  JSON 里**不能写注释**（上面的注释都在正文里）。

### 6.4 输出契约（agent 依赖这个）

成功与失败都是**一行** JSON：

```json
{"ok":true,"command":"validate","version":"0.1.0","elapsed_ms":0,
 "data":{"valid":true,"errors":0,"warnings":0,"diagnostics":[]}}
```

```json
{"ok":false,"command":"validate","version":"0.1.0","elapsed_ms":0,
 "error":{"kind":"invalid","message":"场景不合法：2 个错误、0 条警告","diagnostics":[
   {"severity":"error","code":"invalid_value","path":"objects[0].shape.type","message":"unknown variant `pyramid`, expected one of `box`, `sphere`, …"},
   {"severity":"error","code":"invalid_value","path":"objects[0].body","message":"unknown variant `flying`, expected one of `none`, `static`, …"}]}}
```

| 退出码 | 含义 | agent 应当怎么做 |
| --- | --- | --- |
| 0 | 成功 | 读 `data` |
| 2 | 输入不合法（场景/参数） | 按 `error.diagnostics[].code` 决定动作、按 `[].path` 定位，改输入后重试 |
| 3 | 环境不可用（无 GPU / 后端缺失） | 改用 `simulate`，或换机器 |
| 4 | 网络或大模型服务问题 | 检查服务是否启动、地址与模型名 |
| 1 | 其它运行期错误 | 记录并上报 |

诊断是**一次全给**：上面那份输出里同一个物体的两个字段错误同时返回，
每条都有精确到字段的路径——这让 agent 可以一轮改完，而不是反复试错。

每条诊断还带一个**稳定的机器可读 `code`**（`diagnostics[].code`）：`code` 说**是什么性质**
（该做什么），`path` 说**在哪里**（改哪里），`message` 只给人读。所以 agent 按 `code` 分支，
不用正则匹配中文文案——文案改了分支不会烂。典型分支：

| `code` | 意思 | 该怎么办 |
| --- | --- | --- |
| `unknown_field` / `missing_field` / `invalid_type` | 字段名、缺字段、JSON 类型 | 按 hint 里的合法字段表改 |
| `invalid_value` / `out_of_range` | 枚举拼错 / 数字越界或非有限 | 换成允许的取值 |
| `unknown_reference` | 引用了不存在的物体/材质/模板/相机 | 改名或补定义（hint 带可用名单） |
| `asset_not_found` | 贴图/模型文件找不到 | 放到消息列出的候选位置，或加 `assets.search_paths` |
| `duplicate_name` / `reserved_name` / `cycle` / `include_cycle` | 重名 / 保留名 / 父子成环 / `include` 成环 | 改名 / 换名 / 打断环（含文件包含环） |
| `mutually_exclusive` | 两个字段只能留一个 | 删一个 |
| `ineffective` / `suspicious_value` | 写了不生效 / 合法但很可能不是本意 | 删掉或复核数值 |
| `unit_ambiguity` | 单位容易混（弧度 vs 度、米 vs 度） | 确认单位 |

**完整码表**（18 个，含每个码的"agent 该做什么"）见 [docs/error_codes.md](error_codes.md)，
也可以让 agent 直接读 `quench3d schema --json` 的 `error_codes` 段（同一份表，由测试对账）。
码是**契约**：发布后不改名、不改含义，只会新增。

### 6.5 接大模型：自然语言 → 场景

`ask` 走 OpenAI 兼容的 `/v1/chat/completions`，因此本地推理服务开箱即用：

| 服务 | 默认地址 | 启动 |
| --- | --- | --- |
| Ollama | `http://localhost:11434/v1` | `ollama serve` + `ollama pull qwen2.5:7b` |
| llama.cpp | `http://localhost:8080/v1` | `llama-server -m model.gguf` |
| LM Studio | `http://localhost:1234/v1` | 打开 GUI 的 Local Server |
| vLLM | `http://localhost:8000/v1` | `vllm serve <model>` |
| 云端 | 任意兼容端点 | 设 `QUENCH3D_LLM_API_KEY` |

```bash
# 环境变量（也会回退读 OPENAI_BASE_URL / OPENAI_MODEL / OPENAI_API_KEY / OLLAMA_HOST）
#   API key 不会出现在进程的命令行参数里：https 路径把 url 与 Authorization 头写进一个
#   0600 的临时配置文件、用 `curl -K <文件>` 读入（请求体仍走 stdin，提示词不落盘），
#   请求结束立刻删除。命令行参数对同机其他用户可读（/proc/<pid>/cmdline、ps、审计日志），
#   所以密钥绝不能放在那里——测试里用"假 curl"把 argv 抄下来断言搜不到 key。
export QUENCH3D_LLM_BASE_URL=http://localhost:11434/v1
export QUENCH3D_LLM_MODEL=qwen2.5:7b

quench3d ask "做一个三层箱子塔然后倒下的场景，相机从右前方看" --json
quench3d ask "斜坡上滚球，斜坡 12 度" --out slope.json --render slope.png --simulate
```

`ask` 的闭环是：**模型输出 → 抠出 JSON（容忍 Markdown 代码块与前后解释）→ 按场景语法校验 →
诊断回喂给模型让它自己改 → 落盘 / 仿真 / 渲染**。

**自动修复**默认开着（`--repair-rounds`，缺省 2）：第一版场景不合法时，会把带 JSON 路径的
诊断连同模型自己上一次的输出一起发回去，要求「只返回修正后的完整 JSON」。
本地小模型在这条闭环上的成功率提升很明显，返回里能直接看到过程：

```json
{"valid":true,"repair_rounds":1,
 "attempts":[{"round":1,"repair":false,"valid":false,"errors":1,
              "messages":["objects[0].shape.type: unknown variant `pyramid` …"]},
             {"round":2,"repair":true,"valid":true,"errors":0,"messages":[]}]}
```

修不好时退出码为 2，`data.diagnostics` 就是给上层 agent 的修正清单（`--repair-rounds 0` 可关掉自动修复）。

`http://` 端点用内置的极简 HTTP 客户端（零 TLS 依赖）；`https://` 端点交给系统 `curl`
（Windows 10+ / macOS / 主流 Linux 都自带）。设了 `timeout` 与 `max_tokens` 环境变量可调。

**手边没有模型也能试**：仓库自带一个脚本化的假 OpenAI 服务（`tools/mock_llm_server.py`），
它按固定规则回一个工具调用或一段场景 JSON，用来验证协议与校验链路：

```bash
python3 tools/mock_llm_server.py --port 8123 &
quench3d ask  "斜坡上滚球" --base-url http://127.0.0.1:8123/v1 --model mock --out s.json --simulate
quench3d chat "球停在哪"   --base-url http://127.0.0.1:8123/v1 --model mock
```

### 6.6 命令行 agent：让模型自己调用引擎

`ask` 是「一次生成」，`chat` 是**多轮工具循环**：模型自己决定校验 / 仿真 / 渲染，
引擎把真实结果回喂给它，直到它给出结论。

```bash
quench3d chat "三层箱子塔，五秒后还在原地吗" --json
quench3d chat                     # 交互模式：:help / :tools / :reset / :quit
```

一次真实往返（假模型 + 真引擎，见 `tests/agent_tools.rs`）：

```text
用户：这个球最后停在多高？
  [工具] quench3d_simulate → 步进 180 步（dt=0.01667s）：2 个刚体（动态 1）、1 个接触
  球最终停在地面上，中心高度约 0.5 米。
```

* 工具与 MCP **同源**（`tools/list` 与 chat 的 `tools` 参数由同一份定义生成），不会漂移；
* 工具失败（比如场景写错）会把**带路径的诊断**回喂给模型，让它自己改；
* `--max-rounds`（缺省 6）限制往返次数，到顶就如实告知，不会死循环；
* 模型不支持函数调用时，`ask` 的纯文本 JSON 路径仍然可用。

### 6.7 MCP：把引擎挂到 agent 上

`quench3d mcp` 是一个 stdio 的 MCP（Model Context Protocol）服务器，实现了
`initialize` / `tools/list` / `tools/call` / `ping`。把它写进支持 MCP 的客户端配置即可：

```json
{
  "mcpServers": {
    "quench3d": { "command": "quench3d", "args": ["mcp"] }
  }
}
```

暴露的工具：`quench3d_schema`、`quench3d_doctor`、`quench3d_info`、`quench3d_validate`、
`quench3d_render`、`quench3d_movie`、`quench3d_simulate`、`quench3d_joint_target`、`quench3d_query_contact`、`quench3d_query`、`quench3d_ask`。`quench3d_joint_target` 把某个关节目标改成
`target_deg`（铰链）/`target_m`（滑块）后步进并返回 `joints[]` 遥测；每次调用独立，
同关节的场景 `joint_target` 动作会被移除，避免脚本把目标改回去。

四个对 agent 特别有用的细节：

* 场景参数既可以是**文件路径**，也可以是**内联 JSON 对象**——agent 现生成的场景不必先落盘；
* `quench3d_render` 的返回里除了文本报告，还有一个 `image` 内容块（base64 PNG），
  多模态 agent 可以直接"看"到自己渲染出来的画面；
* 每个工具都带 MCP 的 **`annotations`**（机器可读的"能不能并行/重试/出网"）与
  **`outputSchema`**（`structuredContent` 的字段），agent 不必读描述去猜：

  | 工具 | readOnly | destructive | idempotent | openWorld |
  |---|---|---|---|---|
  | schema / doctor / info / validate / simulate | ✅ | — | ✅ | — |
  | render / movie | — | ✅（可能覆盖文件） | ✅ | — |
  | ask | — | ✅ | —（每次可能不同） | ✅（出网） |

  单测把这些声明与**事实**对齐：只读工具不许同时标"会破坏数据"，出网工具必须不幂等；
* `quench3d_render` / `quench3d_movie` 还能传 `dry_run: true`（只跑物理与校验，不渲染不写盘）
  与 `no_clobber: true`（输出已存在就报错，多 agent 并行时不互相覆盖）。

一次完整的握手与调用（`initialize` → `tools/list` → `tools/call`）有集成测试覆盖，
跑的是真实的 stdio 子进程。

### 6.8 跨平台

* 三平台 CI 矩阵跑格式 + clippy(`-D warnings`) + 全部测试 + CLI 冒烟；
  另有 `wasm32-unknown-unknown` 编译检查。见 `.github/workflows/ci.yml`。
* CI 机器上没有 GPU，也不安装任何软件光栅驱动：渲染路径如实走「无 GPU」分支
  （`render` 返回退出码 3，CLI 冒烟接受 0 或 3），而 `simulate` / `bench` 照常工作。
* 命令行工具自身不依赖任何平台专属 API（路径走 `PathBuf`，网络走 `std::net`），
  Windows 用 `tools\install_cli.ps1` 安装。
* Web 产物可以**在没有浏览器的机器上验证**：

  ```bash
  tools/build_web.sh physics_playground   # 编译 wasm + wasm-bindgen 打包到 web/pkg/
  node tools/check_web.mjs                # 检查页面 import 的符号、wasm 能否实例化
  node tools/check_browser.mjs            # 可选：headless Chrome 里跑 1000 步 + Canvas 预览基准
  python3 -m http.server 8000 --directory web   # 然后浏览器打开 http://localhost:8000/
  ```

  `check_web.mjs` 会读 `web/index.html` 的 import、核对胶水代码导出的符号、
  真正实例化一次 wasm 并确认 Rust 导出与内存都在——它能挡住「打包漏文件 /
  页面 import 了不存在的函数 / wasm 损坏」。CI 的 wasm 任务就是这么跑的
  （浏览器内的 wgpu 渲染帧率仍需带 GPU 的真实浏览器；`check_browser.mjs` 只测
  wasm 物理与 Canvas 2D 预览，没装 Chrome 会自动跳过）。

