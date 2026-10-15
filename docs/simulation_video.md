# 仿真视频：可行性、方案与行业对标

> 结论：**能做，而且这是本引擎最适合做的功能之一**（确定性物理 + 离屏渲染已经打通）。
> **2026-10-15 已落地**：`Engine::render_sequence` + CLI `quench3d movie`，端到端实测
> 300 帧 → 5.000000 秒 MP4（`nb_frames=300`），同场景两次录制逐像素一致。
> 本文保留决策过程、实测数字与行业对标。

---

## 0. 已落地的东西（先看这段）

| 交付 | 位置 | 说明 |
| --- | --- | --- |
| 无头逐帧推进 API | `render_sequence` / `render_sequence_with_game`（`crates/quench3d/src/app.rs`） | 一次 GPU 上下文 + 固定步长循环；可选预热帧；每帧回调拿到像素与统计 |
| `SequenceParams` | 同上 | `frames` / `steps_per_frame` / `dt` / `warmup_frames`，附 `duration()` / `fps()` / `total_steps()` |
| CLI 录制命令 | `quench3d movie <场景>`（`crates/quench3d-cli/src/runner.rs::run_movie`） | 输出 `frame_00000.png…` + `trace.jsonl`，返回建议的 `ffmpeg` 命令 |
| 内置编码器 | `--encoder gif|apng`（+ `--out`） | 纯 Rust、免外部工具，直接产出可播放单文件；换编码器**不改变像素摘要** |
| 参数 | `--frames` `--steps-per-frame` `--frames-dir` `--fps` `--warmup` `--encoder` `--out` | 数值全部校验；`--steps`/`--trace` 等 simulate 专用参数会**直接报错**而不是被忽略；`--out` 与 `--frames-dir` 的意图冲突也会报错 |
| 可复现断言 | 报告里的 `digest`（`fnv1a64:…`，覆盖帧号 + 像素） | 测试钉住「同场景两次录制摘要一致」 |
| 断点续录 | 已存在的帧跳过，报告里报 `skipped` | 长视频中断后重跑即可接上 |

实测证据（release、llvmpipe 软件光栅、4 核）。
下面这组数字是**当时**（HDR 后处理改造前）用某个未随仓库发布的示例场景测的：耗时与 `digest`
会随渲染改动而变（后处理那一轮就变过一次），**唯一稳定的断言是"同一场景两次录制摘要一致"**
（端到端测试已钉住这条），所以把这里当作格式与量级参考，而不是可复现的基线。

```text
quench3d movie scene.json --frames 300 --frames-dir clip640 --width 640 --height 400
  帧目录: clip640（300 帧，640×400，59036 KB）  耗时: 10504 ms
  digest: fnv1a64:7db6d552c4c0a9a6
ffmpeg -framerate 60 -i clip640/frame_%05d.png -c:v libx264 -preset slow -crf 18 \
       -pix_fmt yuv420p -movflags +faststart movie.mp4      # 编码 1.28 s
ffprobe: codec=h264 640×400 pix_fmt=yuv420p nb_frames=300 duration=5.000000 size=125148
```

注意 `duration` 与仿真时间**严格相等**（5 s 仿真 → 5.000000 s 视频），这正是「帧数由物理步
决定」的直接结果；`digest` 两次运行相同，是「可复现」的机器可验证形式。

---

## 1. 现状盘点（要改什么、不用改什么）

| 需要的能力 | 现状 | 证据 |
| --- | --- | --- |
| 离屏渲染单帧 | ✅ 已有 | `render_offscreen` / `render_offscreen_stats`（`crates/quench3d/src/app.rs:985-1022`）；CLI `run_render`（`cli/src/runner.rs`） |
| PNG 落盘 | ✅ 已有（走 `image` crate） | `Texture::write_png`（`render/src/texture.rs:46-52`）、`write_png`（`facade/app.rs:925`） |
| 确定性物理（同场景两次跑逐位相同） | ✅ 已有，有黄金摘要测试 | `physics/src/digest_tests.rs:158-190`；并行==串行逐位一致（`api_tests.rs:418-609`） |
| 逐帧状态记录 | ✅ 已有（`trace.jsonl`） | `run_movie` 每帧写一行：帧号、累计步数、仿真时间、绘制统计 |
| 无头"推进 N 帧"（跑 update/fixed_update/render 循环） | ✅ **已补** | `render_sequence_with_game`（`app.rs`）：帧体顺序与窗口循环一致，固定步长 |
| 复用一个 GPU 上下文渲染多帧 | ✅ **已补** | `render_sequence` 只建一次 `GpuContext::headless`，内部循环（原来逐帧调用会每次重建） |
| 视频编码 | 🟢 GIF/APNG 已内置（纯 Rust）；MP4/H.264 仍交给 ffmpeg | MP4/H.264 内嵌涉及 GPL + 专利池，不做是有意的，见 §3.2 许可 |
| 时间缩放 / 慢动作 | ✅ 通过 `--fps` 实现（只改播放速度，不改仿真） | 引擎级 `time_scale` 仍未做 |
| 音轨 | ❌ 无音频系统 | 引擎/平台层审计 |

**依赖边界澄清**：项目"零第三方依赖"只约束 `quench3d-math` 与 `quench3d-physics`；
门面已经在用 `wgpu`/`winit`/`gltf`/`image`（`render/src/texture.rs:47-52` 用 `image::codecs::png`）。
所以**加编码器依赖不违反任何项目原则**，放 CLI 或 facade 即可。

---

## 2. 实测速度（决定"能录多长、多久"）

**单帧成本**（release、llvmpipe 软件光栅、`quench3d movie` 实测）：

| 分辨率 | 像素 | 实测单帧 | 600 帧（10 秒 @60fps） |
| --- | --- | --- | --- |
| 640×400 | 256,000 | **31 ms** | ≈ 19 s |
| 1280×720 | 921,600 | ≈ 95 ms（拟合） | ≈ 57 s |
| 1920×1080 | 2,073,600 | **205 ms** | ≈ 123 s |

线性拟合 `每帧 ≈ 6.5 ms + 9.6×10⁻⁸ s/像素`（640×400 与 1920×1080 两点实测标定）。
**此前的估算（720p 72 ms / 1080p 162 ms）偏乐观约 25%，已按实测修正。**

另有一次性开销约 **1.1 s**（进程启动 + GPU 设备初始化 + 场景构建）——只在开录时付一次，
这正是必须"一个上下文跑完整个序列"的原因；也有编码开销：上面 300 帧 640×400 的
clip 用 `-preset slow` 编码耗时 **1.28 s**（约 0.26× 实时）。

这是在 **llvmpipe（CPU 软件光栅）** 上测的；换成真实 GPU 会快一个数量级。
再加物理步进（小场景 <1 ms/步，2000 体深堆约 32 ms/步）。

> 结论：软件渲染下录 10 秒 1080p 约 2 分钟、720p 约 1 分钟，完全可用；大场景按物理耗时另算。

---

## 3. 方案（三条路线，按推荐顺序）

### 3.0 地基：`render_sequence`（已实现）

```rust
/// 无头逐帧推进并渲染（不需要窗口），全程共用一个 GPU 上下文。
pub fn render_sequence(
    width: u32,
    height: u32,
    config: &EngineConfig,
    params: SequenceParams,   // frames / steps_per_frame / dt / warmup_frames
    setup: impl FnOnce(&mut Engine),
    on_frame: impl FnMut(&mut Engine, SequenceFrame, &ImageData) -> Result<(), String>,
) -> Result<Vec<SequenceFrame>, String>;

/// 同样，但驱动一个真实的 Game（帧体顺序与窗口循环一致）。
pub fn render_sequence_with_game<G: Game>(..., game: &mut G, ...) -> Result<Vec<SequenceFrame>, String>;
```

设计要点（每条都对应一个已知的坑）：

1. **一次上下文，循环渲染**——否则每帧付 1.1 s 设备初始化（见 3.1）。
2. **帧数由 `steps_per_frame × frames` 决定，绝不用墙钟**——保证视频时长 = 物理时间，且可复现。
3. **`warmup_frames`**——丢弃预热帧（着色器编译、物理沉降、光栅器首次绘制），避免视频第一帧脏。
4. **每帧返回 `SequenceFrame`**（帧号、累计步数、仿真时间、渲染统计）→ 写 `trace.jsonl`。
5. **`on_frame` 回调里判断"文件已存在则跳过"**——长视频崩溃可续。
6. **`render_sequence_with_game` 同时补掉 CI 的盲区**：在此之前帧循环在 CI 里从未被执行
   （`Engine::frame` 私有、`render_offscreen_stats` 不跑 update/fixed_update），
   现在可以"推进 N 帧 + 图像摘要"做渲染回归。
   注意世界步进沿用窗口循环的约定——由 `Game::fixed_update` 调 `world.step`；
   `render_sequence` 用的内部空实现会自动步进。

### 3.1 坑：`render_offscreen` 不能逐帧调用（已规避）

`render_offscreen` 每次调用都会新建 headless GPU 设备（`app.rs:985-995`）。
按 §2 实测的一次性开销 1.1 s 算，**600 帧逐帧调用 = 约 11 分钟纯初始化**（真正渲染只占几十秒）。
`render_sequence` 因此把循环放在内部：建一次设备，之后每帧只做提交 + 读回。

### 路线 A：帧序列 + 外部 ffmpeg（**已实现**，命令是 `movie`）

```bash
quench3d movie scene.json --frames 300 --frames-dir frames --steps-per-frame 1

ffmpeg -framerate 60 -i frames/frame_%05d.png \
       -c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p -movflags +faststart movie.mp4
```

- **为什么选这条**：零新 Rust 依赖（`image` 已在）；崩溃可续（已存在的帧跳过）；可由 CI 断言
  （帧数、像素摘要、逐字节可复现）；管线性工作流可以换 EXR/16-bit；编码参数交给专业工具。
- **这也是行业主流**：Blender 默认输出图像序列、UE Movie Render Queue、Godot movie maker 都支持序列输出。
- 代价：用户要装 ffmpeg（但只是"想要 MP4 时"才装；帧序列本身已经可用）。

### 路线 B：内置编码器（免外部依赖，第二步）

| 选项 | 许可 | 体积/质量 | 评价 |
| --- | --- | --- | --- |
| `gif`（纯 Rust） | 免专利 | 大、256 色 | ✅ **已内置**：`--encoder gif`，逐帧局部调色板（NeuQuant） |
| APNG（`png` crate） | 免专利 | 无损、较大 | ✅ **已内置**：`--encoder apng`（首帧 IDAT + 其余 fdAT） |
| VP9/AV1（`rav1e` 纯 Rust AV1；VP9 需 libvpx） | **免专利** | 好，rav1e 慢 | 若不装 ffmpeg 又要 WebM/MP4，选它 |
| H.264（libx264） | **GPL + AVC 专利池** | 兼容性最好 | 内嵌需谨慎评估许可；建议继续交给 ffmpeg |
| HEVC | 专利更复杂 | — | 不建议内嵌 |

**建议**：`--encoder png|gif|apng`（全部纯 Rust、免专利）内置；**MP4/H.264 交给 ffmpeg**。

### 3.2a 内置编码器的口径与限制（已实现）

```bash
quench3d movie scene.json --frames 60 --encoder gif  --out demo.gif    # 256 色，逐帧局部调色板
quench3d movie scene.json --frames 60 --encoder apng --out demo.png    # 无损
```

* **流式写入**：GIF/APNG 都是边渲染边写，不把 600 帧攒在内存里（PNG 路径的断点续录对单文件编码器
  不适用，重跑会覆盖）。
* **摘要不变**：编码器只换封装，`digest`（覆盖帧号 + 像素）与 PNG 序列**完全一致**，测试直接钉住这点。
* **GIF 的两个硬限制**（都是格式本身，不是实现偷懒）：
  * 帧延迟单位只有 **10 ms**：`--fps 60` 会被量化成 50 fps（20 ms/帧），时长随之变长。
    报告里的 `notes` 会明确算给你看（例：`0.40 s → 0.48 s`），不让你对着变长的视频纳闷。
  * 单帧 256 色：渐变会有色带，适合几十帧的演示循环；要长视频/高压缩率仍走 PNG + ffmpeg。
* **实测**（320×200，24 帧，llvmpipe 软件光栅）：GIF 809 KB / APNG 1504 KB，两者都比 PNG 序列小；
  ffmpeg 独立解码均得到 24 帧，APNG 的 `acTL` 为 `num_frames=24, num_plays=0`（无限循环）。
* 许可：`gif`（MIT/Apache）与 `png`（MIT/Apache）都免专利，可安全内置；H.264/HEVC 仍然不做。

### 路线 C：Web 端录制（浏览器内，wasm）

- `canvas.captureStream(fps)` + `MediaRecorder`（输出 WebM）——**浏览器内置编码器，零依赖**。
- 要精细控制用 **WebCodecs `VideoEncoder`**（码率/关键帧间隔/AV1），自行 mux（WebM/MP4）。
- 注意：wasm 单线程 + WebGL2/软件光栅下渲染慢 → 录制耗时 ≠ 视频时长；
  要么"逐帧渲染 + 慢速录制"，要么走路线 A 的帧序列再打包下载。
- 前置条件与路线 A 相同：需要那个"无头/无窗推进 N 帧"的 API —— **已具备**
  （`render_sequence`/`render_sequence_with_game`，wasm 下同样能编译）。

---

## 4. 行业对标（值得抄什么）

| 引擎 / 工具 | 机制 | 该抄的点 |
| --- | --- | --- |
| **Blender** | 默认输出 PNG/EXR **图像序列**，FFmpeg 是可选输出；`-a` 批量渲染 | 序列优先、编码独立、可续算 |
| **Unreal Movie Render Queue** | 固定步长逐帧；**temporal sampling**（每帧多次子采样累积）；warm-up 帧；tile 渲染超大分辨率；输出 EXR 序列/ProRes | 累积采样 = 免费的运动模糊与抗锯齿；预热帧；分块出 8K |
| **Unity Recorder** | 固定 FPS、可选直接编码或图像序列、命令行/CI 触发 | CI 触发录制、格式可选 |
| **Godot** | `--write-movie out.avi`（movie maker 模式，固定 fps） | 一个开关就录；**帧数与物理步严格对应**，不受墙钟影响 |
| **three.js** | `canvas.captureStream()` + `MediaRecorder` / CCapture.js；CI 里 headless Chrome + ffmpeg | 浏览器内置录制；无头 CI 出视频 |
| **MuJoCo / PyBullet / Isaac Sim**（物理仿真界） | `Renderer` + imageio/ffmpeg 逐帧写盘；附每帧状态 CSV | 仿真视频惯例：**步数=帧数**、逐帧落盘、**元数据 sidecar**、可复现 |

**共同的最佳实践（建议全部采纳）**：

1. **帧数由物理步数决定，不由墙钟决定**：`frames = ceil(total_steps / steps_per_frame)`。
   这样"视频时长 = 物理时间"，且同一场景两次录制**逐像素一致**（可以用测试断言）。
2. **warm-up 帧**：丢弃前 N 帧，避免着色器编译/物理沉降污染开头。
3. **每帧元数据 sidecar**：`trace.jsonl`（步号、时间、物体位姿/速度、渲染统计）。
   这是"仿真视频"区别于"屏幕录像"的关键——视频每一帧都能被程序核对，agent 也能消费。
4. **崩溃可续**：已存在的帧跳过（长视频/大分辨率必踩）。
5. **正确编码参数**：`-pix_fmt yuv420p`（不写这个很多播放器/浏览器不认）、`+faststart`（流式播放）、
   `-crf 18`（质量/体积平衡）、`-framerate` 必须等于逻辑 fps。
6. **编码许可**：H.264/HEVC 有专利池；AV1/VP9/APNG/GIF 免专利 → **内嵌默认用免专利的**。
7. **可选进阶**：累积采样（运动模糊）、时间缩放做慢动作（需先有 `time_scale`）、
   分块渲染超大分辨率、HDR/EXR 输出（浮点渲染目标已经有了：`rgba16float` 中间目标，
   只差把 readback 换成 EXR/PFM 编码）。

---

## 5. 落地顺序与进度

| # | 事项 | 状态 | 备注 |
| --- | --- | --- | --- |
| 1 | `render_sequence` 无头推进 API（复用上下文、固定步长、warm-up、每帧回调） | ✅ 已做 | **同时补掉 CI 缺的"帧循环从未被执行"盲区** |
| 2 | CLI `--frames/--fps/--frames-dir/--steps-per-frame/--warmup` → PNG 序列 + `trace.jsonl` | ✅ 已做 | `quench3d movie`；崩溃可续；像素摘要可断言 |
| 3 | 文档：一条 `ffmpeg` 命令 + `yuv420p/faststart` 说明 | ✅ 已做 | README + 本文；建议命令由 `data.ffmpeg` 直接给出 |
| 4 | `--encoder gif|apng`（纯 Rust，免专利） | ✅ 已做 | `gif` 0.13 + `png` 0.18；流式写入（不缓存全片）；见 §3.2a |
| 5 | Web 端 `MediaRecorder`/WebCodecs 录制 + 页面按钮 | ⬜ 待做 | wasm 单线程要注意耗时；API 已具备 |
| 6 | 可选：内置 MP4（rav1e/AV1 + `mp4` mux）或 openh264 | ⬜ 待做 | 注意许可（见 3.2） |
| 7 | 可选：累积采样（运动模糊）、分块渲染超大分辨率 | ⬜ 待做 | 对标 UE Movie Render Queue |

**验证方式**：`crates/quench3d-cli/tests/cli_e2e.rs::movie_records_deterministic_frame_sequence`
端到端跑二进制，断言帧数 / 时长口径 / PNG 头 / `trace.jsonl` 单调 / **两次录制摘要一致** /
断点续录 / 参数校验退出码；facade 侧另有用例钉住 `SequenceParams` 的时长与帧率口径。
