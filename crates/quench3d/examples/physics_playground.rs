//! 物理沙盒：堆叠箱子、斜面、球体、实时生成刚体、射线检测。
//!
//! 这是桌面端与 Web 端**共用**的示例。桌面端：
//! ```text
//! cargo run --release --example physics_playground
//! ```
//! Web 端：`tools/build_web.sh physics_playground` 后打开 `web/index.html`。
//!
//! 操作：
//! * `中键` / `Alt+左键` 拖拽环绕，`滚轮` 缩放；
//! * `右键` 拖拽进入自由视角，`W A S D` + `Q E` 移动，`Shift` 加速；
//! * `空格` 从相机位置发射一个球；`B` / `C` / `V` 生成箱子 / 圆柱 / 胶囊；
//! * `X` 清空动态物体，`回车` 重新摆好场景，`P` 暂停。

use quench3d::prelude::*;
use std::f32::consts::FRAC_PI_6;

/// 一种「物理驱动」的物体：网格、材质、形状与视觉尺寸。
/// （`mesh` / `material` 在 `Game::init` 里才拿得到，构造时先填 0。）
#[derive(Clone)]
struct SpawnKind {
    mesh: MeshId,
    material: MaterialId,
    /// `unit_*` 网格边长是 1，用它把网格缩放到物理形状的大小。
    visual_scale: Vec3,
    shape: Shape,
    restitution: f32,
}

struct Playground {
    box_kind: SpawnKind,
    sphere_kind: SpawnKind,
    cylinder_kind: SpawnKind,
    capsule_kind: SpawnKind,
    rng: u32,
    hit: Option<RaycastHit>,
    dynamic_count: usize,
}

impl Playground {
    /// xorshift：确定性、无依赖的伪随机数。
    fn next_f32(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / 16_777_216.0
    }

    fn next_range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next_f32()
    }

    /// 把刚体挂到场景里，并按形状尺寸缩放网格。
    fn attach(&self, engine: &mut Engine, kind: &SpawnKind, body: BodyHandle, name: String) {
        let index =
            engine
                .scene
                .add_with_body(kind.mesh, kind.material, body, &engine.world, &name);
        engine.scene.objects[index].transform.scale = kind.visual_scale;
    }

    fn spawn_dynamic(
        &self,
        engine: &mut Engine,
        kind: &SpawnKind,
        position: Vec3,
        name: &str,
    ) -> BodyHandle {
        let body = engine.world.add_builder(
            BodyBuilder::dynamic(kind.shape.clone())
                .position(position)
                .friction(0.55)
                .restitution(kind.restitution)
                .linear_damping(0.02)
                .angular_damping(0.06),
        );
        self.attach(engine, kind, body, name.to_string());
        body
    }

    /// 删除所有动态刚体（保留地面/斜面等静态体）。
    fn clear_dynamic(&self, engine: &mut Engine) {
        let handles: Vec<BodyHandle> = engine
            .world
            .bodies()
            .filter(|(_, b)| b.is_dynamic())
            .map(|(h, _)| h)
            .collect();
        for handle in handles {
            engine.world.remove_body(handle);
        }
        let world = &engine.world;
        engine
            .scene
            .objects
            .retain(|o| o.body.map(|h| world.contains(h)).unwrap_or(true));
    }

    fn dynamic_count(engine: &Engine) -> usize {
        engine
            .world
            .bodies()
            .filter(|(_, b)| b.is_dynamic())
            .count()
    }

    /// 搭好整个沙盒。
    fn build_scene(&mut self, engine: &mut Engine) {
        self.clear_dynamic(engine);
        engine
            .scene
            .objects
            .retain(|o| o.name != "地面" && o.name != "斜面");

        // ---- 地面：物理上是无限半空间，视觉上放一块大方块 ----
        engine
            .world
            .add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }));
        let ground_mat = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.47, 0.49, 0.53, 1.0),
            0.0,
            0.92,
        );
        engine.scene.add_named(
            engine.assets.unit_cube,
            ground_mat,
            Transform::new(
                Vec3::new(0.0, -0.5, 0.0),
                Quat::IDENTITY,
                Vec3::new(40.0, 1.0, 40.0),
            ),
            "地面",
        );

        // ---- 斜面（静态盒子，绕 Z 轴倾斜 30°） ----
        let ramp_half = Vec3::new(4.0, 0.25, 3.0);
        let ramp_rot = Quat::from_euler_yxz(0.0, 0.0, -FRAC_PI_6);
        let ramp_pos = Vec3::new(-7.5, 2.0, 0.0);
        let ramp_body = engine.world.add_builder(
            BodyBuilder::r#static(Shape::Box {
                half_extents: ramp_half,
            })
            .transform(Transform::new(ramp_pos, ramp_rot, Vec3::ONE)),
        );
        let ramp_mat = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.36, 0.56, 0.74, 1.0),
            0.05,
            0.55,
        );
        let index = engine.scene.add_with_body(
            engine.assets.unit_cube,
            ramp_mat,
            ramp_body,
            &engine.world,
            "斜面",
        );
        engine.scene.objects[index].transform.scale = ramp_half * 2.0;

        // ---- 箱子金字塔 ----
        let half = 0.5_f32;
        for level in 0..5 {
            let count = 5 - level;
            let y = half + level as f32 * (half * 2.0 + 0.004);
            for i in 0..count {
                let x = (i as f32 - (count as f32 - 1.0) * 0.5) * (half * 2.0 + 0.03);
                let kind = self.box_kind.clone();
                self.spawn_dynamic(engine, &kind, Vec3::new(x, y, 5.0), "箱子");
            }
        }

        // ---- 一排滚落的球 ----
        for i in 0..7 {
            let mut kind = self.sphere_kind.clone();
            let radius = 0.5;
            kind.shape = Shape::Sphere { radius };
            kind.visual_scale = Vec3::splat(radius * 2.0);
            let x = -6.0 + i as f32 * 2.0;
            self.spawn_dynamic(engine, &kind, Vec3::new(x, 3.0 + i as f32 * 0.4, 1.5), "球");
        }

        // ---- 胶囊与圆柱 ----
        for i in 0..4 {
            let kind = self.capsule_kind.clone();
            let x = -1.5 + i as f32 * 1.5;
            self.spawn_dynamic(engine, &kind, Vec3::new(x, 4.0, -2.0), "胶囊");
        }
        for i in 0..4 {
            let kind = self.cylinder_kind.clone();
            let x = 1.0 + i as f32 * 1.5;
            self.spawn_dynamic(engine, &kind, Vec3::new(x, 5.0, -4.5), "圆柱");
        }

        engine.focus_scene();
        self.dynamic_count = Self::dynamic_count(engine);
    }
}

impl Game for Playground {
    fn init(&mut self, engine: &mut Engine) {
        // 准备网格与材质（此时 GPU 已就绪）
        self.box_kind.mesh = engine.assets.unit_cube;
        self.box_kind.material = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.88, 0.44, 0.28, 1.0),
            0.05,
            0.45,
        );
        self.sphere_kind.mesh = engine.assets.unit_sphere;
        self.sphere_kind.material = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.28, 0.62, 0.90, 1.0),
            0.12,
            0.22,
        );
        self.cylinder_kind.mesh = engine.assets.unit_cylinder;
        self.cylinder_kind.material = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.95, 0.78, 0.25, 1.0),
            0.85,
            0.20,
        );
        self.capsule_kind.mesh = engine.assets.unit_capsule;
        self.capsule_kind.material = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.40, 0.82, 0.46, 1.0),
            0.08,
            0.38,
        );

        // 相机初始站位
        engine.orbit.focus = Vec3::new(0.0, 1.5, 0.0);
        engine.orbit.distance = 20.0;
        engine.orbit.yaw = 0.62;
        engine.orbit.pitch = 0.34;

        self.build_scene(engine);
        log::info!("空格 发射球体 · B/C/V 生成箱子/圆柱/胶囊 · X 清空 · 回车 重置");
    }

    fn fixed_update(&mut self, engine: &mut Engine, dt: f32) {
        engine.world.step(dt);
    }

    fn update(&mut self, engine: &mut Engine, _dt: f32) {
        let camera = engine.camera();
        let muzzle = camera.position + camera.forward() * 1.5;

        if engine.input.key_pressed(KeyCode::Space) {
            let mut kind = self.sphere_kind.clone();
            let radius = 0.42;
            kind.shape = Shape::Sphere { radius };
            kind.visual_scale = Vec3::splat(radius * 2.0);
            let body = engine.world.add_builder(
                BodyBuilder::dynamic(kind.shape.clone())
                    .position(muzzle)
                    .linear_velocity(camera.forward() * 24.0)
                    .friction(0.4)
                    .restitution(0.45),
            );
            self.attach(engine, &kind, body, "炮弹".to_string());
        }
        if engine.input.key_pressed(KeyCode::KeyB) {
            let jitter = Vec3::new(self.next_range(-0.5, 0.5), 0.0, self.next_range(-0.5, 0.5));
            let position = engine.orbit.focus + Vec3::new(0.0, 3.0, 1.0) + jitter;
            let kind = self.box_kind.clone();
            self.spawn_dynamic(engine, &kind, position, "新箱子");
        }
        if engine.input.key_pressed(KeyCode::KeyC) {
            let position = engine.orbit.focus + Vec3::new(0.0, 4.0, 1.0);
            let kind = self.cylinder_kind.clone();
            self.spawn_dynamic(engine, &kind, position, "新圆柱");
        }
        if engine.input.key_pressed(KeyCode::KeyV) {
            let position = engine.orbit.focus + Vec3::new(0.0, 5.0, 1.0);
            let kind = self.capsule_kind.clone();
            self.spawn_dynamic(engine, &kind, position, "新胶囊");
        }
        if engine.input.key_pressed(KeyCode::KeyX) {
            self.clear_dynamic(engine);
            log::info!("已清空动态刚体");
        }
        if engine.input.key_pressed(KeyCode::Enter) {
            self.build_scene(engine);
        }

        // 屏幕中心射线检测
        let (w, h) = engine.size();
        let ray = camera.screen_ray(
            Vec2::new(w as f32 * 0.5, h as f32 * 0.5),
            Vec2::new(w as f32, h as f32),
        );
        self.hit = engine.world.raycast(ray.origin, ray.dir, 300.0);
        self.dynamic_count = Self::dynamic_count(engine);
    }

    fn draw(&mut self, engine: &mut Engine) {
        let camera = engine.camera();
        match &self.hit {
            Some(hit) => {
                engine.renderer.lines.line(
                    camera.position,
                    hit.point,
                    Vec4::new(1.0, 0.85, 0.25, 1.0),
                );
                // 命中点标记 + 法线
                engine.renderer.lines.contact_point(
                    hit.point,
                    hit.normal,
                    0.35,
                    Vec4::new(1.0, 0.45, 0.15, 1.0),
                );
            }
            None => {
                engine.renderer.lines.line(
                    camera.position,
                    camera.position + camera.forward() * 80.0,
                    Vec4::new(0.25, 0.85, 0.35, 1.0),
                );
            }
        }
    }
}

/// 桌面端与 Web 端共用的引擎配置。
fn engine_config() -> EngineConfig {
    EngineConfig {
        title: "Quench3D — 物理沙盒".to_string(),
        width: 1280,
        height: 720,
        ..Default::default()
    }
}

/// 桌面端与 Web 端共用的初始场景状态。
fn make_playground() -> Playground {
    Playground {
        box_kind: SpawnKind {
            mesh: 0,
            material: 0,
            visual_scale: Vec3::splat(1.0),
            shape: Shape::Box {
                half_extents: Vec3::splat(0.5),
            },
            restitution: 0.18,
        },
        sphere_kind: SpawnKind {
            mesh: 0,
            material: 0,
            visual_scale: Vec3::splat(1.0),
            shape: Shape::Sphere { radius: 0.5 },
            restitution: 0.5,
        },
        cylinder_kind: SpawnKind {
            mesh: 0,
            material: 0,
            visual_scale: Vec3::new(1.0, 1.2, 1.0),
            shape: Shape::Cylinder {
                half_height: 0.6,
                radius: 0.5,
            },
            restitution: 0.25,
        },
        capsule_kind: SpawnKind {
            mesh: 0,
            material: 0,
            visual_scale: Vec3::new(1.0, 1.5, 1.0),
            shape: Shape::Capsule {
                half_height: 0.4,
                radius: 0.35,
            },
            restitution: 0.3,
        },
        rng: 0x1234_5678,
        hit: None,
        dynamic_count: 0,
    }
}

/// Web 端入口：`web/index.html` 调用 `start()`。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    quench3d::run_web(engine_config(), make_playground())
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))
}

// 三个 Web 自检探针的实现住在 `quench3d::web_probe` 里（那里有详细说明），
// 这里只做转发：检查脚本认的是 example 导出的这些名字。

/// 时钟探针：读一次引擎时钟（秒）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn clock_probe() -> f64 {
    quench3d::web_probe::clock_probe()
}

/// 路径探针：Web 端按路径加载模型必须给出能照着做的报错。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn model_path_probe() -> String {
    quench3d::web_probe::model_path_probe()
}

/// 字节探针：Web 端从字节加载模型必须真的能跑通。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn model_bytes_probe() -> String {
    quench3d::web_probe::model_bytes_probe()
}

/// wasm 产物是 example，需要一个（Web 端不会调用它的）`main`。
#[cfg(target_arch = "wasm32")]
fn main() {}

/// 桌面端入口。
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    quench3d::run(engine_config(), make_playground())
}
