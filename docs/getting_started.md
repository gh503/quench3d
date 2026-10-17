# Quench3D 开发指南（从零到能改）

> 这份文档假设你已经能跑起来。没跑起来先看 [README](../README.md) §2 与 §3。
> 想直接看代码：先打开 [`crates/quench3d/examples/starter_game.rs`](../crates/quench3d/examples/starter_game.rs)
> ——那是本指南的「可执行版本」，一个能跑、能改、能进 CI 的完整骨架。

---

## 1. 心智模型：一帧里发生了什么

```
每帧（可变步长 dt）：
  input.begin_frame()
  update_camera(dt)                 ← 轨道/飞行相机、鼠标键盘
  Game::update(engine, dt)          ← 【你写】输入、动画、HUD、游戏逻辑
  while 累加器 >= fixed_dt:          ← 最多追 8 步，防「卡顿后炸开」
      Game::fixed_update(engine, fixed_dt)   ← 【你写】物理步进 + 确定性逻辑
  scene.sync_from_physics(world)    ← 引擎自动把刚体位姿同步给场景对象
  Game::draw(engine)                ← 【你写】自定义绘制（调试线、UI 几何）
  renderer.draw_frame(...)          ← 渲染、阴影、后处理
```

三条由此推出的规矩（**踩过就知道疼**）：

1. **物理只在 `fixed_update` 里 `world.step(dt)`。** 放 `update` 里会导致物理步长随帧率抖动，
   同一场景两次运行结果不同，回放/测试全部失效。
2. **不要自己同步位姿。** 用 [`Scene::add_with_body`] 把刚体和场景对象绑上，
   引擎每帧调 [`Scene::sync_from_physics`]。只有离屏渲染那种没有帧循环的场景才要手动调一次。
3. **`init` 只跑一次。** 重置场景要清空（`world.clear()` + `scene.clear()`）再重建，
   而重建逻辑应该和 `init` 共用同一个函数（模板里的 `build_scene`），否则两条路径迟早不一致。

引擎给你的一切都在 `Engine` 的公开字段上：`gpu` `renderer` `assets` `scene` `world`
`input` `orbit` `fly` `debug` `time` `frame_count` `stats` `paused` `should_exit`。

---

## 2. 三条开发路线，按你的目标挑

| 路线 | 适合 | 入口 | 产物 |
| --- | --- | --- | --- |
| **A. Rust 代码** | 真做产品/游戏、要写逻辑 | `Game` trait + `quench3d::run` / `run_web` | 桌面二进制 + Web 页面 |
| **B. 场景 JSON + CLI** | 快速试参数、给别人复现、非程序员调参 | `quench3d render/simulate/validate` | PNG + JSON 报告 |
| **C. Agent 自动化** | 让大模型/脚本自己搭场景、跑验证 | `quench3d ask/chat/mcp` | 场景 JSON + 渲染 + 诊断 |

三条路线**共享同一套引擎与物理**，所以可以混用：用 C 生成场景 JSON，
用 B 跑基准和截图，把定稿的场景用 A 变成产品。

### A. Rust 代码（主线）

最小可运行版本（完整的见 `starter_game.rs`）：

```rust
use quench3d::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    quench3d::run(
        EngineConfig { title: "我的场景".into(), width: 1280, height: 720, ..Default::default() },
        App::new(
            |engine: &mut Engine| {                       // 只跑一次
                let mesh = engine.assets.unit_cube;
                let mat = engine.assets.solid_material(
                    &mut engine.renderer, &engine.gpu,
                    Vec4::new(0.9, 0.35, 0.2, 1.0), 0.1, 0.4);
                engine.scene.add_named(mesh, mat, Transform::IDENTITY, "立方体");
                engine.focus_scene();
            },
            |engine: &mut Engine, dt: f32| {              // 每帧
                engine.world.step(dt);                    // 简单场景可以这样；正式写法用 fixed_update
            },
        ),
    )
}
```

要写逻辑复杂一点，就实现 `Game` 的四个钩子（全部有默认实现，按需覆盖）：

```rust
impl Game for MyGame {
    fn init(&mut self, engine: &mut Engine) {}                          // 建场景
    fn update(&mut self, engine: &mut Engine, dt: f32) {}               // 输入/HUD/动画
    fn fixed_update(&mut self, engine: &mut Engine, dt: f32) {          // 物理
        engine.world.step(dt);
    }
    fn draw(&mut self, engine: &mut Engine) {}                          // 追加绘制
}
```

跑起来：

```bash
cargo run --release --example starter_game        # 桌面窗口
cargo run --release --example starter_game -- --headless out.png   # 不开窗口，出 PNG
```

### B. 场景 JSON + CLI

```bash
quench3d example --out scene.json      # 生成一个可跑的示例场景
quench3d validate scene.json           # 校验，返回带路径的诊断
quench3d simulate scene.json --json    # 只跑物理，输出每个刚体的位置/速度/位移/休眠
quench3d render scene.json --out a.png --width 1280 --height 720
quench3d simulate scene.json --render b.png --steps 240 --trace 箱子0
quench3d movie scene.json --frames 300 --frames-dir frames   # 录 5 秒仿真视频（PNG 序列 + trace）
quench3d doctor                        # 环境自检：平台/CPU/GPU/后端
quench3d schema                        # 能力清单（agent 的自描述入口）
```

`--json` 给的是**单行 JSON 信封**，专门给自动化消费；不带 `--json` 是人读的报告。

> `movie` 缺省产出帧序列（`frames/frame_00000.png…`）和 `frames/trace.jsonl`，
> 编码交给 ffmpeg（报告里的 `data.ffmpeg` 直接给出命令）。帧数由物理步决定而不是墙钟，
> 所以视频时长严格等于仿真时间，同一场景两次录制逐像素一致（`data.digest` 可断言）。
> 不想装 ffmpeg 就用内置编码器直接出单文件：
>
> ```bash
> quench3d movie scene.json --frames 60 --encoder gif  --out demo.gif   # 256 色，短循环演示
> quench3d movie scene.json --frames 60 --encoder apng --out demo.png   # 无损
> ```
>
> 换编码器不改变 `data.digest`（像素没变，只是封装变了）；GIF 的帧延迟粒度是 10 ms，
> 被量化时 `data.notes` 会直接把"总时长从多少变成多少"算出来。

### C. Agent / 大模型

```bash
quench3d ask "做一个 3x3x3 的箱子塔，中间放个球，掉落测试"   # 生成场景 JSON，可接着渲染/仿真
quench3d chat                                            # 命令行 agent，模型自己调工具
quench3d mcp                                             # MCP 服务器（stdio），给外部 agent 用
```

大模型走 OpenAI 兼容接口，**本地优先**：`ollama serve && ollama pull qwen2.5:7b`。
仓库里没有真实本地模型环境时，测试用的是 in-process 的 mock（见 README 的诚实说明）。

---

## 3. 配方表（复制粘贴级）

### 3.1 加载外部模型（glTF / GLB / OBJ）

```rust
// 一步：加载 + 摆位，返回新加进场景的对象下标
let objects = engine.load_model("assets/robot.glb", Transform::new(
    Vec3::new(0.0, 0.0, -3.0), Quat::IDENTITY, Vec3::splat(1.0)))?;
println!("加载了 {} 个网格对象", objects.len());

// 想让它同时当**静态碰撞体**（地形/建筑，动态刚体能落在上面）：
//   engine.load_model_as_static(path, transform, friction) -> (Vec<usize>, BodyHandle)
// 只解析不开窗口（prelude 已导出裸名，也可写全路径）：
//   quench3d::assets::load_model / load_gltf / load_obj
```

模型自带的材质/贴图会用 `AssetCache` 缓存，重复加载同一文件不会重复上传 GPU。

### 3.2 刚体：形状、质量、材质参数

```rust
use quench3d::prelude::*;

let body = engine.world.add_builder(
    BodyBuilder::dynamic(Shape::Box { half_extents: Vec3::splat(0.5) })
        .position(Vec3::new(0.0, 5.0, 0.0))
        .rotation(Quat::from_euler_yxz(0.3, 0.0, 0.0))
        .linear_velocity(Vec3::new(1.0, 0.0, 0.0))
        .friction(0.6)            // 摩擦
        .restitution(0.2)         // 弹性
        .density(800.0)           // 或用 .mass(2.0) 直接指定质量
        .linear_damping(0.05)
);
// 静态：BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
// 运动学（自己推位置、能推动别人）：BodyBuilder::kinematic(shape)
```

可用形状：`Sphere{radius}` `Box{half_extents}` `Capsule{half_height,radius}`
`Cylinder{half_height,radius}` `Cone{half_height,radius}` `ConvexHull{points}` `Plane{normal}`
`TriMesh{vertices,indices}`（静态地形/关卡）。

场景格式里还多一种**格式层**形状 `heightfield`（`heights` 行主序 + `cell_size`）：
装载时展开成静态 `TriMesh`，渲染与物理共用 `MeshData::heightfield` 生成的同一份几何。

### 3.3 让物理推动画面（唯一正确的绑定方式）

```rust
let body = engine.world.add_builder(BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 }));
// 绑上：以后引擎每帧自动 sync_from_physics，你不用管
engine.scene.add_with_body(mesh, material, body, &engine.world, "球");
```

### 3.4 施加力、读状态、唤醒

```rust
if let Some(b) = engine.world.body_mut(body) {
    b.apply_impulse(Vec3::new(0.0, 8.0, 0.0));            // 冲量
    b.apply_impulse_at_point(impulse, world_point);        // 带力臂（会转）
    b.apply_force(force);                                  // 持续力，每步都要加
    let v = b.linear_velocity();
    if b.is_sleeping() { b.wake_up(); }                    // 睡着的刚体不会自己醒
    b.set_can_sleep(false);                                // 不让它睡（例如玩家角色）
}
```

**"第 2 秒推一下"和"有没有进过这个区域"不用写代码**：场景 JSON 里的 `actions[]` 与
`probes[]` 已经把它们变成声明式字段（`simulate` / `render` / `movie` 三条路径都生效）：

```json
{
  "actions": [
    { "type": "impulse", "target": "箱子", "at_time": 2.0, "value": [0, 800, 0] },
    { "type": "force", "target": "气球", "at_step": 30, "steps": 30, "value": [0, 0.5, 0] },
    { "type": "teleport", "target": "箱子", "at_step": 120, "position": [0, 4, 0] }
  ],
  "probes": [
    { "name": "落点", "shape": "sphere", "position": [0, 0.5, 0], "radius": 1.2 },
    { "name": "门口", "shape": "box", "position": [3, 1, 0], "size": [2, 2, 2], "watch": ["箱子"] }
  ]
}
```

报告里会长出 `probes[]`：每个被观测刚体的 `enter_step` / `exit_step` /
`steps_inside` / `entries` / `max_speed`。探针是**只读**的（不唤醒睡眠刚体、
不改变物理结果），判定用刚体的世界包围盒与区域求交——等价于：

```rust
// CLI 内部就是这么做的：`world_aabb()` 与区域求交，只读
for (handle, body) in engine.world.bodies() {
    let bb = body.world_aabb();                 // 半空间（地面）没有有限包围盒
    let speed = body.linear_velocity().length();
    // 球形区域：包围盒上离球心最近的点 ≤ 半径；盒形区域：两个 AABB 相交
}
```

**从内存字节加载模型**（Web 端与任何读不到文件系统的地方唯一的正路）：

```rust
// 桌面上也可以：字节可以来自网络、内嵌资源、zip 包……
let bytes = std::fs::read("chair.glb")?;
let model = quench3d::assets::load_model_from_bytes("chair.glb", &bytes)?;
for (mesh, material) in model.upload(&mut engine.renderer, &engine.gpu) {
    engine.scene.add(mesh, material, Transform::IDENTITY);
}
```

* `.glb` 自包含，任何来源的字节都能解析；`.gltf` 引用外部 `.bin`/贴图时内存里无从解析
  （报错会让你改用 `.glb`）；`.obj` 只解析几何（`mtllib` 不读，材质用缺省值）。
* Web（wasm）端 `load_model(path)` 会返回 `AssetError::Unsupported` 并**直接告诉你调
  `load_model_from_bytes`**，而不是报一个"找不到文件"——wasm 上没有文件系统，
  由 JS 侧 `fetch` 拿字节、交给引擎，是唯一可行的分工。

### 3.5 射线拾取（点击选中物体）

```rust
let mouse = engine.input.mouse_pos();
let camera = engine.camera();
let ray = camera.screen_ray(mouse, engine.viewport());   // Ray { origin, dir }
if let Some(hit) = engine.world.raycast(ray.origin, ray.dir, 1000.0) {
    println!("命中 {:?}，落点 {:?}，距离 {}，法线 {:?}",
        hit.handle, hit.point, hit.distance, hit.normal);
    // 想跳过某类物体：engine.world.raycast_filtered(origin, dir, max, |handle| ...)
}
```

### 3.6 相机

```rust
// 轨道相机（默认）：绕 focus 转，yaw=0 时相机在 focus 的 +Z 方向
engine.orbit.focus = Vec3::new(0.0, 2.0, 0.0);
engine.orbit.distance = 12.0;
engine.orbit.yaw = std::f32::consts::PI + 0.7;
engine.orbit.pitch = 0.35;          // >0 表示相机在焦点上方（俯视）

// 飞行相机（第一人称/自由视角）
engine.camera_mode = CameraMode::Fly;
engine.fly.position = Vec3::new(0.0, 1.7, 6.0);   // FlyCamera 是 position/yaw/pitch
engine.fly.yaw = 0.0;                             // 朝 -Z 看

engine.orbit.near = 0.05;           // 近/远裁剪面（深度冲突多就调大 near）
engine.orbit.far = 5000.0;
engine.orbit.orthographic_height = Some(8.0);  // Some(高度) = 切成正交投影，None = 透视（默认）

engine.focus_scene();               // 按场景包围盒自动取景

// 注意：光照方向默认 (0.42, -0.78, 0.46)（光往 +X/+Z 下方走），
// 相机站在 -X/-Z 一侧才看得到被照亮的面。
```

### 3.7 光照、天空、雾、曝光

```rust
let l = &mut engine.scene.lighting;
l.sun = Some(DirectionalLight {
    direction: Vec3::new(0.4, -0.8, 0.4).normalize_or_zero(),
    color: Vec3::new(1.0, 0.96, 0.9),
    intensity: 2.9,
    cast_shadows: true,
    shadow_radius: 28.0,
    shadow_map_size: 2048,
});
l.ambient_color = Vec3::new(0.40, 0.47, 0.62);
l.ambient_intensity = 0.40;
l.exposure = 1.0;
l.sky = SkyParams { /* 程序化天空 */ ..Default::default() };
l.fog = FogParams { /* 距离雾 */ ..Default::default() };
// 点光源（最多 MAX_POINT_LIGHTS 个）：
//   l.point_lights.push(PointLight::new(位置, 颜色, 强度, 半径));
// 注意：Rust 侧 `FrameUniforms.point_lights` 与 WGSL 的 `array<PointLight, 8>` 是
// **结构体数组**（pos0, color0, pos1, color1…），改动这两个结构体时必须逐字段对齐；
// `renderer.rs` 里的 `frame_uniform_layout_matches_wgsl` 会把偏移量钉死。
```

### 3.8 内置网格与材质

内置网格是 `AssetCache` 上的**字段**（单位尺寸，用 `Transform` 缩放来改大小）：
`unit_cube` `unit_sphere` `unit_cylinder` `unit_capsule` `unit_cone` `unit_plane`。
自己造网格用 `MeshData` + `engine.renderer.add_mesh(...)`；
加载外部模型见 3.1，缓存命中情况用 `engine.assets.cached_counts()` 看。

```rust
// 纯色（最常用）：基色、金属度、粗糙度
let mat = engine.assets.solid_material(&mut engine.renderer, &engine.gpu,
    Vec4::new(0.85, 0.45, 0.25, 1.0), 0.05, 0.45);

// 线性空间基色（想直接用 sRGB 十六进制值算颜色时更方便）
let mat2 = engine.assets.solid_material_linear(&mut engine.renderer, &engine.gpu,
    Vec4::new(0.85, 0.45, 0.25, 1.0), 0.0, 0.8);

// 完整描述（贴图、法线、自发光……）：注意第三个参数是**名字**，名字是缓存键
let mat3 = engine.assets.material(&mut engine.renderer, &engine.gpu, "我的材质",
    MaterialDesc { base_color: Vec4::splat(1.0), ..Default::default() });
```

### 3.9 截图、离屏渲染、CI

```rust
// 有窗口时：把当前帧存成 PNG（宽高可缩放）
engine.screenshot(1920, 1080, "shot.png")?;

// 无窗口：搭好场景渲染一张，CI 里也能跑（不需要显示器）
let image = quench3d::render_offscreen(1280, 720, &config, |engine| {
    engine.scene.add_named(mesh, mat, Transform::IDENTITY, "东西");
});
quench3d::write_png(&image, "out.png")?;
// 还想要「画了多少东西」：render_offscreen_stats(...) -> (ImageData, RenderStats)
```

### 3.10 调试可视化

```rust
engine.debug.axes = true;               // 世界坐标轴
engine.debug.physics_contacts = true;   // 接触点与法线
engine.debug.velocities = true;         // 速度矢量
engine.debug.body_bounds = true;        // 每个动态刚体的包围盒
engine.debug.enabled = false;           // 总开关

// 运行时长/帧率/暂停
println!("t={:.2}s frames={} fps={:.0}", engine.time, engine.frame_count, engine.fps);
engine.paused = true;                   // 暂停时不推进时间与物理
```

内置快捷键：`F3` 网格、`F4` 阴影、`F5` 接触点、`P` 暂停、`R` 对准场景、`Tab` 切换相机。

### 3.11 看性能花在哪

```rust
engine.world.set_phase_timing(true);
// ...跑若干步...
let t = engine.world.phase_timings();
println!("整步 {:.3} ms（宽相 {:.3} / 窄相 {:.3} / 速度迭代 {:.3} / 位置迭代 {:.3}）",
    t.total_us / 1000.0, t.broadphase_us / 1000.0,
    t.narrowphase_us / 1000.0, t.velocity_us / 1000.0, t.position_us / 1000.0);
println!("{} 步、最近一步 {} 个岛（最大 {}，多体岛 {}）",
    t.steps, t.islands, t.largest_island, t.multi_body_islands);
engine.world.set_phase_timing(false);
```

---

## 4. 迭代循环（每天要敲的命令）

```bash
# 单机开发
cargo run --release --example starter_game          # 跑自己的场景
cargo test --workspace                              # 285 个测试，几分钟内
cargo fmt --all && cargo clippy --workspace --all-targets

# 物理调参（不用开窗口，输出结构化报告）
cargo run -p quench3d-cli -- simulate scene.json --json --steps 600
cargo run -p quench3d-physics --example bench -- --bodies 500 --layout stack

# Web
tools/build_web.sh starter_game      # 编译 wasm + 生成 web/pkg + 写构建戳
node tools/check_web.mjs             # 产物检查（含时钟探针、新鲜度）
python3 -m http.server 8000 --directory web     # 然后打开 http://localhost:8000

# 出问题时的诊断
node tools/probe_start.mjs           # 在 Node 里重放 start()，打出真正的 panic 原因
```

`tools/check_web.mjs` 是新加的一道闸，它做四件事，都**踩过坑才会加**：

1. wasm 里有没有残留 `time not implemented on this platform` 这类必然 panic 的路径
   （并且会按符号名归因：只有 quench3d 自己的符号在用才算失败）；
2. 真在 Node 里**调用** `clock_probe()` 读一次时钟（旧代码会在这里就 panic）；
3. 页面 `import` 的符号胶水里都真的有；
4. 产物是不是比源码旧（靠 `web/pkg/build_stamp.txt` 里的 `example=` 精确判断）。

---

## 5. 坑清单（Web 端尤其看）

| 坑 | 症状 | 解法 |
| --- | --- | --- |
| **wasm 上没有 std 时间** | `std::time::Instant::now()` 在 `wasm32-unknown-unknown` 上**直接 panic**："time not implemented on this platform"，但 `cargo check --target wasm32` 查不出来 | 一律用 `quench3d::clock::now_seconds()`；物理阶段计时走可插拔时钟 `set_time_source()`（wasm 由门面自动装） |
| **WebGL2 的 limits** | 浏览器没有 WebGPU 时回退 WebGL2，若请求默认 limits，`request_device` 失败 → 空白页 | 引擎已按后端选档位并降档重试；自己写 GPU 初始化时记得 `Limits::downlevel_webgl2_defaults()` |
| **工具链不在 `~/.rustup`** | `can't find crate for 'core'` / "wasm32 target may not be installed" | 用 `tools/build_web.sh`（它会自动切到仓库内的工具链）；手敲 `cargo` 要先导出 `RUSTUP_HOME`/`CARGO_HOME` |
| **wasm 体积 4.6 MB** | 首屏加载慢；浏览器还会缓存旧包 | 这是 wgpu 双后端（WebGPU + WebGL2）的真实体积；发布时上 Brotli/gzip 与 CDN，改完代码**硬刷新** |
| **Web 端单线程** | `parallel` 在 wasm 上是编译期关掉的 | 岛并行只在原生生效；Web 靠「整岛休眠」省时间（实测睡着后 ≈100×） |
| **旧产物骗人** | 页面没变、检查还通过 | `tools/check_web.mjs` 的构建戳检查就是为了挡这个 |
| **`sync_from_physics` 忘了** | 物理在动、画面不动 | 用 `add_with_body` 绑定；离屏渲染里手动 `sync_from_physics` 一次 |
| **物理放 `update` 里** | 不同帧率下结果不同、测试飘 | 放 `fixed_update` |

---

## 6. 你自己的 crate 长什么样

`quench3d` 是个普通库，可以直接当依赖用（路径依赖即可）：

```toml
[package]
name = "my-game"
edition = "2021"

[dependencies]
quench3d = { path = "../quench3d/crates/quench3d" }

[profile.release]
lto = "thin"
codegen-units = 1
```

```rust
// src/main.rs —— 推荐直接复制 examples/starter_game.rs 再改
use quench3d::prelude::*;

struct MyGame { /* 你的状态 */ }

impl Game for MyGame {
    fn init(&mut self, engine: &mut Engine) { }
    fn update(&mut self, engine: &mut Engine, dt: f32) { }
    fn fixed_update(&mut self, engine: &mut Engine, dt: f32) { engine.world.step(dt); }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    quench3d::run(EngineConfig::default(), MyGame { })
}
```

要给 Web 用，就在同一个 crate 里加一个 wasm 入口（`#[cfg(target_arch = "wasm32")]`
+ `#[wasm_bindgen] pub fn start()`），`web/index.html` 照抄仓库里那份——它已经把
「遮罩在首帧后才收掉」「启动失败显示原因」都处理好了。

---

## 7. 目录速查

```
crates/
├── quench3d-math/       向量/矩阵/四元数/包围盒/射线（无依赖）
├── quench3d-physics/    自研物理：宽相、窄相、求解器、岛并行、休眠（零第三方依赖）
├── quench3d-render/     wgpu 渲染器：PBR、级联阴影、实例化、天空、调试线
├── quench3d/            门面：Engine / Game / Scene / 资源（桌面 + Web）
└── quench3d-cli/        CLI：schema/doctor/render/simulate/bench/ask/chat/mcp
docs/                   设计文档与计划（island_parallel_plan.md 等）
tools/                  build_web.sh / check_web.mjs / probe_start.mjs / ab_bench.sh
web/                    网页外壳 + 构建产物 pkg/
```

想更深入了解实现：`README.md` 是总览（物理管线、性能数据、门禁、已知限制），
`docs/island_parallel_plan.md` 是最近一次大改（岛并行 + 整岛休眠）的设计与实测。
