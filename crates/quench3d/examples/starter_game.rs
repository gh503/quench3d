//! 起步模板：照着这个文件改，就能长成你自己的 3D 应用 / 游戏 / 仿真。
//!
//! 它是「一份骨架 + 一张配方表」：
//!
//! * 场景怎么搭（地面、材质、相机）→ [`StarterGame::build_scene`]
//! * 物理怎么接（刚体 ↔ 场景对象自动跟随）→ [`StarterGame::spawn_box`]
//! * 逐帧逻辑写哪（输入、HUD）→ [`Game::update`]
//! * 物理写哪（固定步长）→ [`Game::fixed_update`]
//! * 桌面怎么跑：`cargo run --release --example starter_game`
//! * Web 怎么跑：`tools/build_web.sh starter_game`
//! * CI / 自动化怎么截图：`cargo run --example starter_game -- --headless out.png`
//!
//! 关键的心智模型（[`Engine`] 一帧做什么）：
//!
//! ```text
//! 每帧：  update(dt) ─┬─→ fixed_update(1/60) × 0..8   ← 物理步进只在这里
//!                     │        （累加器补齐，最多追 8 步）
//!                     ├─→ scene.sync_from_physics()   ← 自动把刚体位姿同步给场景对象
//!                     └─→ draw()  →  渲染
//! ```
//!
//! 所以你**不需要**自己调 `sync_from_physics`，也**不应该**在 `update` 里
//! `world.step(dt)`——物理要确定性，就放在 `fixed_update` 里。

use quench3d::prelude::*;

/// 箱子半边长（视觉与物理用同一个尺寸，才不会「看着穿模」）。
const BOX_HALF: f32 = 0.5;

/// 起步模板的游戏状态：只放「跨帧要记住」的东西。
struct StarterGame {
    /// 复用同一个网格/材质：`MeshId` / `MaterialId` 都是轻量句柄。
    box_mesh: MeshId,
    box_material: MaterialId,
    /// 已生成多少箱子（HUD 用）。
    spawned: usize,
    /// 自动生成用的计时器。
    auto_spawn: f32,
}

impl StarterGame {
    fn new() -> Self {
        // 句柄在 `init` 里填；这里给个占位值，`build_scene` 会覆盖。
        Self {
            box_mesh: MeshId::default(),
            box_material: MaterialId::default(),
            spawned: 0,
            auto_spawn: 0.0,
        }
    }

    /// 搭场景。`init` 与「按 R 重置」都走这里，保证两条路径完全一致。
    fn build_scene(&mut self, engine: &mut Engine) {
        // ---- 地面：视觉网格 + 静态物理半空间 ----
        let ground_mesh = engine.assets.unit_cube;
        let ground_material = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.40, 0.43, 0.48, 1.0),
            0.0,  // 金属度
            0.85, // 粗糙度
        );
        engine.scene.add_named(
            ground_mesh,
            ground_material,
            Transform::new(
                Vec3::new(0.0, -0.5, 0.0),
                Quat::IDENTITY,
                Vec3::new(20.0, 1.0, 20.0),
            ),
            "地面",
        );
        engine
            .world
            .add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }));

        // ---- 所有箱子共用一份网格与材质 ----
        self.box_mesh = engine.assets.unit_cube;
        self.box_material = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.85, 0.45, 0.25, 1.0),
            0.05,
            0.45,
        );

        // ---- 一摞箱子：验证堆叠、休眠（跑一会儿它们会整体睡着）----
        self.spawned = 0;
        for i in 0..5 {
            let y = BOX_HALF + i as f32 * (BOX_HALF * 2.0 + 0.02);
            self.spawn_box(engine, Vec3::new(0.0, y, 0.0));
        }

        // ---- 取景：直接摆轨道相机，比 focus_scene() 更可控 ----
        //
        // 默认太阳方向是 (0.42, -0.78, 0.46)（光往 +X/+Z 下方走），
        // 所以相机站到 -X/-Z 一侧才看得到被照亮的面（yaw ≈ π + 0.7）。
        engine.orbit.focus = Vec3::new(0.0, 2.4, 0.0);
        engine.orbit.distance = 12.0;
        engine.orbit.yaw = std::f32::consts::PI + 0.7;
        engine.orbit.pitch = 0.35;
    }

    /// 生成一个会掉下来的箱子：物理刚体 + 场景对象，一次搞定。
    ///
    /// `add_with_body` 会把刚体的当前位姿写进场景对象并记下绑定关系，
    /// 之后每帧由引擎自动同步——你只管 `world.step()`。
    fn spawn_box(&mut self, engine: &mut Engine, position: Vec3) {
        let body = engine.world.add_builder(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(BOX_HALF),
            })
            .position(position)
            .friction(0.6)
            .restitution(0.05),
        );
        let name = format!("箱子{}", self.spawned);
        engine
            .scene
            .add_with_body(self.box_mesh, self.box_material, body, &engine.world, &name);
        self.spawned += 1;
    }
}

impl Game for StarterGame {
    /// 引擎就绪后调用一次。
    fn init(&mut self, engine: &mut Engine) {
        self.build_scene(engine);
        log::info!("场景已就绪：{} 个刚体", self.spawned);
    }

    /// 每帧（可变步长）：输入、动画、HUD 这类「看着顺就行」的东西。
    fn update(&mut self, engine: &mut Engine, dt: f32) {
        // 空格：在相机注视点上方丢一个箱子
        if engine.input.key_pressed(KeyCode::Space) {
            let focus = engine.orbit.focus;
            let offset = (engine.time * 1.7).sin() * 1.2;
            self.spawn_box(engine, Vec3::new(focus.x + offset, focus.y + 6.0, focus.z));
        }
        // R：清空重来
        if engine.input.key_pressed(KeyCode::KeyR) {
            engine.world.clear();
            engine.scene.clear();
            self.build_scene(engine);
        }

        // 每 2 秒自动丢一个，方便无人值守观察（想关掉就删这段）
        self.auto_spawn += dt;
        if self.auto_spawn >= 2.0 {
            self.auto_spawn = 0.0;
            let focus = engine.orbit.focus;
            self.spawn_box(
                engine,
                Vec3::new(focus.x - 1.5, focus.y + 7.0, focus.z + 1.0),
            );
        }

        // HUD：直接写窗口标题，零成本
        engine.set_title(&format!(
            "Quench3D 起步模板 — 刚体 {} · {:.0} FPS · 空格生成 / R 重置 / P 暂停",
            self.spawned, engine.fps
        ));
    }

    /// 固定步长：**物理只在这里推进**（引擎按累加器调用 0..8 次）。
    fn fixed_update(&mut self, engine: &mut Engine, dt: f32) {
        engine.world.step(dt);
    }

    /// 渲染阶段：自定义绘制（调试线、准星……）。
    fn draw(&mut self, engine: &mut Engine) {
        // 例：从相机位置打一条 3 米的射线，画出命中点，方便确认拾取坐标
        // （`engine.debug.physics_contacts = true` 也能看接触点）
        let _ = engine;
    }
}

fn engine_config() -> EngineConfig {
    EngineConfig {
        title: "Quench3D 起步模板".to_string(),
        width: 1280,
        height: 720,
        ..Default::default()
    }
}

/// Web 端入口：`web/index.html` 里调用的就是它。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    quench3d::run_web(engine_config(), StarterGame::new())
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))
}

/// 时钟探针：给 `tools/check_web.mjs` 在 Node 里实测时间路径。
///
/// `std::time::Instant::now()` 在 `wasm32-unknown-unknown` 上**直接 panic**
/// （"time not implemented on this platform"），而编译期查不出来——留着这 4 行，
/// 换来的是一条能在 CI 里跑的回归防线。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn clock_probe() -> f64 {
    quench3d::clock::now_seconds()
}

/// wasm 产物是 example，需要一个（Web 端不会调用的）`main`。
#[cfg(target_arch = "wasm32")]
fn main() {}

/// 桌面端入口：`--headless <文件>` 时不开窗口，直接离屏渲染一张 PNG。
///
/// 这个模式是给 CI / 无人值守用的：同一个 [`StarterGame::build_scene`]
/// 搭出来的场景，跑 2 秒物理让它落稳，再渲染。
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|a| a == "--headless") {
        let path = args
            .get(index + 1)
            .cloned()
            .unwrap_or_else(|| "starter_game.png".to_string());
        let image = quench3d::render_offscreen(1280, 720, &engine_config(), |engine| {
            let mut game = StarterGame::new();
            game.init(engine);
            // 离屏路径没有引擎帧循环，所以要自己步进物理并同步位姿
            for _ in 0..120 {
                engine.world.step(1.0 / 60.0);
            }
            engine.scene.sync_from_physics(&engine.world);
            engine.focus_scene();
        })
        .map_err(|e| format!("离屏渲染失败：{e}"))?;
        quench3d::write_png(&image, &path)?;
        println!("已渲染 {path}（{}x{}）", image.width, image.height);
        return Ok(());
    }

    quench3d::run(engine_config(), StarterGame::new())
}
