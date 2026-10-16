# Quench3D

**纯 Rust 的 3D 引擎，自带从零实现的物理引擎，桌面端 + Web 端同一套代码。**

<details>
<summary>关于名字：为什么叫 Quench3D（而不是 forge3d / free3d）</summary>

这个项目最初叫 `forge3d`，但这个名字在 Rust 与 3D 生态里已经有主了，于是改名
**Quench3D**（淬火——锻造之后的下一道工序，也贴「刚性求解」的物理味）。

2026-10 实测核实过的情况：

| 名字 | 状态 |
| --- | --- |
| `crates.io/forge3d` | **已占用**：egui 上的 Blender 风格 UI 库（0.1.0 / 0.2.0） |
| `pypi.org/project/forge3d` | **已占用且活跃**：一个 Rust + WebGPU 的地形/制图渲染器（1.4x），与本项目同领域 |
| `forge3d.com` | 在运营的 Unity 素材出版方（同领域商业主体），另有 `.dev` |
| `free3d.com` | 也已存在（活跃站点，反爬保护） |
| `quench3d` | crates.io / PyPI / npm 三个注册表、`quench3d.com`、`quench3d.dev`、GitHub 同名仓库**全空** |

教训：`3d` 后缀的短名字在 3D 领域几乎被抢完了；「免费/开源」这类信息放在 README 和
许可证里，不要占用名字——名字要负责「这是什么」和「能不能被搜到」。

</details>

Quench3D 面向「3D 模型展示 / 空间与场景搭建 / 物理仿真 / 小游戏原型」四类活动。
不依赖任何第三方物理库（`quench3d-physics` 只依赖自家数学库），渲染基于 `wgpu`
（Vulkan / Metal / DX12 / WebGPU / WebGL2 同一份着色器）。

```text
quench3d/
├── crates/
│   ├── quench3d-math/     零依赖数学库：Vec2/3/4、Mat3/4、Quat、Transform、Aabb、Ray、Frustum
│   ├── quench3d-physics/  从零实现的刚体物理：形状、宽相/窄相、顺序冲量求解器、5 种关节、角色控制器
│   │                     热路径零分配：内联小向量 SmallVec、跨步复用的宽相网格与求解缓冲
│   │                     基准：cargo run --release -p quench3d-physics --example bench
│   ├── quench3d-render/   wgpu 前向 PBR 渲染器：阴影、天空、网格、实例化、调试线段、离屏截图
│   ├── quench3d/          门面层：Engine / Game / App、场景与资源（glTF/OBJ）、输入、相机、Web 入口
│   ├── quench3d-editor/   编辑器：egui 界面（工具栏 / 场景树 / 属性面板 / 状态栏）+ 引擎接线；
│   │                     桌面端与 Web 端**同一套界面代码**（Web 端就是仓库的 web/index.html）
│   └── quench3d-cli/      命令行工具 quench3d：渲染、纯物理仿真、模型检查、性能基准、大模型生成场景、MCP 服务器
├── tools/
│   ├── build_web.sh      wasm 打包脚本（cargo build + wasm-bindgen）
│   ├── install_cli.sh    安装命令行工具（Linux / macOS）
│   ├── install_cli.ps1   安装命令行工具（Windows）
│   ├── ascii_preview.py  把渲染结果转成 ASCII / 数值检查（无浏览器环境下的视觉验证）
│   ├── ab_bench.sh       配对 A/B 基准（同会话多次取最小，避开机器噪声）
│   ├── mock_llm_server.py 假 OpenAI 服务：没有本地模型也能试 ask / chat
│   └── check_web.mjs     检查 Web 产物：页面 import 的符号、wasm 能否实例化
├── docs/
│   └── island_parallel_plan.md  island 并行求解设计稿（三步均已落地，见 §8.6 / §8.7）
├── .github/workflows/ci.yml  三平台 + wasm 的 CI 矩阵
└── web/index.html        Web 客户端页面外壳（后端探测、加载遮罩、场景文件上传/下载）
```

约 3.7 万行 Rust（`src/`，其中 CLI 约 1.1 万、编辑器约 0.24 万）+ 0.65 千行 WGSL，
另有约 0.8 万行示例与测试。
引擎本体全部 `#![forbid(unsafe_code)]`（只有两个**基准/测试工具**为了统计堆分配用了
`unsafe impl GlobalAlloc`，见第 8 节）。

---

## 1. 快速开始（桌面端）

需要 Rust 1.90+（根 `Cargo.toml` 的 `rust-version`：由依赖图自己声明的 MSRV 推出，
下面几个依赖是决定项——`wgpu` 1.87、`image` 1.88、`ordered-float` 1.90；
实际使用 1.99 工具链验证 —— 本机与 CI 都用 stable）。
**例外是编辑器**：`quench3d-editor` 单独声明 `rust-version = "1.95"`，因为 egui 0.37
要求 1.95，而 0.37 是唯一同时满足「MSRV 可用」与「依赖 `wgpu 30`」的版本
（0.35 还停在 `wgpu 29`，两个 wgpu 类型不兼容）。

```bash
cd quench3d

# 最小示例：一个自转的方块
cargo run --release --example hello_cube

# 物理沙盒：堆叠箱子 / 斜面 / 球体，空格发射小球，可射线拾取
cargo run --release --example physics_playground

# 3D 模型查看器：加载 glTF/GLB/OBJ（含 MTL），空格丢球验证模型碰撞体
cargo run --release --example model_viewer
cargo run --release --example model_viewer -- 你的模型.glb

# 无窗口集成验证：物理 + 场景 + 渲染跑 240 步，输出前后两张 PNG
cargo run --release --example headless

# 编辑器（桌面应用；同一套界面在浏览器里就是 Web 客户端，见 §2）
cargo run --release -p quench3d-editor
cargo run --release -p quench3d-editor -- 场景.json   # 启动即打开一个场景
```

> 开发期建议直接用 `--release`：debug 下 CPU 侧物理与场景同步会成为瓶颈
> （`Cargo.toml` 里已经把 `profile.dev` 设为 `opt-level = 1`、依赖设为 3）。

### 1.1 编辑器：桌面应用 = Web 客户端

`quench3d-editor` 是一个**手工操作界面**：不用写代码就能摆场景、调参数、跑物理、
存成场景文件。它和 CLI / MCP / SDK 是同一套引擎能力的第四个入口，
而且桌面与浏览器**跑的是同一套界面代码**（egui），不是两个实现。

| 区域 | 内容 |
| --- | --- |
| 顶部工具栏 | 播放/暂停、单步、重置；**↶ 撤销 / ↷ 重做**；添加图元（方块/球/圆柱/胶囊）、复制、删除、清空；**变换手柄模式（移动/旋转/缩放）+ 坐标系（世界/局部）**；网格/阴影/坐标轴开关；相机模式；打开/保存/截图/帮助 |
| 左侧场景树 | 物体列表（点选）、`⌖` 聚焦到物体；**父子层级按缩进显示**（拖父物体时子物体跟着走）；导入模型（glTF/OBJ）展开出的子网格按 `模型 #i · 路径` **分组**显示，标题上有"整组聚焦" |
| 中央视口 | 3D 画面本身；**左键拖 = 环绕**、右键拖 = 平移、滚轮 = 缩放；单击拾取物体（射线 vs 网格 AABB）；**选中物体身上有变换手柄**（拖轴箭头/圆环/方块改位置/朝向/缩放） |
| 右侧属性面板 | **光照（场景级：太阳方向/强度、投影、阴影强度/覆盖半径、环境光、曝光、天空与天顶/地平线颜色、雾、网格）**、**模型引用（位置/旋转/缩放，改完当场生效）**、名字、可见性、**父物体（下拉选择，改父不跳位）**、变换（位置/旋转/缩放数值框；有父物体时是**局部**量，并同时显示世界位置）、外观（颜色乘子、金属度/粗糙度覆盖、自发光）、物理（类型 动态/静态/视觉、质量/摩擦/弹性、停下、踢一脚） |
| 底部状态栏 | 帧率、绘制统计、当前文件名与操作反馈 |

* **变换手柄（gizmo）**：`W`/`E`/`R` 切移动/旋转/缩放，`L` 切世界/局部坐标系；
  手柄**屏幕定长 88 px**（贴脸和远处一样大），抓取容差 10 px 且**命中优先于物体拾取**
  （被墙挡住的手柄也能抓）；`Ctrl` 吸附（平移 0.25 / 旋转 15° / 缩放 0.1，作用在整段拖拽的累计增量上）、
  `Shift` 精细 ×0.1；拖拽中 `Esc` 取消并还原。一次拖拽只留**一步**历史。
* **撤销 / 重做**：`Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y`。整场景快照式（上限 128 步），
  拖拽与滑杆整段算一步，**撤销不改变你的视角**，撤销后的场景与快照逐字节相同。
  边界：恢复场景内容（物体/变换/材质/刚体参数/灯光），不恢复"物理跑到哪一步"（速度、睡眠）。
* **`F` 聚焦选中物体**（未选中时聚焦整个场景）。
* **导入的模型（glTF/OBJ）可整体编辑**：模型的每个子网格在引擎里是一个普通物体，
  编辑器靠"物体↔模型"映射（`runner::ModelInstance`，装载/撤销/重做时刷新）把它们当成一组——
  在视口里拖其中任何一个 = **整组联动**，新变换当场写回场景文件的 `models[i]`；
  右侧"模型"面板改位置/旋转/缩放同样**当场生效**（不再是"重开后生效"）。
  边界：一个模型只有一个变换，不能单独挪某个子网格；缩放对模型强制等比
  （场景格式里 `models[i].scale` 是一个数）。
* **父子层级**：场景树按缩进显示父子关系，属性面板里用"父物体"下拉改父（也可以提回根物体）。
  改父与**拖动父物体**都会**保持子物体的世界位姿**（改父不跳位、删父不跳位），
  一次改动只留一步历史。边界：子物体继承父物体的**位置与朝向，但不继承缩放** ——
  这个格式里 `scale` 就是图元尺寸（`shape.size` 折算而来），乘法继承会让"挂在大地板下的树"
  被压成一块饼；带刚体的物体不能有父物体（它的位姿由物理决定，校验器与引擎都会拒绝）。
* **相机**：环绕相机由编辑器接管（`Engine::auto_camera = false`），`Tab` 仍可切到自由飞行；
  其余引擎内置快捷键照旧（`R` 对准场景、`F3`/`F4` 网格/阴影、`F5`/`F6` 调试、`P` 暂停、`Esc` 退出）；
  **例外**：轨道相机下 `W`/`E`/`R`/`L` 归变换手柄（飞行相机下它们仍归相机与引擎）；
  在输入框里打字时这些快捷键自动让位（`egui_wants_keyboard_input`）。
* **一帧是怎么拼出来的**：先跑界面（egui）→ 拿到这一帧的交互 → `Engine::tick` 推进物理与提交绘制 →
  3D 画进交换链纹理 → egui 在同一张纹理上叠一层（`LoadOp::Load`）→ 呈现。
  所以没有第二条渲染循环，也没有两份场景状态。
* **场景文件与 CLI 完全共用**（`quench3d.scene`，见 §6）：
  ```bash
  quench3d scene --prompt "三层堆叠的箱子" -o 场景.json   # 或手写 / 由别处生成
  cargo run --release -p quench3d-editor -- 场景.json      # 手工调整
  quench3d movie 场景.json --out 演示.gif                  # 渲成 GIF/APNG 或帧序列（见 §5）
  ```
  * 桌面端「打开/保存」走**路径输入框 + 真实文件系统**（不引入原生对话框依赖）；
  * Web 端走浏览器：页面提供文件选择与下载（`web/index.html` 里的
    `__quench3d_pick_scene_file` 与 wasm 导出的 `on_scene_file`），wasm 侧只收字符串。
* **中文界面**：egui 自带字体**一个中文字形都没有**（界面会变成一排豆腐块），
  所以编辑器内嵌了一份中文子集字体（`crates/quench3d-editor/assets/ui-font.ttf`，563 KB，
  源自 Droid Sans Fallback，Apache-2.0；字符集 = 界面文本 + GB2312 一级字 + 标点，共 3899 字），
  挂在字体栈末尾作为兜底。启动时会打一行日志把它变成可观测事实：
  `编辑器字体：中文子集可用 = true`。

## 2. 快速开始（Web 端）

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129   # 必须与 wasm-bindgen crate 版本一致

tools/build_web.sh web_client                      # 默认就是它：编辑器（Web 客户端）
python3 -m http.server 8000 --directory web
# 打开 http://localhost:8000/
```

> **线上已经部署好了**：<https://gh503.github.io/quench3d/> —— 每次 push 到 `main`
> 由 `.github/workflows/pages.yml` 重新构建并发布（同一个 `tools/build_web.sh web_client`）。
> 首次打开要下 ~11 MB 的 wasm；想本地跑就按上面三行来。

页面里就是 **§1.1 那个编辑器**：工具栏、场景树、属性面板全在画布上由引擎自己画出来，
页面只负责三件事——给一张画布、在 `start()` 之前探好后端、把用户选中的场景文件读成文本交给 wasm。
（`physics_playground` 这个示例仍然可以构建成 Web 包：`tools/build_web.sh physics_playground`，
但 `web/index.html` 现在服务的是编辑器。）

> **时间 API 在 wasm 上要小心**：`std::time::Instant::now()` 在
> `wasm32-unknown-unknown` 上是**直接 panic**（"time not implemented on this platform"），
> 而 `cargo check --target wasm32` 看不出来——只会在浏览器里第一次跑到就白屏。
> 引擎的所有取时间都走 [`clock`](crates/quench3d/src/clock.rs)（原生 `Instant`，
> wasm `performance.now()`，取不到就退 `Date.now()`）；物理的阶段计时走
> 可插拔时钟 `set_time_source()`（wasm 上由门面装入，不装也只是计时为 0，不 panic）。
> `tools/check_web.mjs` 会实例化 wasm 并真的调用示例导出的 `clock_probe()`
> 读一次时钟，还会检查产物是不是比源码旧（防止拿旧包自欺欺人）。
>
> **本机的工具链在仓库里，不在 `~/.rustup`**：`infra-dev/.rustup` 装了带 wasm32
> 目标的工具链，`infra-dev/.cargo-home` 里放了 `wasm-bindgen 0.2.129`。
> 系统默认的 `~/.rustup` 里**没有** wasm32 目标，所以直接跑脚本会报
> `can't find crate for 'core'` / `the wasm32-unknown-unknown target may not be installed`。
> `tools/build_web.sh` 现在会自己检测并切到仓库内那套（只在当前工具链确实缺
> wasm32 时才切，不覆盖你自己配好的 `RUSTUP_HOME`/`CARGO_HOME`），也会去
> `$CARGO_HOME/bin` 找 `wasm-bindgen`。要手动复现同样的环境：
>
> ```bash
> export RUSTUP_HOME=<infra-dev>/.rustup
> export CARGO_HOME=<infra-dev>/.cargo-home
> ```

产物 `web/pkg/quench3d_web_bg.wasm` 约 **11 MB**（release、`lto=thin`、
`opt-level=3`、已 strip 调试信息；胶水 `quench3d_web.js` 约 144 KB）。
构成大致是：`wgpu` 的 WebGPU + WebGL2 两条后端、引擎与物理、glTF/OBJ 与图片解码
（自检探针要用）、egui 与内嵌的中文子集字体。
比物理示例包（5.4 MB）大了一倍，多出来的主要就是 egui 这一套 + 字体 + 模型解析。
真要压体积：加一步 `wasm-opt -Oz`、把自检探针从发布包里去掉，或者只保留必需特性。
浏览器里 `Esc` 退出、`Tab` 切换相机、`F3`~`F6` 是调试开关，和桌面端完全一致。

#### 后端怎么选（WebGPU → WebGL2 回退）

**先探再定**：页面在 `start()` 之前用 `navigator.gpu.requestAdapter()` 探一次
（**不碰画布**），把结论写进 `globalThis.__quench3d_backend`，引擎据此排候选后端：

| 探测结果 | 候选 | 说明 |
|---|---|---|
| 有 WebGPU 适配器 | WebGPU 优先，WebGL2 兜底 | 正常情况 |
| 没有 `navigator.gpu`，或 `requestAdapter()` 返回 null | **只**试 WebGL2 | `navigator.gpu` 存在 ≠ 有适配器（Linux Chrome 没开 Vulkan、软件光栅环境都这样） |
| 网址带 `?backend=gl` / `?backend=webgpu` | 按指定的来 | 强制后端，排查用 |

为什么不能"交给 wgpu 两个一起试"（这是踩过的坑，两道闸门都有回归测试）：
wgpu 只要看到请求里带 `BROWSER_WEBGPU` 且 `navigator.gpu` 存在，就**只**建 WebGPU
上下文、连 WebGL2 实例都不建，于是报 `No available adapters`；更要命的是**一旦对着
这张画布要过 `webgpu` 上下文，同一张画布的 `getContext("webgl2")` 会返回 null**
（一张画布只能有一种上下文，且不可逆）——事后回退在同一条路上根本走不通。

`tools/check_web.mjs` 会静态挡住两件事：wasm 里必须真的含 WebGL2 后端、
页面的后端探测必须发生在 `start();` 之前。**真正在浏览器里**的那一层用
[`tools/probe_webgl2.sh`](tools/probe_webgl2.sh)（headless Chrome + SwiftShader，
`--enable-unsafe-swiftshader --use-angle=swiftshader`，缺省 `?backend=gl`）跑：
它在日志里要求「编辑器第一帧已渲染」且页面遮罩（`#overlay`）被收掉，
出现 `panicked at` 就判红。闸门是**遮罩**：Rust 侧只在第一帧真的画出来之后才收它，
所以"启动成功但白屏"过不了这一关。

> ⚠️ **第 57 轮的教训（为什么必须有一条真浏览器的探针）**：GL/WebGL2 是**另一套语言后端**
> （naga → GLSL）。`post.wgsl` 的 SSAO 对深度纹理用 `textureLoad` 时，naga 的 GLSL 后端
> 直接拒绝（`WGSL \`textureLoad\` from depth textures is not supported in GLSL`），
> 而 wasm 上管线校验错误是**致命**的 —— **编辑器启动即 panic**，本地门禁（Vulkan）与
> Linux CI 却全绿。现在有两层覆盖：这条浏览器探针（真机行为）
> + 一条不需要显卡的 naga GLSL 翻译守卫（可编译性）。
> GL 上的能力矩阵（SSAO 按能力关闭、MSAA > 1 的既有问题）见
> [docs/production_readiness.md](docs/production_readiness.md) §12.7。

### 2.1 Web 端加载模型：走字节，不走路径

wasm 里没有文件系统，`load_model("assets/chair.glb")` 只会得到一句
"找不到资源，已尝试: chair.glb, assets/chair.glb, …"——对着这句话没人知道该怎么办。
所以 Web 端走 [`load_model_from_bytes`](crates/quench3d/src/assets.rs)：

```rust
// 在你自己的 wasm 侧 Rust 代码里（例如一个 #[wasm_bindgen] 包装函数）
#[wasm_bindgen]
pub fn load_chair(bytes: &[u8]) -> Result<(), JsValue> {
    let model = quench3d::assets::load_model_from_bytes("chair.glb", bytes)?; // 由 JS fetch 得到
    for (mesh, material) in model.upload(&mut engine.renderer, &engine.gpu) {
        engine.scene.add(mesh, material, Transform::IDENTITY);
    }
    Ok(())
}
```

* **`.glb` 是自包含的**：任何来源的字节都能直接解析（Web 推荐的格式）；
* `.gltf` 若用 `uri` 引用外部 `.bin`/贴图，内存里无从解析 → 报错并让你改用 `.glb`
  （或把资源内联成 `data:` URI）；
* `.obj` 只解析几何：`mtllib` 不会被读取（没有目录可查），材质用缺省值；
* `load_model(path)` 在 wasm 上返回 `AssetError::Unsupported`，**错误文本里直接点出
  `load_model_from_bytes`**，不是一句没用的 NotFound。

`tools/check_web.mjs` 在 Node 里把这两条路都真的跑一遍（示例导出了两个探针）：
按路径加载的报错里必须出现 `load_model_from_bytes`，从字节加载必须真的解析出
`meshes=1 triangles=1`。这是"Web 端能加载模型"的硬证据——`cargo check --target wasm32`
只能证明它编译得过。

> 代价说清楚：这两个探针把 glTF/OBJ 解析器与图片解码器链进了示例 wasm。
> 实测（同一台机器，`--release`）：**去掉探针 4.56 MB，带上探针 5.39 MB**——
> 那 0.83 MB 就是模型解析本身。真要给演示包瘦身，去掉这两个探针即可；
> 但任何真的要加载模型的 Web 应用都需要这部分代码。

## 想读代码从哪里开始

**想先知道"它离生产还差什么"**：读 [docs/overnight_report.md](docs/overnight_report.md)（本轮做了什么、你怎么验收）
与 [docs/production_readiness.md](docs/production_readiness.md)（逐域差距矩阵、六个场景判定、四类一票否决项）。

先看 **[docs/getting_started.md](docs/getting_started.md)**（心智模型、三条开发路线、配方表、坑清单），
再打开 **[crates/quench3d/examples/starter_game.rs](crates/quench3d/examples/starter_game.rs)**：
一个能跑、能改、能进 CI 的完整骨架（`cargo run --release --example starter_game`）。
想确认文档里的 API 没写错：`crates/quench3d/tests/api_snippets.rs` 把文档 §3 的每段配方
都编译一遍——文档写错名字，`cargo check --all-targets` 就会先炸。

## 3. 五分钟上手

### 3.1 最小可运行代码

```rust
use quench3d::prelude::*;

// 最简：闭包就是一个 Game，每帧调用一次
fn main() -> Result<(), Box<dyn std::error::Error>> {
    quench3d::run(
        EngineConfig { title: "我的第一个场景".into(), ..Default::default() },
        |_engine: &mut Engine, _dt: f32| { /* 每帧逻辑 */ },
    )
}
```

需要状态时实现 `Game` trait（所有方法都有默认实现，只覆写需要的）：

```rust
struct Demo { cube: BodyHandle }

impl Game for Demo {
    fn init(&mut self, engine: &mut Engine) {
        // 地面（静态刚体）+ 动态方块，挂到场景后会自动跟随物理
        let ground = engine.world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
        );
        let ground_mat = engine.assets.solid_material(
            &mut engine.renderer, &engine.gpu,
            Vec4::new(0.55, 0.58, 0.62, 1.0), 0.0, 0.90,
        );
        engine.scene.add_with_body(engine.assets.unit_plane, ground_mat, ground, &engine.world, "地面");

        self.cube = engine.world.add_builder(
            BodyBuilder::dynamic(Shape::Box { half_extents: Vec3::splat(0.5) })
                .position(Vec3::new(0.0, 4.0, 0.0))
                .restitution(0.4)
        );
        let cube_mat = engine.assets.solid_material(
            &mut engine.renderer, &engine.gpu,
            Vec4::new(0.85, 0.35, 0.30, 1.0), 0.10, 0.50,
        );
        engine.scene.add_with_body(engine.assets.unit_cube, cube_mat, self.cube, &engine.world, "方块");
    }

    fn update(&mut self, engine: &mut Engine, _dt: f32) {
        if engine.input.key_pressed(KeyCode::Space) {
            if let Some(body) = engine.world.body_mut(self.cube) {
                body.apply_impulse(Vec3::Y * 6.0);
            }
        }
    }
}
```

> 完整可运行版本见 `crates/quench3d/examples/hello_cube.rs`。

### 3.2 场景与资源

```rust
let mesh = engine.assets.unit_cube;   // 内置：unit_cube / unit_sphere / unit_cylinder / unit_capsule / unit_plane
let mat  = engine.assets.solid_material(&mut engine.renderer, &engine.gpu, Vec4::new(0.9, 0.4, 0.3, 1.0), 0.0, 0.6);
let id   = engine.scene.add(mesh, mat, Transform::from_translation(Vec3::Y * 2.0));

engine.scene.objects[id].transform.scale = Vec3::new(2.0, 1.0, 2.0);
engine.scene.objects[id].visible = false;

// 带物理体：之后 sync_from_physics 会自动把刚体位姿写回变换
engine.scene.add_with_body(mesh, mat, body, &engine.world, "箱子");

// 自定义网格与材质
let mesh = engine.renderer.add_mesh(&engine.gpu, &MeshData::torus(1.0, 0.3, 32, 16));
let mat  = engine.renderer.add_material(&engine.gpu, MaterialDesc::from_srgb(1.0, 0.8, 0.2, 1.0).pbr(0.15, 0.9));

// glTF / glb / OBJ 模型：自动按扩展名选择解析器，每个子网格注册成一个场景物体
let ids = engine.load_model("chair.glb", Transform::from_translation(Vec3::Y * 1.0))?;
engine.orbit.focus_on(&engine.scene.bounds_of(&engine.renderer, &ids).unwrap());

// 关卡地形：同时生成静态三角网格碰撞体（可落脚、可被射线命中）
let (ids, terrain) = engine.load_model_as_static("level.obj", Transform::IDENTITY, 0.9)?;
```

### 3.3 物理

```rust
let mut world = PhysicsWorld::with_gravity(Vec3::new(0.0, -9.81, 0.0));

// 7 种形状
Shape::Sphere { radius: 0.5 }
Shape::Box { half_extents: Vec3::splat(0.5) }
Shape::Capsule { half_height: 0.6, radius: 0.3 }
Shape::Cylinder { half_height: 0.5, radius: 0.5 }
Shape::ConvexHull { points }
Shape::Plane { normal: Vec3::Y }
Shape::TriMesh { vertices, indices }        // 静态地形/关卡

// 生成刚体：dynamic / static / kinematic，链式配置
let body = world.add_builder(
    BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
        .position(Vec3::Y * 5.0)
        .density(1.0)
        .restitution(0.6)
        .friction(0.4)
        .linear_damping(0.02)
        .can_sleep(true)
);

world.step(1.0 / 60.0);              // 定步长
world.step_with_accumulator(dt);     // 引擎内部按 fixed_dt 拆分子步（最多 8 步）

// 查询：最近一条射线 / 沿路所有命中 / 任意形状重叠 / 到某个刚体的最近距离
if let Some(hit) = world.raycast(origin, dir, 100.0) {
    println!("命中 {:?} 于 {:?}，距离 {}", hit.handle, hit.point, hit.distance);
}
for hit in world.raycast_all(origin, dir, 100.0) {          // 距离升序，平局按槽位
    println!("沿路命中 {:?} 距离 {}", hit.handle, hit.distance);
}
// 层过滤是**单向**的（只看被查方在不在掩码里，与物理碰撞的对称判据不同）
let filter = QueryFilter::default().mask(0b101).sensors(false);
let all = world.raycast_all_filtered(origin, dir, 100.0, filter);
let overlapping = world.overlap_shape(&Shape::Box { half_extents: Vec3::splat(0.5) },
                                      &Transform::from_translation(p));
if let Some(hit) = world.raycast_in(origin, dir, 100.0, filter) { /* 同一条判据的最近命中 */ }

// 一帧里查很多次时：索引按帧重建一次（快照），查询只走射线经过的格子；
// 世界动过（version 变了）它会自动回退到线性扫描——结果永远一致，只是不快。
let index = world.build_query_index();
if let Some(hit) = index.raycast(&world, origin, dir, 100.0, QueryFilter::default()) {
    println!("索引命中 {:?}", hit.handle);
}
if index.is_stale(&world) { /* 该重建了（上一步 step 之后必然为真） */ }
for c in world.contacts() {
    println!("{:?} <-> {:?} 法线 {:?} 冲量 {}", c.a, c.b, c.normal, c.impulse);
}

// 碰撞过滤（层/掩码，**对称**：双方都愿意才碰）、传感器（触发区）、CCD
world.add_builder(
    BodyBuilder::r#static(Shape::Box { half_extents: Vec3::splat(0.5) })
        .layer(3)                 // 自己在第 3 层
        .collides_with(1 << 0)    // 只和第 0 层碰
        .sensor(true)             // 触发区：报事件，不产生冲量
);
world.add_builder(
    BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
        .linear_velocity(Vec3::X * 300.0)
        .ccd(true)                // 高速小物体：每子步一条中心射线，不穿薄墙
);
for e in world.sensor_events() {  // 只在进入/离开的那一子步有内容
    println!("{} {:?} {:?}", if e.started { "进入" } else { "离开" }, e.sensor, e.other);
}

// 物理材质：静摩擦独立于动摩擦；摩擦/恢复的合成规则可选（缺省 = 老行为）
world.add_builder(
    BodyBuilder::dynamic(Shape::Box { half_extents: Vec3::splat(0.5) })
        .friction(0.4)                    // 动摩擦（已经在滑动时）
        .static_friction(0.9)             // 静摩擦（切向速度很小时）——"推得动但推不住"
        .friction_combine(CombineRule::Min)   // 与别的物体接触时听谁的
        .restitution_combine(CombineRule::Average)
);
world.config.friction_combine = CombineRule::Min;      // 世界缺省（不设 = 几何平均）
world.config.restitution_combine = CombineRule::Max;  //              （不设 = 取大）

// 睡眠：自动（静止 0.5 秒后整岛一起睡）之外，也能手动控制
world.wake_up_all();                  // 一趟唤醒全部，返回"叫醒了几个"
if let Some(b) = world.body_mut(handle) {
    b.sleep();                        // 立即入睡，**速度清零**
    b.set_can_sleep(false);           // 硬承诺：这辈子都不睡（睡下去下一步也会醒）
}
```

关节（5 种）：`Fixed`（焊接）、`Ball`（球窝，可选**锥形限位**）、`Hinge`（铰链，
带角度限位、速度马达与**位置伺服**）、`Distance`（距离/弹簧）、`Slider`（滑块，带轴向限位）。
每个关节都可以设**断裂阈值**（`break_force` / `break_torque`：本步约束力量级超限就把关节
移除，`PhysicsWorld::broken_joints()` 与 `simulate` 报告的 `joints_broken` 留痕）。

```rust
use quench3d::prelude::*;

// 伺服：门自动开到 90° 并停住（k p = 8 / kd = 1 是缺省）
world.add_joint(Joint::Hinge {
    a: frame, b: door, local_anchor_a: Vec3::ZERO, local_anchor_b: Vec3::new(-0.5, 0.0, 0.0),
    local_axis_a: Vec3::Y, local_axis_b: Vec3::Y, limits: None,
    motor: Some(HingeMotor::servo(90f32.to_radians(), 40.0).gains(8.0, 1.0)),
});

// 锥形限位：手臂最多摆到 60° 的锥面内（绕轴自转仍然自由）
world.add_joint(Joint::Ball {
    a: torso, b: arm, local_anchor_a: Vec3::ZERO, local_anchor_b: Vec3::ZERO,
    cone_limit: Some(BallConeLimit {
        half_angle: 60f32.to_radians(), local_axis_a: Vec3::Y, local_axis_b: Vec3::Y,
    }),
});

// 断裂：绳子受力超过 500 N 就断
let rope = world.add_joint(Joint::Distance {
    a: anchor, b: load, local_anchor_a: Vec3::ZERO, local_anchor_b: Vec3::ZERO,
    rest_length: 2.0, stiffness: 0.0, damping: 0.0,
});
world.set_joint_break(rope, Some(500.0), None);
assert_eq!(world.broken_joints().len(), 1);   // 超限之后这里会有记录
```

> 关节两端**缺省不互相碰撞**（`collide_connected = false`，与 Bullet / PhysX 一致）：
> 铰链/球窝的两端在几何上就是重合的，让它们参与碰撞等于让门和门轴互相挤。
> 需要"连起来还能互相弹开"时显式打开 `world.set_joint_collide_connected(handle, true)`。
> 另外：**被马达驱动的刚体不会自动睡眠**（伺服到位时速度正好是 0，否则会被冻结在
> "还没到位"的角度上）。

复合形状（一个刚体多个 Shape）：桌子的"板 + 腿"、载具的底盘 + 车厢不再需要多刚体 + 关节硬拼。

```rust
use quench3d::prelude::*;

// 顶板 + 两条腿：**一个**刚体、三个形状
let table = world.add_builder(
    BodyBuilder::dynamic(Shape::Box { half_extents: vec3(0.6, 0.05, 0.4) })
        .position(vec3(0.0, 0.75, 0.0))
        .part_at(Shape::Cylinder { radius: 0.05, half_height: 0.35 }, vec3(0.5, -0.4, 0.3))
        .part_at(Shape::Cylinder { radius: 0.05, half_height: 0.35 }, vec3(-0.5, -0.4, -0.3)),
);
let body = world.body(table).unwrap();
assert_eq!(body.part_count(), 3);
assert_eq!(world.body_count(), 1);        // 仍然是**一个**刚体

// 部件可以带朝向（局部位姿相对刚体原点）
let arm = world.add_builder(
    BodyBuilder::dynamic(Shape::Capsule { radius: 0.1, half_height: 0.3 })
        .part(ShapePart::at(
            Shape::Box { half_extents: Vec3::splat(0.15) },
            vec3(0.0, -0.45, 0.0),
            Vec3::Z.axis_angle(0.5),
        )),
);
let _ = arm;
```

> 质量、质心、惯性张量按所有部件合成；碰撞、AABB、射线/距离/重叠查询、角色控制器都**逐部件**处理；
> 每对刚体最多保留 **8 个接触点**（超限按穿透深度取最深的）。**单形状刚体走原来的路径**，
> 数值与以前逐位一致。任一部件是半空间/三角网 → 整个刚体强制静态。
> 部件**没有**自己的摩擦/恢复（那是刚体级的），也**不能**是三角网。

事件总线（"这一步发生了什么"）：接触开始/结束、传感器进入/离开、睡眠/唤醒、关节断裂。

```rust
let mut events = Vec::new();
world.step(1.0 / 60.0);
world.drain_events(&mut events);          // 缓冲复用，不产生分配
for e in &events {
    match e.kind {
        PhysicsEventKind::ContactBegin => { /* 落地音效 */ }
        PhysicsEventKind::JointBroken => { /* 断裂特效：e.force / e.torque */ }
        _ => {}
    }
}
// 队列有上限（默认 4096）：宿主忘取时会丢新事件，但 `events_dropped()` 会说明丢了多少
assert_eq!(world.events_dropped(), 0);
```

角色控制器（胶囊、可上台阶、贴地、沿墙滑动）：

```rust
let character = CharacterController { radius: 0.35, height: 1.7, ..Default::default() };
let result = character.move_and_slide(&mut world, feet_position, desired_translation);
if result.grounded { /* 可以跳跃 */ }
```

### 3.4 相机与输入

```rust
engine.camera_mode = CameraMode::Orbit;   // Orbit（环绕）/ Fly（自由飞行）
engine.orbit.focus_on(bounds);            // 对准一批物体
engine.fly.speed = 12.0;

if engine.input.key_down(KeyCode::KeyW) { /* ... */ }
if engine.input.mouse_button_pressed(MouseButton::Left) {
    let ray = engine
        .camera()
        .screen_ray(engine.input.mouse_pos(), engine.viewport());
    if let Some(hit) = engine.world.raycast(ray.origin, ray.dir, 200.0) {
        println!("点到了 {:?}", hit.handle);
    }
}
```

内置快捷键：`Tab` 切相机、`R` 对准场景、`F3` 网格、`F4` 阴影、`F5` 接触点、
`F6` 包围盒、`P` 暂停、`Esc` 退出。

### 3.5 截图 / 离屏渲染

```rust
engine.screenshot(1920, 1080, "shot.png")?;                    // 桌面：写文件；Web：触发下载

// 不需要窗口（CI、批量出图）
let image = quench3d::render_offscreen(960, 640, &config, |engine| { /* 搭场景 */ })?;
quench3d::write_png(&image, "out.png")?;
```

## 4. 渲染器能力

| 能力 | 说明 |
| --- | --- |
| 着色 | 前向 Cook–Torrance PBR（金属度/粗糙度）、法线贴图、指数雾；物体写**线性 HDR** |
| 后处理 | `rgba16float` 中间目标 → 可选 SSAO（深度-only，法线由深度重建，`post.wgsl`）→ 可选 bloom（半分辨率亮度提取 + 可分离高斯）→ 统一 ACES 色调映射（**唯一一份**）→ 可选 FXAA（tonemap 之后，见 `fxaa.wgsl`）；混合因此发生在 tonemap 之前。MSAA > 1 且开 SSAO 时多一趟深度解析（`depth_resolve.wgsl`）。**GL/WebGL2 上 SSAO 与深度解析按能力整体关闭**（naga 的 GLSL 后端不支持读深度纹理，见 [production_readiness §12.7](docs/production_readiness.md)）；tonemap / bloom / FXAA 不受影响 |
| 阴影 | 太阳：单张（级联时 2×2 图集）`Depth32Float` 阴影贴图（分辨率是渲染器构造参数 `RendererOptions::shadow_map_size`，**不在场景文件里**），按纹素对齐的正交拟合，3×3 PCF（硬件比较采样）；阴影强度（`.strength`）与覆盖半径（`.radius`）可从场景文件调；**级联阴影** `.cascades`（1~4，缺省 1 = 单级老行为）+ `.split_lambda`（0~1）：多级按视锥 PSSM 切段，主 pass 按视空间深度选级。**局部光阴影**：`SpotLight.cast_shadows`（一张透视 2D 深度图，视锥 = 外锥角）与 `PointLight.cast_shadows`（**立方体阴影**：`texture_depth_2d_array` 6 层，每层 90°，面选择 + 面内 uv 是 CPU 纯函数、WGSL 逐字镜像，靠"公式 == 矩阵"的 CPU 测试保证两边一致）；两者**各自整帧只支持一盏**（第一盏要求投影的），缺点都是缺省 `false`（老场景像素逐位不变），两套图互相独立 |
| 光照 | 1 个平行光 + 最多 **8** 盏局部光（点光源与聚光灯共享名额、点光源优先；`MAX_POINT_LIGHTS`，与 WGSL 数组长度一致；超出部分给出警告后忽略），平滑衰减到半径处归零，聚光灯有内外锥角的平滑过渡，环境光 + 程序化天空（互为补充的 IBL 近似）；**指数距离雾**（`lighting.fog`，不写 = 关闭） |
| 天空/地面 | 全屏三角形程序化天空（三个渐变颜色可从 `lighting.sky_colors` 调）+ 解析无限网格（`fwidth` 抗锯齿 + 深度写入） |
| 批处理 | 按 (材质, 网格) 分批 + 实例化（96 字节/实例）+ 逐实例视锥剔除 |
| 其它 | MSAA（1/2/4/8；GL/WebGL2 上按能力夹到 1）、Alpha 混合与 Alpha 裁剪、无光照材质、调试线段/接触点/包围盒 |
| 后端 | 桌面 Vulkan/Metal/DX12，Web WebGPU→WebGL2 回退；同一套 WGSL。GL/WebGL2 上：SSAO 按能力关闭（读不了深度纹理）、**MSAA 按能力夹到 1**（多采样纹理在 WebGL2 的 glow 里是 `panic!`、在原生 GL 上出黑帧），见 [production_readiness §12.7](docs/production_readiness.md) |

渲染器可以脱离引擎单独使用（`quench3d-render` 的 `offline_render` 示例演示了
不带窗口、不带物理，直接出图的用法）。

> ⚠️ 点光源在 2026-10-15 修过一个**真 bug**：`FrameUniforms` 把点光源写成两个并列数组
> （SoA：8 个位置 + 8 个颜色），而 WGSL 的 `array<PointLight, 8>` 是**结构体数组**
> （AoS：pos0, color0, pos1, color1…）。布局错位后，着色器把"下一盏灯的位置"当成
> "这一盏灯的颜色和强度"在读——**场景里只给一盏灯时完全不亮**，多盏灯时颜色是错的。
> 修复后：`scene.json` 的 `lighting.point_lights` 才真正可用，但**用 studio 预设的
> 画面会变化**（补光颜色终于对了：同一个箱子中心从偏暖的 `(198, 203, 175)` 变成
> `(162, 180, 202)`）。回归防线是 `frame_uniform_layout_matches_wgsl`（把偏移量钉死）
> 与 `crates/quench3d-render/tests/point_lights.rs`（单盏红灯必须照亮且变红）。

## 5. 示例清单

共 **10 个**示例（`quench3d` 6 个、`quench3d-render` 2 个、`quench3d-physics` 1 个、
`quench3d-editor` 1 个）；编辑器另有可执行程序 `quench3d-editor`：

| 示例 | 演示内容 | 运行 |
| --- | --- | --- |
| `hello_cube` | 最小可运行程序：自转方块 + 地面 | `cargo run -p quench3d --example hello_cube` |
| `starter_game` | 起步模板：场景/物理/逐帧逻辑的骨架，支持 `--headless out.png` 无头截图与 Web 构建 | `cargo run -p quench3d --example starter_game` |
| `physics_playground` | 完整沙盒：堆叠、斜面、生成刚体、射线拾取、调试绘制；桌面/Web 共用 | `cargo run -p quench3d --example physics_playground` |
| `headless` | 无窗口集成验证：240 步物理 + 渲染出图，并断言不穿地/无 NaN | `cargo run -p quench3d --example headless` |
| `offline_render` | 渲染特性回归：天空、网格、阴影、地面逐个开关并统计像素 | `cargo run -p quench3d-render --example offline_render` |
| `shadow_probe` | 阴影诊断：自阴影检测 + 阴影方向与解析落点对比（误差 < 1px）+ **级联 sweep**（1~4 级 × 远/近/深景，打印半影比例与纹素足迹） | `cargo run -p quench3d-render --example shadow_probe` |
| `events` | 事件总线：接触开始/结束 · 传感器进出 · 睡眠/唤醒 · 关节断裂（报告里按物体名输出） | `cargo test -p quench3d-cli --test cli_e2e -- simulate_report_includes_an_event_stream` |
| `joints` | 5 种关节：铰链门（速度马达 + **位置伺服**）、垂直滑块（轴向限位）、弹簧、焊接、球窝摆（**锥形限位**），以及**断裂阈值**与"关节两端不互相碰撞" | `cargo run -p quench3d --example joints` |
| `model_viewer` | 模型加载：glTF/GLB/OBJ+MTL，轨道相机对准模型；`--collide` 把模型变成静态三角网格碰撞体（丢球验证） | `cargo run -p quench3d --example model_viewer` |
| `bench` | 物理性能与分配基准（吞吐 / 分配次数 / 长跑稳定性，`--phases` 看阶段分解、`--layout` 换布局） | `cargo run --release -p quench3d-physics --example bench` |
| `web_client` | Web 客户端入口（§1.1 的编辑器跑在浏览器里；另导出 CI 自检探针） | `tools/build_web.sh web_client` |

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
| `info <模型>` | glTF/GLB/OBJ：网格、材质、贴图、顶点、三角形、包围盒 | 否 |
| `validate <场景>` | 校验场景，返回全部诊断（带 JSON 路径） | 否 |
| `simulate <场景>` | 只跑物理，报告每个刚体的位置 / 速度 / 位移 / 休眠 / 接触，可按 `--trace` 采集轨迹 | 否 |
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
quench3d simulate scene.json --steps 600 --json
quench3d simulate scene.json --trace-all --json            # 附带每条轨迹的结论
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

**仿真视频（`movie`）**：逐帧推进物理并渲染。两条路：PNG 序列交给 `ffmpeg`，
或者用**内置纯 Rust 编码器**直接出 GIF/APNG（不装任何外部工具）：

```bash
# 路线 A：PNG 序列 + ffmpeg（长视频、高压缩率）
quench3d movie scene.json --frames 300 --frames-dir frames --json
# data.frames_dir / data.trace / data.digest / data.ffmpeg / data.probes（写了 probes 才有）
ffmpeg -framerate 60 -i frames/frame_%05d.png -c:v libx264 -preset slow -crf 18 \
       -pix_fmt yuv420p -movflags +faststart movie.mp4

# 预检：只校验 + 跑物理（几毫秒），不花 GPU 时间、不写任何文件
quench3d render scene.json --dry-run --json
# data.dry_run / data.would_write / data.simulate（物理是真跑的，渲染统计全为 0）

# 多 agent 并行：输出已存在就报错（退出码 2，error.kind = output_exists），互相不覆盖
quench3d render scene.json --out mine.png --no-clobber --json

# 路线 B：内置编码器直接出可播放文件（README 演示、几十帧的短循环）
quench3d movie scene.json --frames 60 --encoder gif  --out demo.gif   # 256 色
quench3d movie scene.json --frames 60 --encoder apng --out demo.png   # 无损
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
  "materials": {
    "floor": { "color": [0.55, 0.58, 0.62], "roughness": 0.9 },
    "wood":  { "color": [0.78, 0.48, 0.26], "roughness": 0.55 },
    "steel": { "color": [0.70, 0.72, 0.76], "metallic": 0.9, "roughness": 0.28 },
    "格子":  { "color": [1, 1, 1], "base_color_texture": "checker.png", "uv_scale": 4 },
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
                "shadow_quality": { "strength": 0.85, "radius": 28 },
                "sky_colors": { "zenith": [0.22, 0.42, 0.78], "horizon": [0.62, 0.74, 0.88] },
                "bloom": { "intensity": 0.6, "threshold": 1.0 } },
  "physics": { "friction_combine": "min", "restitution_combine": "max",
               "static_friction_speed": 0.02 },
  "simulate": { "steps": 180, "dt": 0.0166667 },
  "render": { "output": "scene.png", "width": 960, "height": 600, "msaa": 4 }
}
```

单位：长度**米**、时间**秒**、质量由密度×体积推导、角度用**度**（字段名以 `_deg` 结尾）；
`shape.size` 是**边长不是半长**；材质颜色是 0~1 的 **sRGB** 分量。
完整字段表用 `quench3d schema --json` 读（`data.scene` 就是语法本身）。

几处容易踩的语义（都有校验/警告兜着）：

* **贴图路径相对场景文件解析**（再回退 `assets` 搜索路径），相对路径不会跟着你的 shell 目录跑；
  支持 4 个槽位：`base_color_texture` / `normal_texture` / `metallic_roughness_texture` / `emissive_texture`。
* `alpha_cutoff` 与 `alpha_blend` **同时给时裁剪优先**（会给出警告，避免你以为在混合）。
* `lighting.point_lights` 最多 8 盏生效，**不写这个字段 = 保留预设自带的补光**，写 `[]` 才是显式清空。
* `lighting.bloom` / `lighting.fog` **不写 = 关闭**（像素与关闭时逐位相同）；写了就是打开
  （fog 的 `density` 缺省 0.02，写 0 等于关）。
* `objects[].static_friction` 缺省**跟随 `friction`**（只管动摩擦的老场景行为逐位不变）；
  想表达"推得动、但推住之后不会慢慢滑"就把它写得比 `friction` 大。
* `friction_combine` / `restitution_combine` 可写在世界级 `physics` 段（缺省）或单个物体上
  （覆盖缺省）。缺省规则是**几何平均**（摩擦）与**取大**（恢复）——正是老行为；
  改成 `average`/`min`/`max`/`multiply` 会改变已有场景的观感，属于**有意**调整。
  两端都显式写了不同规则时，取 slot 序号靠前的那个（宽相输出按序号有序，所以与线程数无关）。
* `lighting.point_lights.color` / `lighting.fog.color` / `lighting.sky_colors.*` 都是
  **线性空间**数值（跟 `materials.*.color` 的 sRGB 约定不同）——想要暖光就写
  `[1, 0.92, 0.85]` 这种数，写 `[1, 235, 220]` 那种 0~255 的整数会过曝或全黑。
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
* 命名相机：`cameras: { 名字 → 相机对象 }`（缺省空 = 只渲 `camera` 段那台）。校验拦空名与
  保留名 `default`。`--camera <名字>`（`render`/`simulate`/`movie` 都支持，未知名退出码 2、
  `error.kind = unknown_camera`）已可用；多输出（`render.outputs[]`）是下一步。
* 机器可读的 JSON Schema：`quench3d schema --json-schema`（draft 2020-12，可直接喂给
  编辑器补全 / CI 校验 / agent 自检）。**schema 管"结构"（哪些键、嵌套、不许有额外字段），
  `quench3d validate` 管"语义"（范围、互斥、交叉引用）**——schema 过了不等于场景能跑。
  版本策略：当前格式版本 2；版本 1 的文件把 `version` 改成 2 即可（1→2 的唯一差别是
  `objects[].parent` 在 v2 才生效，没有 parent 的文件语义完全一样）。
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
| `duplicate_name` / `reserved_name` / `cycle` | 重名 / 保留名 / 父子成环 | 改名 / 换名 / 打断环 |
| `mutually_exclusive` | 两个字段只能留一个 | 删一个 |
| `ineffective` / `suspicious_value` | 写了不生效 / 合法但很可能不是本意 | 删掉或复核数值 |
| `unit_ambiguity` | 单位容易混（弧度 vs 度、米 vs 度） | 确认单位 |

**完整码表**（17 个，含每个码的"agent 该做什么"）见 [docs/error_codes.md](docs/error_codes.md)，
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
`quench3d_render`、`quench3d_movie`、`quench3d_simulate`、`quench3d_query`、`quench3d_ask`。

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
  python3 -m http.server 8000 --directory web   # 然后浏览器打开 http://localhost:8000/
  ```

  `check_web.mjs` 会读 `web/index.html` 的 import、核对胶水代码导出的符号、
  真正实例化一次 wasm 并确认 Rust 导出与内存都在——它能挡住「打包漏文件 /
  页面 import 了不存在的函数 / wasm 损坏」。CI 的 wasm 任务就是这么跑的
  （浏览器内的渲染仍需真实浏览器 + GPU）。

## 7. 测试与验证

```bash
cargo test --workspace         # 526 个测试（物理/CLI/渲染/数学/门面/编辑器 + doctest），0 失败
cargo clippy --workspace --all-targets     # 0 警告（含 examples / tests）
cargo fmt --all --check                    # 0 差异
cargo test -p quench3d-physics  # 只看物理（含 200 体宽相、5 秒长跑稳定性、确定性）
cargo test -p quench3d-editor   # 编辑器：场景存读往返（没有 GPU 时自动跳过）
```

`crates/quench3d/tests/readme_api.rs` 是**文档守卫**：它把 README 第 3 节的代码
片段原样编译一遍，所以「文档里写的 API」与真实 API 不会漂移。

### 7.1 编辑器的验证（桌面 + 浏览器）

编辑器的两条测试

* `scene_json_roundtrip_keeps_objects_and_ground`：默认场景 → 存成 `quench3d.scene` →
  用 **CLI 的解析器与校验器**读回来 → 装回引擎，逐项对账（物体名不丢、位置漂移 < 0.01、
  地面高度与物理半空间一致、刚体数量 = 物体数 + 1）。
  这条测试第一次跑就抓到一个真 bug：默认场景的地面是「压扁的立方体」，
  它的**视觉表面**在原点上方半个板厚，而物理半空间在原点平面上 —— 物体看起来半埋在地里，
  存读一轮还会掉下去 0.5。已改成 `unit_plane`（与 CLI 的地面表示一致）。
* `scene_to_json_reports_unrepresentable_objects`：塞一个场景格式表达不了的物体
  （三角网碰撞体）进去，导出时必须**报告物体名**，而不是静默丢掉。

两条都不依赖窗口；没有可用 GPU 时打印一行「跳过」并返回（CI 机器通常没有软件光栅驱动）。

手工验过的两条实机路径（本机软件光栅 llvmpipe / SwiftShader）：

| 路径 | 证据 |
| --- | --- |
| 桌面（X11 窗口 1280×800） | 日志 `编辑器字体：中文子集可用 = true`、`编辑器第一帧已渲染：1280x800`；窗口截屏分区统计：工具栏平均色 (46,48,52)、左面板 (29,29,30)、视口 (77,92,119)/985 种颜色、右面板 (33,34,34) |
| Web（headless Chrome + SwiftShader，走 WebGL2） | 控制台 `后端选择：gl（navigator.gpu 存在但没有适配器）`、`渲染设备就绪：… / Gl（限制档位 downlevel_webgl2）`、`编辑器字体：中文子集可用 = true`、`编辑器第一帧已渲染：1280x800` |

而且**桌面与浏览器的画面是同一张图**：两边各截一张 1280×800，
16×16 平均亮度网格的平均差 **0.2/255**（最大差 13，出现在状态栏文字那一行），
3D 视口区域的平均像素差 **0.0**；连工具栏按钮的横向像素位置都一一对上。
这比"两边都能启动"强得多——它说明两个入口用的是同一条渲染与界面路径。

**界面能不能真的操作**（这不能靠"截图像界面"证明，必须点下去看结果）：
在浏览器里用 DevTools 协议（`Input.dispatchMouseEvent`）真的按坐标点击画布，
再比对点击前后的像素：

| 操作 | 结果（平均像素差，0~255） |
| --- | --- |
| 点工具栏第一个按钮（播放） | 工具栏 **6.76**（按钮文字 播放 → 暂停）；3D 视口 **4.19** —— 方块真的掉下来了 |
| 再点一次（暂停） | 工具栏 **3.26**（文字变回 播放）；此后 8 秒视口 **0.00** —— 暂停真的停住了 |
| 在视口里点一下 | 左面板（场景树选中高亮）**0.90**、右面板（属性面板内容）**3.13** —— 射线拾取真的选中了物体 |

> 诚实说明：桌面那条路**没能**用合成事件验完。X11 侧用 `xdotool` 打进去的
> `CursorMoved` 能到（日志里有 3 条），但 `MouseInput` / `KeyboardInput` **0 条** ——
> XWayland 下 XTest 的按键/按钮事件没有转发给客户端。窗口缩放（`Resized`）能到，
> 且缩放后界面真的按新尺寸重排（900×620 时右面板真的挪到 x=640）。
> 所以"点击操作界面"这条证据是在**浏览器**里取的，用的还是同一套界面代码。

物理测试覆盖：自由落体、静置接触、三层堆叠、恢复系数、高/低摩擦斜面、静/动摩擦分离、
**复合形状**（质量/质心/惯量合成、AABB 并集、附加部件真的参与碰撞与查询）、**模板/实例**
（展开语义、名字规则、覆盖规则，以及"一份描述 → N 个独立刚体"的端到端）、**事件总线**（接触开始/结束、传感器进出、睡眠/唤醒、断裂、队列上限、确定性）、
射线检测、铰链摆与马达、**位置伺服与锥形限位**、5 种关节、角度限位、**关节断裂**、
**关节两端不互相碰撞**、确定性（逐位相等，含槽位复用）、
200 体宽相、睡眠/唤醒、三角网格与凸包（含**薄板**：模型网格当碰撞体的典型情况）、
病态输入、长时间稳定性。

资源层测试覆盖：OBJ 四边形三角化、负索引、MTL 解析（`Kd/Ks/Ns/d/map_Kd`）。

命令行工具的 **165** 个测试（`cargo test -p quench3d-cli`：lib 84 + bin 6 + 6 个集成测试二进制共 74 + 1 个 doctest），按目的分这几块：

* **契约测试**（`tests/cli_e2e.rs`，54 个）真的去 spawn `quench3d` 子进程，断言
  **单行 JSON 信封、退出码（0/2/3/4）、诊断带路径**、渲染出的确实是 PNG；
  其中 4 条是「能力真的接线了」的像素/数值证据：**材质贴图**（同一箱子带/不带贴图的
  中心色差）、**点光源**（一盏红灯必须把箱子照红，且显式清空预设补光必须改变画面）、
  **相机**（`far` 把 6 米外的箱子裁成 0 像素；`orthographic_height` 让它占满 14400 像素）、
  **刚体参数**（`gravity_scale=0` 悬停、阻尼减速、`allow_sleep=false` 不睡、
  `mass` 改变碰撞结果、`angular_velocity` 真的改朝向）、**关节锚点**（球关节带偏置锚点时
  质心被约束在 1 米外）、**关节按端局部轴**（门板转 90° 时两端共用一根局部轴会被扭平：
  薄轴与世界 Y 的夹角 `|n·Y|` 从 0.0002 变成 1.0000；只写 `axis` 与两端显式同轴的 `states`
  逐字节相同）、**定时动作**（第 100 步的冲量把静止的球重新打上天；
  `at_time: 1.0` 与 `at_step: 60` 的输出逐字节相同）、**区域探针**
  （`enter_step`/`exit_step`/区内最高速符合自由落体，且加探针不改变物理）、
  **三条路径一致**（simulate / render / movie 都要认动作与探针）、
  **`--dry-run` 一个字节都不写**（渲染/录制/帧目录全都不存在，而物理是真跑的）、
  **`--no-clobber` 拒绝覆盖**（退出码 2 + `error.kind = output_exists` + `path`，旧文件字节不变）、
  **无意义的参数组合会报错**（`simulate --dry-run` 不带 `--render` → 退出码 2，而不是悄悄忽略）；
  `movie` 那条还会断言帧序列的**像素摘要两次录制一致**（确定性）与断点续录，
  内置编码器那条则断言 GIF 魔数/结束标记、APNG 的 `acTL` 帧数与循环次数，
  以及「换编码器 digest 不变」；
  MCP 那一条还会走完整的 stdio 握手 → `tools/list` → `tools/call`。
* **大模型测试**（`tests/llm_mock.rs`，9 个）起一个**假 OpenAI 服务**（`TcpListener`），
  覆盖 Content-Length 与 chunked 两种响应、Markdown 代码块里抠 JSON、
  HTTP 4xx 报错、连接被拒时的建议、环境变量优先级——不需要真的模型。
  还有一个**假 `curl`**：它抄下自己的 argv，断言 `https` 路径的 API key 不在命令行参数里、
  且配置文件权限是 `-rw-------`（把 key 塞回 argv，这条测试会真的变红）。
* **agent 循环测试**（`tests/agent_tools.rs`，5 个）用假模型发出真实的 `tool_calls`，
  验证「调用 → 执行 → 结果回喂 → 收尾」全链路、同一回复里的多个工具、
  工具失败时把诊断回喂、以及往返上限不会死循环。
* **零分配回归**（`tests/steady_state_allocations.rs`，1 个）：计数分配器断言物理稳态
  每步堆分配为 0、活动内存不增长。
* **并行 == 串行（逐位）**（`api_tests.rs`，3 个）：无关节快速路径、有关节交错路径、
  以及**换线程数**（2/3/4 线程）结果不变；每个测试都先断言「并行真的启用了」，
  再逐位比较全部刚体状态。
* **像素级能力验证**（`crates/quench3d-render/tests/`）：alpha 混合与双面渲染
  必须改变画面；**点光源必须真的亮**（单盏红灯：旧 SoA/AoS 布局错位时它完全不亮）；
  另有 `frame_uniform_layout_matches_wgsl` 把 Rust 与 WGSL 的结构体偏移量钉死。
* **整岛休眠/唤醒**（`api_tests.rs`，2 个）：一摞箱子必须同步入睡且不来回醒；
  睡着的一摞被推动后整岛一起醒。
* **island 划分**（`island.rs`，6 个）：独立簇/静态体不粘连/关节合并/编号确定性/退化输入；
  公开 API 契约测试还验证了「两个独立的球 = 2 个岛」「叠三层箱子 = 1 个 3 体的岛」。
* **宽相暴力对照**（`broadphase.rs::pairs_match_brute_force_on_a_dense_scene`）：
  400 个代理（含静态/睡眠/全局）的宽相输出必须恰好等于 O(n²) 暴力枚举的 AABB 相交集合。
* **物理行为锁**（`quench3d-physics` 的 `digest_tests`，4 个）：60 步紧容差参考、
  400 步物理不变量、400 步摘要警报；外加 `world_inv_inertia_tests` 证明
  「预计算世界逆惯量」与原来的两次四元数旋转数学等价（最大相对偏差 1.03e-6）。
* **自动修复测试**（`tests/ask_repair.rs`，3 个）验证「模型第一次给错 → 诊断回喂 →
  第二次修好」这条闭环，以及修不好时按 `--repair-rounds` 停下。
* **单元测试**（`src/`，82 个）覆盖场景解析与校验、规模上限诊断、结构诊断路径、生成与修复提示、
  信封与退出码映射、MCP 协议形状与轨迹、agent 工具定义、base64、参数解析
  （含「schema 里每个带取值的参数都必须登记进 VALUE_FLAGS」这条守卫）。

渲染侧的数值验证不靠肉眼：`offline_render` / `shadow_probe` 会打印
「天空占比 / 网格占比 / 阴影像素占比 / 阴影质心与解析落点误差」等指标。

下面这些自检都可以直接放进 CI（前三个带断言，失败即非零退出）：

```bash
cargo run -p quench3d --example headless                     # 240 步物理 + 出图 + 断言不穿地/无 NaN
F3D_JOINTS_HEADLESS=1 cargo run -p quench3d --example joints  # 关节自检 + 出图
F3D_MODEL_HEADLESS=1 cargo run -p quench3d --example model_viewer  # 模型加载 + 碰撞体 + 丢球断言
cargo run -p quench3d-render --example shadow_probe           # 自阴影 + 阴影方向（误差 < 1px）+ 级联 sweep
cargo run -p quench3d-render --example offline_render         # 天空/网格/阴影/地面逐项开关比对
node tools/check_web.mjs                                     # Web 产物自检（页面 import + wasm 实例化）
```

命令行工具也有可直接进 CI 的冒烟（见 `.github/workflows/ci.yml` 的「CLI 冒烟测试」）：

```bash
quench3d example --out scene.json
quench3d simulate scene.json --steps 300 --json   # 不需要 GPU，断言无 NaN、不穿地
quench3d validate bad.json --json                 # 断言退出码 2 且诊断带路径
quench3d render scene.json --out out.png --json   # 有 GPU 出图，无 GPU 必须是退出码 3
```

## 8. 性能与资源

这一节只讲**量出来的数字**，以及怎么自己复现。所有数字来自一台 4 线程 x86_64 Linux
（无独显，渲染走软件 Vulkan/lavapipe），`--release` 构建。

### 8.1 一条命令测自己的机器

```bash
quench3d bench --json                                  # 缺省 100/500/1000 体，scatter 布局
quench3d bench --bodies 500,2000,5000 --no-sleep        # 「一直在算」的最坏情况
quench3d bench --layout stack --no-render               # 深堆场景，跳过渲染
```

输出包含：启动开销、常驻/峰值内存、每个规模的每步耗时与「体·步/秒」吞吐、
轨迹采样开销、单帧渲染耗时。它**不需要 GPU、不需要网络、不写任何文件**，
所以在 CI 和容器里都能跑。

测量方法很重要：这台机器的绝对数字在会话之间会漂 ±10%，所以**前后对比必须配对、
多次、取最小**。仓库里带了一个小工具做这件事（目前只在本地手动跑，CI **尚未**接入它——
`.github/workflows/ci.yml` 里没有 `ab_bench` 这一步）：

```bash
tools/ab_bench.sh 基线 stack 3              # 同一会话内跑 3 次取最小
tools/ab_bench.sh 实验 stack 3 F3D_X=1      # 带环境变量再跑一遍，直接对比
AB_BODIES=1000 AB_STEPS=200 tools/ab_bench.sh 小场景 scatter 3
```

物理侧还有一个更细的基准（带分配计数与分配点溯源）：

```bash
cargo run --release -p quench3d-physics --example bench
cargo run --release -p quench3d-physics --example bench -- --layout stack --no-sleep
cargo run --release -p quench3d-physics --example bench -- --who-allocates   # 打印分配点调用栈
cargo run --release -p quench3d-physics --example bench -- --phases         # 阶段分解（引擎内计时）
```

### 8.2 实测（本机）

吞吐（单层平铺、关闭睡眠 = 全部一直在算，最能反映扩展性）：

| 刚体数 | 每步 | 步/秒 | 体·步/秒 | 每步堆分配 |
| --- | --- | --- | --- | --- |
| 100 | 0.95 ms | 1053 | 105 306 | 0.00 |
| 500 | 5.53 ms | 181 | 90 410 | 0.01 |
| 1000 | 11.27 ms | 89 | 88 759 | 0.03 |
| 2000 | 22.21 ms | 45 | 90 051 | 0.06 |

接触图密集的深堆（逐层堆叠、关闭睡眠，最坏情况）：
200 体 1.44 ms/步 · 500 体 5.20 ms/步 · 1000 体 11.71 ms/步 · 2000 体 34.3 ms/步。

稳定与省资源：

| 项目 | 数值 |
| --- | --- |
| 稳态每步堆分配 | **0 次**（2000 体场景也一样，由回归测试钉住） |
| 常驻内存（2000 体） | 约 4 MB（进程启动约 2.4 MB） |
| 长跑 500 体 × 20000 步 | 无 NaN、无内存增长，活动内存变化 < 1 MB |
| CLI 启动开销 | 约 0.04 ms（进程启动到开始测量） |
| 轨迹采样 600 步 × 5 物体 | 2404 个采样点，开销 +5%（相对不开轨迹） |
| 单帧渲染（960×600，软件光栅） | 约 1.06 s（有独显时快一到两个数量级） |

### 8.3 做了哪些优化

优化前后跑同一套基准（300 步，逐层堆叠 + 启用睡眠）：

| 规模 | 优化前 | 优化后 | 分配/步（前 → 后） |
| --- | --- | --- | --- |
| 100 体 | 12.2 ms | **1.8 ms**（6.8×） | 83 → **0.00** |
| 500 体 | 416.6 ms | **237.2 ms**（1.8×） | 990 → **0.02** |
| 1000 体 | 5016 ms | 5091 ms | 11 898 → **0.04** |
| 2000 体 | 13 920 ms | 14 118 ms | 30 770 → **0.07** |

完全睡着的世界（500 体 × 20000 步，全部入睡后每步只做 O(n) 记账）：
**14.10 s → 1.13 s（12.5×）**。

关键改动都在物理热路径上：

1. **宽相网格换掉 `BTreeMap`**：原来每步为每个占据的单元分配/释放 `BTreeMap` 节点与
   内部 `Vec`，一个「全睡着」的 500 体场景每步要 480 次分配。现在用跨步复用的
   「排序后的单元键 + 区间 + 展平下标」（CSR 结构），并加了一条快速路径：
   没有任何清醒动态体时直接返回（`try_pair` 本来也会全部拒绝）。
2. **每个接触对不再分配**：`ContactManifold.points`、求解器的流形条目、窄相里的
   候选点/裁剪多边形缓冲，全部换成内联小向量 [`SmallVec`]（容量 4/8/12，
   超出才退化为堆，行为与 `Vec` 完全一致）。
3. **热启动缓存换掉 `BTreeMap`**：改成「按键稳定排序 + 区间 + 展平冲量」，
   查询用二分；键内顺序用序号保证与原来一致。
4. **每步缓冲复用**：刚体视图、材质表、关节条目/冲量表都挂在世界上跨步复用
   （`mem::take` + `clear`），不再每步重新分配 O(n) 内存。
5. **不稳定排序**：热路径上的 `sort_by`（稳定排序会分配临时缓冲）换成
   `sort_unstable_by` + 序号兜底，顺序语义不变、分配为零。

### 8.4 第二轮：世界逆惯量预计算

第一轮把「每步分配」打到零之后，深堆场景的时间几乎没变——说明瓶颈不在分配器。
于是给引擎加了**可开关的阶段计时**（`PhysicsWorld::set_phase_timing(true)`，
默认关闭时连一次 `Instant::now()` 都不调用，且不影响物理结果），
在同一份真实状态上分段取时钟，得到 2000 体深堆（关闭睡眠、200 步窗口、只统计子步本身）
每步 28.6 ms 的构成：

| 阶段 | 2000 体深堆 | 占比 | 2000 体平铺 | 占比 |
| --- | --- | --- | --- | --- |
| 宽相（代理 + 空间哈希候选对） | 10.50 ms | 36.7% | 5.93 ms | 32.4% |
| 速度迭代（含松弛） | 12.18 ms | 42.6% | 9.20 ms | 50.2% |
| 窄相（形状求交） | 2.38 ms | 8.3% | 0.71 ms | 3.9% |
| 准备 / 热启动 | 1.74 ms | 6.1% | 1.43 ms | 7.8% |
| 位置迭代 | 1.37 ms | 4.8% | 0.66 ms | 3.6% |
| 速度积分 / 写回 / 记账 | 0.41 ms | 1.4% | 0.39 ms | 2.1% |

> 为什么不靠「改配置再差分」来拆阶段：**改迭代次数会改变模拟状态本身**。
> 把 `position_iterations` 设成 0，箱子会互相陷进去、穿透越深接触流形越大，
> 窄相反而更贵——实测 2000 体深堆在位置修正关闭时每步 38 ms，
> 比完整步的 34 ms 还慢，差分出来「每个速度迭代」是负数。这种数字不能用。

按数据做的这一轮优化是**世界逆惯量预计算**：之前 `inv_i_world` 每次调用都要
「四元数逆旋转 → 乘本地对角 → 四元数正旋转」（约 60 次浮点运算），
而求解器里每个接触每次迭代都会对两侧各调用一次，深堆一步十几万次。
现在每个刚体每步只算一次 `R·diag(I⁻¹)·Rᵀ` 矩阵，之后就是一次 3×3 乘向量（约 15 次）：

| 深堆规模 | 优化前 | 优化后 | 提升 |
| --- | --- | --- | --- |
| 200 体 | 1.86 ms/步 | **1.46 ms/步** | 1.27× |
| 500 体 | 7.27 ms/步 | **5.20 ms/步** | 1.40× |
| 1000 体 | 16.89 ms/步 | **11.71 ms/步** | 1.44× |
| 2000 体 | 37.71 ms/步 | **34.29 ms/步** | 1.10× |

> 上表全部来自同一套吞吐基准（300 步、关闭睡眠），前后窗口一致。
> 2000 体深堆的收益最小（10%），因为它同时受宽相与求解器支配；
> 中段规模（500~1000 体）收益最大（40%+）。

这个改写**不是逐位等价**的（两次四元数旋转 vs 一次矩阵乘法，浮点舍入路径不同），
所以先用单元测试钉住数学等价：对 500 组四元数/惯量/向量采样，两种写法的最大相对偏差
**1.03e-6**（见 `solver.rs::world_inv_inertia_tests`）。

### 8.5 第三轮：宽相换成开放寻址哈希查找

阶段分解说宽相占 32~37%，那就动它。原来的实现是「排序后的单元键 + 对 13 个前向邻居
逐个二分查找」，2000 体一步要做十几万次二分。现在换成**直接映射的开放寻址哈希表**：

* 键 → 单元下标用整数混洗哈希 + 线性探测；
* 时间戳代替每步清零（`stamps/slots` 合并成一个 `u64` 数组：高 32 位时间戳、
  低 32 位下标 + 1），每步只写新槽位，不重扫整张表；
* 缓冲跨步复用，稳态仍然零分配；
* 哈希不用随机种子 → 跨运行可复现。

**这个改动是逐位保行为的**，证据有两条：

1. `golden_digest_is_stable` 的 64 位摘要**没变**（还是 `0xbcc0bccf037b867f`）——
   候选对集合与顺序完全一样（单元集合一样，输出照样排序去重）；
2. 新增暴力对照测试 `pairs_match_brute_force_on_a_dense_scene`：400 个代理（含静态、
   睡眠、全局）的宽相结果必须**恰好等于** O(n²) 暴力枚举出的 AABB 相交集合。
   以后不管怎么改网格/查找，漏检多检都会被抓住。

实测（2000 体，同一次会话内 3 次取最小，平铺布局）：
每步 **15.91 ms**、宽相 **3.23 ms**（上一轮同场景宽相约 5.9 ms）。

> **测量方法上踩过的坑（写下来免得再犯）**：这台机器上绝对数字在会话之间会漂 ±10%，
> 单次运行前后对比得出的结论可能是错的。我第一次测完哈希版本，看到深堆从 34.3 变成
> 36.1 ms，差点判定「哈希更慢」并回滚；改成**同一次会话内的配对 A/B + 取 3 次最小**
> 之后，结论反过来：深堆 36.4（哈希）vs 38.6（二分）ms/步，哈希是更快的。
> 所以：阶段分解表用来看「哪一块最大」这种量级信息（可信），
> 前后对比一律用配对、多次、取最小。

**下一步该动哪里**：宽相降下来之后，速度迭代成了最大块（平铺 58%、深堆 40%）。
完整设计稿（数据结构、为什么并行不改变结果、休眠语义、验证清单）在
[`docs/island_parallel_plan.md`](docs/island_parallel_plan.md)。
稿子里的三步**已经全部落地**：按接触/关节图把刚体切成互不相交的连通分量、
每个岛内部保持原来的顺序求解（结果不变）、岛与岛之间用 `std::thread` 并行
（wasm 走单线程路径）——因为岛之间不共享刚体，并行不改变任何结果，做到了逐位可复现
（§8.6 / §8.7 / §8.8 就是这三步的实现与实测）。另外宽相的**单元边长倍率**
（现在是中位数的 2 倍）是个还没做过配对 A/B 的旋钮，留给下一轮测。

### 8.6 island 划分：先看清场景的连通结构

求解器是顺序冲量（Gauss-Seidel），**结果依赖接触的处理顺序**，所以「按刚体对重排接触」
这类看似无害的优化会改变物理结果。但把刚体按「接触 + 关节」的连通性切成 island 之后，
有一个关键性质：**岛与岛之间不共享清醒刚体**，所以岛内保持原顺序、岛间并行，
结果与串行逐位相同（静态体与睡着的体是只读的，可以同时出现在多个岛的接触里，
所以「所有箱子压在同一块地板上」不会把全世界并成一个岛）。

这一轮先把**划分**做出来并接到公开 API 上（`PhysicsWorld::island_stats()`、
`island_count()`、`island_bodies()`、`island_of_body()`），并联调进 `--phases` 打印。
它本身就是给使用者和 agent 的诊断信息：**这个场景能不能靠并行变快**。

实测（`bench --phases`，2000 体，关闭睡眠）：

| 布局 | island 数 | 最大岛 | 多体岛 | 判断 |
| --- | --- | --- | --- | --- |
| 平铺（散放） | 2000 | 1 体 | 0 | 单体岛，并行没有意义 |
| 深堆（20 层） | **125** | **110 体** | **125** | 并行友好 |

第三条是个反直觉的发现：**2000 体的深堆不是「一个巨大的岛」，而是 125 根互不相干的柱子**
（每个箱子只和上下相邻的箱子接触，水平方向留着 0.22 的缝）。
所以并行求解恰好能帮到我们最需要的场景，而不是像原先估计的那样「深堆只能靠单线程硬算」。

划分本身的实现要点：并查集（按秩合并 + 迭代式路径压缩，避免深堆递归爆栈）、
岛编号按「岛内最小刚体下标」升序（与遍历顺序无关 → 可复现）、CSR 展平存储、
缓冲跨步复用（零分配回归测试仍然通过）。6 个单元测试覆盖：独立簇、静态/睡眠体不粘连、
关节跨接触合并、编号确定性、退化输入（自环/越界）。

### 8.7 island 并行求解：结果逐位不变

在划分之上做了 `std::thread` 并行求解（wasm 上是单线程路径，整块 `cfg` 掉）：

* 每个岛分到一份**本地**刚体视图（岛内清醒体 + 它接触到的冻结体副本），
  接触下标重映射到本地下标；岛的本地数组首尾相接、按工作线程分组，
  于是每个线程拿到的是**一段连续的可变切片**——全程安全 Rust，没有 `unsafe`；
* **岛内接触保持全局顺序**（分桶是稳定的），关节不参与并行
  （每个迭代本来就是「先全部接触、再全部关节」两趟）；
* 没有关节时（绝大多数纯刚体场景）每个线程一口气跑完自己那些岛的
  「全部速度迭代 → 松弛 → 位置修正」，每步只分发**一次**线程；
* 岛的分配用 LPT 贪心（接触多的岛先分给最闲的线程），分配本身也是确定性的。

**结果与串行逐位相同**，这不是嘴上说的，是两个自动化测试钉住的：
`parallel_solver_matches_sequential_bit_for_bit`（无关节快速路径）与
`parallel_solver_with_joints_is_also_bit_exact`（有关节的交错路径），
都断言并行路径**真的启用**（否则测试不算数）然后逐位比较位置/旋转/速度/睡眠标志。

实测（2000 体、300 步、关闭睡眠、本机 4 核，配对且 3 次取最小）：

| 布局 | 串行 | 并行（4 线程） | 提升 |
| --- | --- | --- | --- |
| 独立小堆（250 个岛 × 8 体） | 19.41 ms/步 | **11.86 ms/步** | **1.64×** |
| 深堆（岛结构随时间退化） | 36.56 ms/步 | **32.16 ms/步** | 1.14× |

> 为什么深堆只有 1.14×：它的岛结构**随时间变化**——刚倒下时是 125 根互不相干的柱子，
> 抖动/滑移之后逐渐并成一个岛，并行收益也就跟着消失。这也说明
> `island_stats()` 是个有用的诊断：调它就知道一个场景此刻值不值得并行。
> 另外为了能真正量出并行收益，基准里加了第三种布局 `--layout clusters`
> （大量互不相干的独立小堆），因为原先的平铺/深堆一个全是单体岛、一个是单一大岛，
> 都量不出这块。

并行不是白拿的：`std::thread::scope` 的线程分发本身要分配。所以零分配回归测试
现在是两条：**串行求解器热路径仍然是「恰好 0 次分配」**，
**并行路径守「分配有界且活动内存不跨步增长」**——后者正是为了抓
这次踩到的一个真 bug：布局缓冲一开始忘了每步清空，于是每步都往同一个 `Vec`
末尾追加，工作量变成 O(步数²)，一个 240 步的等价性测试从 0.5 秒变成 **36 秒**。
是「逐位相同」的等价性测试把这个 bug 暴露出来的。

### 8.8 整岛休眠与唤醒：不再有「半睡半醒的堆」

老规则是**逐体**计时：每个刚体自己慢够 0.5 秒就自己睡。毛病在于堆叠里下面几个
先睡、被当成无限质量冻住，接触力传不下去，上面还在抖的继续往里渗。

新规则（`update_sleeping` 现在拿到 island 划分）：

* 逐体累计「慢」的时间，但**不直接入睡**；
* 岛内**所有**成员都慢够了，整岛一起入睡；
* 睡着的一摞被推动时整岛一起醒（唤醒沿接触传播，1~5 步内传遍）。

这个改动**会改变模拟结果**（入睡时机变了），所以黄金摘要与 60 步参考表都
重新标定过（`0xbcc0bccf037b867f` → `0x70261b1f4c5257e5`）。重标定的工具是现成的：

```bash
cargo test -p quench3d-physics --lib print_reference_table -- --ignored --nocapture
```

**它到底好在哪，实测说清楚**（500 体堆叠、本机 4 核）：

| | 逐体计时（旧） | 整岛计时（新） |
| --- | --- | --- |
| 5 箱高塔：各箱入睡步 | 第 39 步 / 第 171 步（**132 步半睡半醒**） | **四个箱子同一步入睡** |
| 5 箱高塔：900 步之后 | **仍有箱子没睡着** | 全部睡着且不再醒 |
| 500 体宽阔堆叠：整场入睡 | 第 62 步 | **第 38 步**（结算耗时 0.25 s → 0.14 s） |
| 500 体入睡后稳态 | 0.0378 ms/步 | 0.0391 ms/步（噪声内相同） |
| 全睡 vs 一直算（500 体） | — | **0.039 vs 4.07 ms/步（约 100×）** |

结论要说实话：整岛计时的收益**主要在「正确」和「结算」**——高塔那类场景旧规则
根本睡不下来（900 步后还有醒着的），新规则同步入睡、结算快约 1.7×；
而**一旦都睡着了，稳态成本两者一样**（都是 ~0.039 ms/步，只剩 O(n) 记账）。
所以它不是稳态优化，而是把「半睡半醒的堆」这种不稳定状态直接消灭。
「入睡后比一直算快约 100×」那个倍数是**睡眠本身**带来的，不是整岛计时带来的。

两个测试直接钉住这两条语义：`stacked_bodies_fall_asleep_in_the_same_step`
（同步入睡、睡着后不来回醒）与 `sleeping_island_wakes_as_a_unit`（整岛唤醒）。
它们**确实能抓到旧行为**：把 `update_sleeping` 临时换回逐体规则，两个测试都失败
（第 3 个箱子第 171 步才睡；900 步后整摞仍有没睡的），换回来立刻通过。

### 8.9 怎么保证没有改坏物理

* **先承认一件事**：长时间跨度的接触仿真**不可能**逐位锁死。同一套场景，在第 0 步把
  某个刚体的速度加上 1e-7（和「等价重写带来的浮点差异」同量级），400 步后位置偏差会
  放大到 **3.4~7.8 米**（实测曲线：11 步 1.4e-8 → 51 步 1.1e-2 → 201 步 1.4e-1 → 400 步 3.4e0）。
  任何改动浮点运算顺序的重构都会分道扬镳，所以判据必须分两层：
* **短跨度紧容差**（`sixty_step_states_match_reference`）：同一套覆盖盒/球/胶囊/圆柱/
  凸包/三角网格 + 铰链/距离关节的场景跑 60 步，与参考终态逐体比对，容差 1e-5。
  这一层能抓住真正的数学错误。
* **长跨度不变量**（`long_run_keeps_physical_invariants`）：跑 400 步，只查物理不变量——
  状态有限、速度有界、不穿地、该静止的确实静止。
* **摘要警报**（`golden_digest_is_stable`）：400 步状态的 64 位摘要写死在测试里。
  它不证明「行为没变」（上面说了不可能），它的作用是「**变了就必须来人看**」——
  摘要一变，CI 就红，改动者必须解释清楚并按流程更新。
* **零分配回归测试**：`crates/quench3d-cli/tests/steady_state_allocations.rs` 用计数分配器
  断言预热后 60 步里堆分配为 0、活动内存不增长。谁再往热路径里塞 `Vec::new()` /
  `collect()` / 字符串格式化，CI 就会失败。
* **规模与参数上限**：场景物体数上限 20000、单次 `simulate` 步数上限 100 万、
  时间步长必须在 `(0, 1]` 秒内、渲染单边不超过 8192 且总像素不超过 4096×4096。
  超了都给带路径的诊断（退出码 2），而不是跑到内存耗尽、或者返回一个
  「其实什么都没算」的成功报告。

## 9. 已知限制

* 物理：半隐式欧拉积分（自由落体 1 秒位置误差约 1.6%）；CCD 是**中心射线 TOI**
  （每子步从刚体中心沿这一步的位移方向打一条射线，命中就把朝表面的法向位移截到
  `命中距离 - 中心到表面的距离`）——不是精确 TOI、不做旋转扫掠、只扫中心线
  （擦着角过去可能漏），极快 + 极薄也仍可能漏；
  圆柱/凸包之间走 GJK+EPA 单点流形，静置稳定性弱于盒-盒；
  island 划分 + 整岛休眠/唤醒 + 多岛 `std::thread` 并行求解都已具备，
  但**关节不参与并行**（每个迭代里接触并行、关节串行），wasm 上是单线程路径；
  `TriMesh↔TriMesh`、`Plane↔Plane` 不产生接触。
  `Joint::Fixed` 的角约束是速度级的：焊接的是**创建时**的相对姿态，不是参考姿态。
* 渲染：单张阴影贴图（大场景需要更小的 `shadow_radius` 或后续加 CSM）；
  有 HDR 中间目标 + bloom + FXAA + SSAO，但**还没有**景深 / TAA / AO 去噪（AO 是 16 抽样
  交错梯度噪声，接触处仍有轻微颗粒）、没有骨骼动画、没有延迟管线；
  Alpha 混合物体不写深度（后处理链已经能把混合放到 tonemap 之前，见 §4）。
* 工程：窗口截图需要能创建 GPU 设备；无头验证依赖软件光栅（lavapipe）时较慢。
* Web：没有 WebGPU 适配器的浏览器走 WebGL2（先探再定，见 §2 的「后端怎么选」）。
  本机只在 headless Chrome + SwiftShader 上验过 **WebGL2** 这条路（出第一帧、
  `EXT_color_buffer_float`/`EXT_float_blend`/MSAA 4 都在）；**真实 GPU 浏览器上的
  WebGPU 路径没验过**，遇到问题用 `?backend=gl` 强制回退再排查。
* 性能：求解器是顺序冲量，无 SIMD 特化；多岛可并行（4 核上独立小堆 1.64×），
  但单一大岛无法并行；深堆（十几层箱子）在 2000 体时约 32 ms/步，达不到实时——开阔场景（接触数与刚体数同阶）则可线性扩展到
  每秒约 9~10 万「刚体·步」。当前最大的单一块是宽相（32~37%），
  其次是速度迭代（43~50%）；两者都已优化过一轮（宽相换开放寻址哈希见 §8.5、
  多岛并行求解见 §8.7），下一步是宽相单元边长倍率这类还没做过配对 A/B 的旋钮。
  渲染在软件光栅下约 1 秒/帧，有独显时才有实用帧率。
* CLI / 大模型：`ask` 需要有一个正在运行的 OpenAI 兼容服务（本地 Ollama / llama.cpp /
  LM Studio / vLLM，或云端）；`https://` 端点依赖系统 `curl`。模型输出质量取决于所选模型，
  本仓库的测试用假服务覆盖协议与校验，不评价模型本身。
  场景语法目前只覆盖图元 + 外部模型 + 关节，没有粒子/地形/程序化材质。
* 编辑器：**已有拖拽式变换 gizmo、撤销/重做与父子层级**（见 §1.1），但
  **手柄不支持拖拽中键入数值、不支持轴心切换、不支持多选成组变换**（多选还没做；
  改父只能在下拉框里选，场景树里还不支持拖放改父；
  现在"成组"的来源是父子层级与导入的模型实例）；
  没有原生文件对话框（桌面端用路径输入框，避免为此引入 `rfd` 这类依赖）；
  打开/保存是**整场景替换**（不做增量合并）。
  导入的模型（glTF/OBJ）已经接进场景树：可整组选中、可聚焦、可在视口里实时拖动并写回
  `models[i]`（见 §1.1），但**一个模型只有一个变换** —— 不能单独挪模型里的某个子网格，
  也不能把非均匀缩放写回格式（`models[i].scale` 是单个浮点数）。
* 编辑器能改**光照**（场景级）与**材质**（基础色/金属度/粗糙度/自发光 + 四个贴图槽 +
  UV 平铺/法线强度/alpha 裁剪与混合/无光照/双面），改不了的是材质节点图 / 自定义着色器，
  也没有贴图 UV 的可视化编辑（贴图按**路径**指定：原生端可从磁盘应用，Web 端写进场景文件）。
  编辑器不建模的段**不再会丢**：导出现在以"最近载入的原文档"为底稿，材质按同名物体继承贴图、
  UV 与开关，`lighting`/`simulate`/`render`/`models`/`probes` 也原样保留
  （以前这些会被静默清零，是个真 bug，已修并有测试钉住）。
* Web 客户端：wasm 包 11 MB（见 §2），首次加载需要点时间；`web/pkg` 是按
  **固定 example 名**（`web_client`）打包的，页面 `import` 的符号必须与之一致
  （`tools/check_web.mjs` 会静态核对）。
* MCP：只实现 stdio 传输与工具（无 resources / prompts / HTTP SSE）。
* 渲染后处理：HDR 中间目标 + `post.wgsl` + bloom + FXAA + SSAO 都在；景深 / TAA 还没接，
  详见 [gap_analysis](docs/gap_analysis.md) P1 #25。
* SSAO 是**深度-only**、无去噪：没有法线缓冲（法线由深度重建），也没有单独的 AO 模糊
  （AO 直接在后处理里算完就用）。表现是接触处有轻微颗粒（16 抽样 + 交错梯度噪声），
  以及掠射角下的平坦地面可能暗 1~2 个色阶；要更干净需要加一张 AO 目标 + 双边模糊。
* SSAO 需要采样深度，所以深度格式是 `Depth32Float`（`Depth24Plus` 不可采样）。
* `simulate` / `render` / `movie` 的报告里，`sensor: true` 的物体会以 `sensors[]` 出现
  （每个进过它的刚体一条：`enter_step` / `exit_step` / `steps_inside` / `entries`）。
* 物理的碰撞过滤/传感器/CCD 都有**边界**：过滤是**对称**的（`A` 愿意碰 `B` 且 `B`
  也愿意才碰）、只作用于**物理**（射线/拾取/`overlap` 查询不看分组）；
  传感器**不唤醒睡眠体**（巨大的触发区不该叫醒整个世界），事件是**每步轮询** `sensor_events()`
  或事件总线（`drain_events()`），**没有回调/订阅**，也不保证"睡眠体从里面被搬走"能报离开；
  CCD 是"中心射线 + 只截**法向**位移"（切向保留，所以沿墙滑行不受影响），
  没有旋转扫掠、只扫中心线，动态×动态只有"两边都开才互相拦"的语义，
  极快 + 极薄（一步能跨过整个物体）仍可能漏。
* **JSON Schema 的边界**：它是**手写**的（加了一致性测试，不引 `schemars` 这类依赖），
  值类型宽松（多为"任意"）、不含 `format`/`minimum` 等语义约束；不支持外部 `$ref`、
  不生成 draft-07 版本；编辑器补全要靠它自己解析 `$defs`。
* **模板/实例的边界**：没有**外部场景引用**（跨文件 include 组装，与资产搜索路径同源）；
  模板不能嵌套模板；编辑器能装载、但**存回时把 `instances` 摊平**成具体物体
  （行为与数量不变，文件变长——有意取舍，测试钉住）；`instances` 只覆盖位置/朝向/缩放/名字。
* **复合形状的边界**：部件没有自己的摩擦/恢复/传感器/CCD（都是刚体级）；部件不能是三角网
  （凸包也只能当整个刚体用）；编辑器没有部件编辑面板——能原样往返，但界面上既不能增删部件、
  也看不到部件的选择框；接触点每对刚体最多 8 个；惯量是对角近似（没有惯性积）。
* 编辑器**没有**事件面板/时间线、也不能清空事件队列（状态栏只读显示队列深度、丢弃数与
  最近一条事件）；**没有**碰撞层/传感器/CCD 的编辑界面（面板里还看不到），但它不会吃掉这些字段：
  导出时从物理刚体上读回来（有往返用例钉住）。
* 查询的层过滤是**单向**的（`QueryFilter::mask` / `--mask` 只看被查方在不在掩码里），
  与物理碰撞的对称判据**故意不同**：查询是"我问你答"，只有提问方有立场。
  另外 `QueryFilter::sensors` 缺省为 `true`（触发区也是几何体），想排除要显式关掉。
* `QueryIndex` 是**快照**：它只在"每帧重建一次、帧内查很多次"这个用法下才划算；
  世界一动（`add_body` / `remove_body` / `body_mut` / `step` 都会 +1 版本号）它就过期，
  此时查询**自动回退**到线性扫描（结果永远一致、不会有静默错答），
  `is_stale()` / CLI 报告里的 `fallback` 是给你看的信号。
  加速结构只对**射线**与**重叠**生效；`shape_cast`（保守推进）仍然线性。
* `quench3d query` 一次只做一次查询，且默认走线性扫描（`--index` 才建索引）：
  单次查询建索引是净亏（建表 O(N) 比扫描还贵）。要省就一次进程里批量查——
  目前 CLI 不提供批量查询，MCP/引擎 API 里可以自己循环。
* FXAA 是 console/低质量档（没有子像素锯齿消除），且在**线性**空间混合、只在判断边缘时
  转成 sRGB 亮度——和 FXAA 原版"整条都在 gamma 空间"略有差别，见 `fxaa.wgsl` 顶部注释。

## 10. 设计取舍

* **物理不用第三方库**：整个求解器（宽相空间哈希、GJK/EPA、顺序冲量 + Baumgarte、
  热启动、关节、角色控制器）都在 `quench3d-physics` 里，只有数学依赖，便于阅读与改造。
* **门面层薄**：`quench3d` 只做装配（GPU + 场景 + 物理 + 输入 + 相机 + 帧循环），
  想要更细的控制可以直接用下面三层中的任意一层，或只用 `quench3d-render`。
* **Web 与桌面同构**：同一份 `Game`、同一套 WGSL、同一套 API；
  差异只有入口（`run` / `run_web`）与截图落盘方式。
* **agent 优先的 CLI**：场景 JSON 是唯一的输入契约，物理与渲染分成两条路径
  （`simulate` 不需要 GPU，几十毫秒返回；`render` 才建 GPU 上下文），
  所有命令共用一套「单行 JSON + 固定退出码 + 带路径诊断」的输出约定。
* **CLI 只多两个依赖**：引擎四个 crate 保持极轻（数学库零依赖），
  只有 CLI 引入 `serde` / `serde_json` 来承载 JSON 契约，HTTP 客户端与 base64 都是手写的。
* **用内存换分配次数**：热路径上的缓冲一律跨步复用而不是每步新建，代价是进程常驻
  内存会停在峰值水平（2000 体约 4 MB），换来的是稳态零分配、没有 allocator 抖动、
  也不会有「跑久了内存碎片化」的问题。物理结果用黄金摘要逐位锁定，只降开销不改行为。

## 11. 许可证

本项目按 **MIT OR Apache-2.0** 双许可发布（与 Rust 生态的惯例一致），**你可以任选其一**：

* [`LICENSE-MIT`](LICENSE-MIT) —— MIT 许可证全文；
* [`LICENSE-APACHE`](LICENSE-APACHE) —— Apache License 2.0 全文（含附录里的版权与许可样板）。

版权署名统一为 `Copyright (c) 2026 Quench3D contributors`：项目目前既没有公开仓库，
也没有正式的作者名单，所以**不写任何真人姓名**（署名写错比不写更难纠正）。

按 Apache-2.0 第 5 条的约定：除非你明确声明「不是贡献」，否则任何有意提交并被本项目
收录的贡献，都同时按上述两份许可证授权，不附加任何额外条款。

对应地，各 crate 的 `Cargo.toml` 里 `license = "MIT OR Apache-2.0"`，
`license-file` 不设置（双许可用 SPDX 表达式 + 两份文件表达，而不是单一 `license-file`）。
