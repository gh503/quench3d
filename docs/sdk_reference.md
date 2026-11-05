# 引擎 SDK 与能力参考

[返回项目入口](../README.md)。本页包含详细操作与接口说明；机器可读参数以 `quench3d schema --json` 为准。

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

场景生命周期回调（可选；不装时零开销）：物体加入/删除/清空会主动调用，
不再需要每帧扫描 `objects` 做差分。回调里只读，不要在回调里再改场景。

```rust
struct Logger;
impl SceneHooks for Logger {
    fn on_object_added(&mut self, index: usize, object: &SceneObject) {
        println!("+{index} {}", object.name);
    }
    fn on_object_removed(&mut self, index: usize, object: &SceneObject) {
        println!("-{index} {}", object.name);
    }
}
engine.scene.set_hooks(Some(Box::new(Logger)));
```

这是**宿主回调**，不是脚本语言/热重载；逐属性修改、物理事件仍走各自 API。

### 3.2 场景与资源

```rust
let mesh = engine.assets.unit_cube;   // 内置：unit_cube / unit_sphere / unit_cylinder / unit_capsule / unit_cone / unit_plane
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

播放控制里还有**时间缩放**（0.1×–2×，编辑器工具栏/播放菜单）：它只改物理、动画、
粒子与 `Engine::time`，不改相机手感、`game.update(dt)` 的真实帧间隔与 FPS 统计；
0 是“冻结仿真但不暂停”。离屏录制用 `SequenceParams` 的步长/帧率，不受它影响。

播放控制里还有**时间缩放**（0.1×–2×，编辑器工具栏/播放菜单）：它只改物理、动画、
粒子与 `Engine::time`，不改相机手感、`game.update(dt)` 的真实帧间隔与 FPS 统计；
0 是“冻结仿真但不暂停”。离屏录制用 `SequenceParams` 的步长/帧率，不受它影响。

编辑器工具栏的**回放**控制会把刚体位姿录进内存：录制时按物理步采，播放时暂停
物理、只写回位姿并立即同步场景，时间轴可拖动；停止回到第 0 帧。它和 CLI 的
`record`/`movie --replay` 用同一套 `ReplayClip`，但**只录刚体位姿**，不录速度、
流体、泡沫、气泡或关节，也不会写进场景文件。

### 3.3 物理

Rust 宿主可通过 `world.config.global_normal_projection = true` 启用全局投影法向求解；
默认 `false` 保留旧路径。此选项目前通过 Rust 配置开启，尚未接入场景 JSON 或编辑器面板。
已验收的堆叠配置为 60 Hz、`speculative_margin = 0.005`、20 次速度与 8 次位置迭代：
4/12/32 层各运行 1800 步，逐步独立 OBB 检查箱间与有限地板穿透不超过 5.01 mm，
同时要求有限状态与无离地板样本。固定成本门禁比较同一 release 二进制的完整 `world.step`，
12/32 层分别要求候选与旧路径中位数之比不超过 1.15；这些固定场景结果不代表任意场景精度或普适加速。
持续精度已纳入普通测试，默认路径逐状态黄金校验和固定成本已接入完整门禁及 Linux CI。
完整过程和适用范围见[验收记录](experiments/global_normal_final_gate.md)。

```rust
let mut world = PhysicsWorld::with_gravity(Vec3::new(0.0, -9.81, 0.0));

// 9 种形状
Shape::Sphere { radius: 0.5 }
Shape::Box { half_extents: Vec3::splat(0.5) }
Shape::Capsule { half_height: 0.6, radius: 0.3 }
Shape::Cylinder { half_height: 0.5, radius: 0.5 }
Shape::Cone { half_height: 0.5, radius: 0.5 }   // 底面在 -h、顶点在 +h（质心不在原点）
Shape::RoundedBox { half_extents: Vec3::splat(0.5), radius: 0.05 }  // 盒子 ⊕ 半径 r 的球
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

// 刚体位姿录制/回放：只存渲染位姿，二进制带 magic/版本/FNV 校验
let mut clip = ReplayClip::new(world.fixed_dt());
clip.record(&world);                 // 每固定步调一次
world.step(1.0 / 60.0);
clip.record(&world);
let bytes = clip.encode();
let loaded = ReplayClip::decode(&bytes)?;
loaded.apply(&mut world, 0.05);      // 按时间插值写回位姿；播放时应暂停物理

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
        .ccd(true)                // 高速小物体：中心射线 + 形状极端点旋转扫掠，不穿薄墙
        // .ccd_precise(true)     // 凸-凸/半空间/三角网精确 TOI：支持函数 + 三角网 AABB 网格/球解析最近点，采样间隙也不漏；更贵
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

### 3.4 音频（离线/宿主输出）

`quench3d-audio` 是不依赖设备库的音频基础层：WAV 解码（PCM 8/16/24/32 与
float32/64）、多源混音、线性重采样、3D 距离衰减与等功率声像。
`Engine::audio` 已经是一个 `AudioMixer`，宿主每帧把 `mix` 写出的交错立体声
缓冲交给自己的音频后端（桌面 cpal / WebAudio）即可；本仓库**不假装已经内置
设备输出**，但解码/混音/声像/循环都有无设备单测。

```rust
let clip = quench3d::audio::AudioClip::from_wav(&std::fs::read("music.wav")?)?;
engine.audio.play(clip, quench3d::math::vec3(1.0, 0.0, 0.0));
let mut out = vec![0.0f32; 480 * 2];
engine.audio.mix(&mut out, 48_000);
```

### 3.5 相机与输入

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
`F6` 包围盒、`F7` 查询探针、`F8` 模型骨骼、`F11` 关节、`T` 循环色调映射、`K` 3D 光标到选中中心、`O` 自动 LOD、`P` 暂停、`Esc` 退出。

### 3.6 截图 / 离屏渲染

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
| 后处理 | `rgba16float` 中间目标 → 可选 SSAO（深度-only，法线由深度重建，`post.wgsl`）→ 可选 bloom（半分辨率亮度提取 + 可分离高斯）→ 可选 TAA（当前帧 + 历史；优先读运动向量，缺失像素回退深度重投影，`taa.wgsl` + `motion.wgsl`）→ 统一色调映射（ACES / Reinhard / 线性三档，**只此一处**）→ 可选 FXAA（tonemap 之后，见 `fxaa.wgsl`）；混合因此发生在 tonemap 之前。MSAA > 1 且开 SSAO 时多一趟深度解析（`depth_resolve.wgsl`）。**GL/WebGL2 上 SSAO、深度解析与 TAA 按能力整体关闭**（naga 的 GLSL 后端不支持读深度纹理，见 [production_readiness §12.7](production_readiness.md)）；tonemap / bloom / FXAA 不受影响 |
| LOD | 静态高模自动生成 1/2、1/4 两档（`render.auto_lod`，顶点聚类，非蒙皮），按批次包围盒距离切换；手工 `models[].lods` 仍可用；`O` 开关。蒙皮/动画网格不自动生成，无遮挡剔除 |
| 运动模糊 | 相机运动模糊（`lighting.motion_blur`，缺省关；深度反投影 + 上一帧 view-proj，原生后端；物体自身运动不糊） |
| 阴影 | 太阳：单张（级联时 2×2 图集）`Depth32Float` 阴影贴图（分辨率是渲染器构造参数 `RendererOptions::shadow_map_size`，**不在场景文件里**），按纹素对齐的正交拟合，3×3 PCF（硬件比较采样）；阴影强度（`.strength`）与覆盖半径（`.radius`）可从场景文件调；**级联阴影** `.cascades`（1~4，缺省 1 = 单级老行为）+ `.split_lambda`（0~1）：多级按视锥 PSSM 切段，主 pass 按视空间深度选级。**局部光阴影**：`SpotLight.cast_shadows`（一张透视 2D 深度图，视锥 = 外锥角）与 `PointLight.cast_shadows`（**立方体阴影**：`texture_depth_2d_array` 6 层，每层 90°，面选择 + 面内 uv 是 CPU 纯函数、WGSL 逐字镜像，靠"公式 == 矩阵"的 CPU 测试保证两边一致）；聚光**每帧最多 4 盏**（4 层深度数组，逐层 pass）、点光每帧 1 盏（6 面数组）；两者都缺省 `false`（老场景像素逐位不变），两套图互相独立 |
| 光照 | 1 个平行光 + 最多 **8** 盏局部光（点光源与聚光灯共享名额、点光源优先；`MAX_POINT_LIGHTS`，与 WGSL 数组长度一致；超出部分给出警告后忽略），平滑衰减到半径处归零，聚光灯有内外锥角的平滑过渡，环境光 + 程序化天空 + **IBL 环境贴图**（`lighting.environment_map`，等距柱状 HDR/EXR/PNG/JPEG → 64px 7 级预滤波 cubemap，按粗糙度采样）或解析式环境高光（不写贴图时；`RendererOptions.environment` 可关，缺省开）；**指数距离雾**（`lighting.fog`，不写 = 关闭）。IBL 没有精确 PMREM/GGX、没有屏幕空间反射 |
| 天空/地面 | 全屏三角形程序化天空（三个渐变颜色可从 `lighting.sky_colors` 调）+ 解析无限网格（`fwidth` 抗锯齿 + 深度写入） |
| 批处理 | 不透明按 (材质, 网格) 分批 + 实例化（160 字节/实例）+ 逐实例视锥剔除；超过 `render.max_draw_distance` 的实例按包围盒最近点剔除（`RenderStats.distance_culled` / 报告同名字段，不是遮挡剔除）；**半透明按相机距离全局由远到近逐实例绘制**（按实例 AABB 中心排序，不再按批次/材质 id 顺序）；`RendererOptions.oit`（缺省关闭）可切到 WBOIT，非水面半透明与绘制顺序无关，GL/WebGL2 自动回退旧排序 |
| 其它 | MSAA（1/2/4/8；GL/WebGL2 上按能力夹到 1）、Alpha 混合与 Alpha 裁剪、无光照材质、调试线段/接触点/包围盒 |
| 后端 | 桌面 Vulkan/Metal/DX12，Web WebGPU→WebGL2 回退；同一套 WGSL。GL/WebGL2 上：SSAO 按能力关闭（读不了深度纹理）、**MSAA 按能力夹到 1**（多采样纹理在 WebGL2 的 glow 里是 `panic!`、在原生 GL 上出黑帧），见 [production_readiness §12.7](production_readiness.md) |

### 色彩约定

* **工作空间是线性 RGB**：光照、雾、自发光、HDR 中间目标（`rgba16float`）都在线性空间。
* **材质颜色是 sRGB**：`materials.*.color` / `emissive_color` 按 sRGB 解释，进入着色前转线性；
  灯光颜色（太阳/点光/聚光）是**线性**值，不要照抄材质数值。
* **输出**：色调映射把 HDR 压到 0..1，写 sRGB 目标时由硬件做一次 sRGB 编码；
  不再在着色器里手工编码。
* **色调映射算子可选**：`Tonemap::Aces`（默认）/ `Reinhard` / `Linear`，
  编辑器灯光面板可切换；曝光仍在 tonemap 前乘到线性光上。
* **边界**：没有 ICC / 可配置输出色彩空间；贴图没有 per-texture 色彩空间元数据，
  统一按 sRGB 处理。

### 单位、轴向与确定性

* **单位**：1 个世界单位 = **1 米**；重力缺省 `-9.81 m/s²`；质量是千克；时间步是秒。
* **轴向**：**Y-up、右手系**。`+X` 向右、`+Y` 向上、`+Z` 指向屏幕外；相机默认看向 `-Z`。
* **导入**：glTF 本身就是 Y-up 右手系，直接按原样导入；OBJ 没有轴向声明，也按 Y-up
  解释。**没有**导入时的轴向/缩放转换开关——源资产不是 Y-up 时先在导出端转好。
* **确定性边界**：
  * 同一二进制、同一机器、同一线程数：**逐位确定**；并行求解与串行逐位一致，
    线程数 2/3/4 不影响结果（`api_tests.rs` 有测试）。
  * 跨平台/跨编译器：**不承诺逐位一致**。浮点收缩、`libm` 实现、SIMD 路径都可能不同；
    需要跨机器对账时用带容差的物理量，不要比对浮点字节。
  * 渲染：同一 GPU/驱动/帧参数下可复现；不同 GPU 的 MSAA/精度/纹理过滤不保证逐像素一致。

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

### 4.1 摄像头动捕（实验）

一条**本地、单帧、单人**的摄像头参考管线，入口在
[`tools/camera_mocap/`](../tools/camera_mocap/)：

* 采集：OpenCV `VideoCapture`（Linux V4L2），普通 RGB 摄像头；
* 姿态：MediaPipe Pose，33 个 2D 关键点 + 33 个以髋部为原点的 3D 世界点；
* 轮廓：Pose 自带人体分割 mask → 最大外轮廓多边形；
* 深度：可选 MiDaS small ONNX，**单目相对逆深度**（0..255，不是米）；
* 点云：把深度按相机内参反投影成 `latest.points`（xyz_f32_le，缺省 6 像素下采样），
  z 是 0.3~3.0 的**任意尺度**；采集端用 `--no-points` 只写深度时，编辑器会用
  JSON 里的 `intrinsics` + `depth.point_step/z_near/z_far/origin` 现算同一套点云
  （缺内参不猜，保持无点云）；
* 编辑器：`quench3d-editor` 的「摄像头动捕（实验）」浮动窗口显示画面/轮廓/骨架，
  3D 世界点作为调试线段叠加到视口（青绿骨骼、黄色关节点）；勾选「显示点云」后
  把深度反投影点画成小十字；有 `latest.rgb` 和内参时按摄像头画面取色，缺色时退回青色。勾选「作为 3D 视口背景（AR）」后，最新画面按
  cover 铺满视口、画在天空之前、场景几何之后——模型真正叠在摄像头画面上，而不是
  只在浮动窗口里预览；画面会经过场景的色调映射/泛光。
  「生成/更新网格」（或勾选「实时跟随」逐新帧自动重建）把当前深度点云按体素
  （1~10 cm）固化成三角表面（默认 Surface Nets 平滑表面，可切回体素块；
  两种模式都先按每轴 0.5% 分位裁掉深度飞点，超过 20 万点时先等距抽样），作为一个
  **会话内生成**的可选中物体加进场景：有真实网格、法线、包围盒，可移动/打光/渲染。
  有 `latest.rgb` 和内参时，顶点会按针孔模型投回原图、把摄像头画面作为基础色贴图，
  所以重建出来是**带颜色的表面**，不是单色石膏；没内参/没 RGB 时自动退回原来的
  蓝灰纯色，旧路径不变。每换一帧只重传一张贴图，旧贴图立即回收。
  它不会被写进场景 JSON（没有源网格，重开时会悬空），需要长期保存请另存为 glTF。
  勾选「时间累积」后，最近约 90 帧点云按体素平均后再重建：慢速移动时能补上
  单帧遮挡/噪声，快速移动会有拖影；缺省关，关掉时仍是单帧旧路径。
  同面板的「跟踪位姿（ICP）」把相邻帧点云做刚体配准，再叠进累积参考系，
  移动相机时不会把同一表面拖成双层；只适合相邻帧小运动（初始猜测是恒等），
  配准失败会清空累积器并以当前帧重开参考系，不会把两个坐标系硬混在一起。
  最近邻用构建一次的稠密网格链表，合成 4k 参考 + 1.5k 当前点在 release 下约
  1.35 ms、3 轮收敛，低于 60 Hz 单帧预算；这是**增量**跟踪，不是全局 SLAM/回环，也没有 IMU 融合。桌面窗口直接提供开关；
  Web 端通过 `window.quench3dCameraDepthTracking(true)` 或摄像头窗口的同一开关
  使用（会同时打开时间累积）。
  深度-only 帧（没有 `latest.points`）同样能重建；点云来自读端的内参反投影，
  不是新增的推理。

```bash
python3 -m venv target/tmp/camvenv
target/tmp/camvenv/bin/python -m pip install -r tools/camera_mocap/requirements.txt
target/tmp/camvenv/bin/python tools/camera_mocap/camera_mocap.py --source 0
cargo run -p quench3d-editor
```

没有摄像头时用 `--source synthetic` 验证协议；深度模型需要显式下载一次，详见
[`tools/camera_mocap/README.md`](../tools/camera_mocap/README.md)。

**Web 端是预览 + 外部姿态/深度注入 + 3D 背景**：顶部「摄像头」按钮 → `getUserMedia` →
`<video>` → canvas → egui 纹理；窗口里勾选「作为 3D 背景」后，同一帧 RGBA 会上传为
3D 视口背景（同帧不重传，关掉即清除）。页面脚本（例如 MediaPipe Tasks Vision）可调用

```js
window.quench3dCameraPose(JSON.stringify({
  present: true, confidence: 0.9,
  landmarks: [{ x: 0.5, y: 0.2 }, /* … 归一化图像坐标，至少 2 点 */],
  // 可选：MediaPipe 世界坐标（米，髋部原点），给了就在 3D 视口里画骨架
  world: [{ x: 0.0, y: 0.0, z: 0.0 }, /* … */],
}));
window.quench3dCameraPoseClear();
```

外部深度（WebXR Depth Sensing / 服务端推流）也可注入。参数与桌面 `latest.depth`
同一套约定：`u8=255 → zNear`、`u8=0 → zFar`，`step` 是像素下采样步长，
`origin` 是反投影后加的世界坐标：

```js
window.quench3dCameraDepth(
  width, height,
  new Uint8Array(depthBuffer),       // 长度必须是 width*height
  fx, fy, cx, cy, zNear, zFar, step,
  [0, 0, 0],
);
// 窗口里勾「显示注入深度的点云」，或点「从注入深度重建表面」画体素表面线框
window.quench3dCameraDepthAccumulation(true); // 可选：最近约 90 帧按 2 cm 体素平均
window.quench3dCameraDepthClear();
```

浏览器**不把普通 RGB 猜成深度**；没有外部深度源时点云/表面保持空。深度点云只用于
调试线框，不写回场景 JSON。窗口里的「深度时间累积」与桌面同一套
`PointCloudAccumulator`（2 cm 体素、约 90 帧 TTL）；关掉立即回到单帧原始点云。桌面「打开」与「导出 glTF/GLB」都会先弹内置文件选择器（导出可改路径/文件名）。
读端把 2D 33 点连接画成洋红骨架，有 `world` 时按同一套 35 根骨在 3D 视口里画
teal/gold 骨架。**浏览器里不内置推理**（没有 MediaPipe 模型与文件协议），
骨架/轮廓/深度仍走桌面 Python 管线。需要 https 或 `localhost`。
`tools/probe_web_camera.sh` 用 Chrome 假摄像头 + `?camera=1` 做端到端截图探针，
并注入 33 点姿态、断言 `camera_pose=true` 且截图里真的有洋红像素。

**边界（诚实）**：没有 SMPL/SMPL-X 体型拟合、没有多人体、没有米制深度、没有
TSDF 网格重建（只有相对尺度点云；Web 表面是体素/Surface Nets 线框，不是可编辑网格）、
没有时间同步/IMU 融合；Web 普通 RGB 摄像头不产生深度，深度必须由 WebXR Depth
Sensing 或服务端等外部源注入。3D 骨骼是 MediaPipe 的 33 点近似，不是精确关节角。
所有推理在本地完成，不把画面发到网络。

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

