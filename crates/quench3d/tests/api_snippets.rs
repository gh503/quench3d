//! 文档里的 API 用法必须能编译。
//!
//! `docs/getting_started.md` §3 的每一段配方都抄在这里，函数**从不被调用**
//! （只求编译通过），所以这里不碰 GPU、不开窗口，`cargo check --all-targets`
//! 就能在原生和 wasm 两个目标上把签名钉住。文档改错了名字，这里先炸。
//!
//! 加新配方时：先在这里写上，再抄进文档。

#![allow(dead_code)]

use quench3d::prelude::*;

/// §3.1 加载外部模型。
fn load_models(engine: &mut Engine) -> Result<(), Box<dyn std::error::Error>> {
    let objects = engine.load_model(
        "assets/robot.glb",
        Transform::new(Vec3::ZERO, Quat::IDENTITY, Vec3::splat(1.0)),
    )?;
    println!("加载了 {} 个网格对象", objects.len());
    // 第三个参数是摩擦系数：静态三角网格碰撞体（关卡地形/建筑）
    let _ = engine.load_model_as_static("assets/tree.obj", Transform::IDENTITY, 0.8);
    // 这三个在 crate 根的 `assets` 模块里（prelude 也已导出，可直接用裸名）
    let _ = quench3d::assets::load_model("assets/robot.glb");
    let _ = quench3d::assets::load_gltf("assets/robot.glb");
    let _ = quench3d::assets::load_obj("assets/tree.obj");
    Ok(())
}

/// §3.2 刚体形状与参数。
fn bodies(engine: &mut Engine) {
    let body = engine.world.add_builder(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .position(Vec3::new(0.0, 5.0, 0.0))
        .rotation(Quat::from_euler_yxz(0.3, 0.0, 0.0))
        .linear_velocity(Vec3::new(1.0, 0.0, 0.0))
        .angular_velocity(Vec3::new(0.0, 1.0, 0.0))
        .friction(0.6)
        .restitution(0.2)
        .density(800.0)
        .linear_damping(0.05),
    );
    let _ = engine
        .world
        .add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }));
    let _ = engine
        .world
        .add_builder(BodyBuilder::kinematic(Shape::Sphere { radius: 0.5 }));
    let _ = engine
        .world
        .add_builder(BodyBuilder::dynamic(Shape::Capsule {
            half_height: 0.6,
            radius: 0.3,
        }));
    let _ = engine
        .world
        .add_builder(BodyBuilder::dynamic(Shape::Cylinder {
            half_height: 0.5,
            radius: 0.4,
        }));
    let _ = engine
        .world
        .add_builder(BodyBuilder::dynamic(Shape::ConvexHull {
            points: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
        }));
    let _ = engine
        .world
        .add_builder(BodyBuilder::dynamic(Shape::TriMesh {
            vertices: vec![Vec3::ZERO, Vec3::X, Vec3::Z],
            indices: vec![0, 1, 2],
        }));
    let _ = engine
        .world
        .add_builder(BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 }).mass(2.0));
    let _ = body;
}

/// §3.3 物理 ↔ 场景绑定。
fn bind(engine: &mut Engine) {
    let mesh = engine.assets.unit_cube;
    let material = engine.assets.solid_material(
        &mut engine.renderer,
        &engine.gpu,
        Vec4::new(0.9, 0.35, 0.2, 1.0),
        0.1,
        0.4,
    );
    let body = engine
        .world
        .add_builder(BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 }));
    engine
        .scene
        .add_with_body(mesh, material, body, &engine.world, "球");
    engine.scene.sync_from_physics(&engine.world);
    engine.scene.sync_to_physics(&mut engine.world);
}

/// §3.4 施力、读状态、唤醒。
fn forces(engine: &mut Engine, body: BodyHandle, impulse: Vec3, world_point: Vec3, force: Vec3) {
    if let Some(b) = engine.world.body_mut(body) {
        b.apply_impulse(impulse);
        b.apply_impulse_at_point(impulse, world_point);
        b.apply_force(force);
        b.apply_torque_impulse(Vec3::Y);
        let _ = b.linear_velocity();
        let _ = b.angular_velocity();
        let _ = b.position();
        let _ = b.is_sleeping();
        if b.is_sleeping() {
            b.wake_up();
        }
        b.set_can_sleep(false);
    }
    let _ = engine.world.body(body);
}

/// §3.5 射线拾取。
fn picking(engine: &mut Engine) {
    let mouse = engine.input.mouse_pos();
    let camera = engine.camera();
    let ray = camera.screen_ray(mouse, engine.viewport());
    if let Some(hit) = engine.world.raycast(ray.origin, ray.dir, 1000.0) {
        println!(
            "命中 {:?}，落点 {:?}，距离 {}，法线 {:?}",
            hit.handle, hit.point, hit.distance, hit.normal
        );
    }
    let _ = engine
        .world
        .raycast_filtered(ray.origin, ray.dir, 1000.0, |_handle| true);
}

/// §3.6 相机。
fn cameras(engine: &mut Engine) {
    engine.orbit.focus = Vec3::new(0.0, 2.0, 0.0);
    engine.orbit.distance = 12.0;
    engine.orbit.yaw = std::f32::consts::PI + 0.7;
    engine.orbit.pitch = 0.35;
    engine.camera_mode = CameraMode::Fly;
    engine.fly.position = Vec3::new(0.0, 1.7, 6.0);
    engine.fly.yaw = 0.0;
    engine.focus_scene();
    let _ = engine.camera().position;
    let _ = engine.aspect();
    let _ = engine.size();
}

/// §3.7 光照、天空、雾、曝光。
fn lighting(engine: &mut Engine) {
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
    l.shadow_strength = 1.0;
    l.sky = SkyParams::default();
    l.fog = FogParams::default();
    // 点光源：最多 MAX_POINT_LIGHTS 个，用 new(位置, 颜色, 强度, 半径)
    l.point_lights.push(PointLight::new(
        Vec3::new(0.0, 4.0, 0.0),
        Vec3::new(1.0, 0.9, 0.7),
        8.0,
        20.0,
    ));
}

/// §3.8 材质。
fn materials(engine: &mut Engine) {
    let _ = engine.assets.solid_material(
        &mut engine.renderer,
        &engine.gpu,
        Vec4::new(0.85, 0.45, 0.25, 1.0),
        0.05,
        0.45,
    );
    let _ = engine.assets.solid_material_linear(
        &mut engine.renderer,
        &engine.gpu,
        Vec4::new(0.85, 0.45, 0.25, 1.0),
        0.0,
        0.8,
    );
    // 完整描述要走具名材质（名字是缓存键，重名会复用）
    let _ = engine.assets.material(
        &mut engine.renderer,
        &engine.gpu,
        "我的材质",
        MaterialDesc {
            base_color: Vec4::splat(1.0),
            ..Default::default()
        },
    );
    // 内置网格（字段，不是方法）；单位尺寸，用 Transform 缩放
    let _ = engine.assets.unit_cube;
    let _ = engine.assets.unit_sphere;
    let _ = engine.assets.unit_cylinder;
    let _ = engine.assets.unit_capsule;
    let _ = engine.assets.unit_plane;
    let _ = engine.assets.cached_counts();
}

/// §3.9 截图与离屏渲染。
fn captures(engine: &mut Engine, config: &EngineConfig) -> Result<(), Box<dyn std::error::Error>> {
    engine.screenshot(1920, 1080, "shot.png")?;
    let image = quench3d::render_offscreen(1280, 720, config, |engine| {
        engine.debug.axes = true;
    })
    .map_err(|e| format!("离屏渲染失败：{e}"))?;
    quench3d::write_png(&image, "out.png")?;
    let (image2, stats) = quench3d::render_offscreen_stats(640, 360, config, |_engine| {})
        .map_err(|e| format!("离屏渲染失败：{e}"))?;
    println!(
        "{}x{} 画了 {} 个绘制调用",
        image2.width, image2.height, stats.draw_calls
    );
    Ok(())
}

/// §3.10 调试可视化与运行时状态。
fn debug(engine: &mut Engine) {
    engine.debug.axes = true;
    engine.debug.physics_contacts = true;
    engine.debug.velocities = true;
    engine.debug.body_bounds = true;
    engine.debug.enabled = false;
    println!(
        "t={:.2}s frames={} fps={:.0}",
        engine.time, engine.frame_count, engine.fps
    );
    engine.paused = true;
    engine.set_title("标题");
    engine.request_redraw();
}

/// §3.11 性能阶段计时。
fn timings(engine: &mut Engine) {
    engine.world.set_phase_timing(true);
    engine.world.step(1.0 / 60.0);
    let t = engine.world.phase_timings();
    println!(
        "整步 {:.3} ms（宽相 {:.3} / 窄相 {:.3} / 速度迭代 {:.3} / 位置迭代 {:.3}）",
        t.total_us / 1000.0,
        t.broadphase_us / 1000.0,
        t.narrowphase_us / 1000.0,
        t.velocity_us / 1000.0,
        t.position_us / 1000.0
    );
    println!(
        "{} 步、最近一步 {} 个岛（最大 {}，多体岛 {}）",
        t.steps, t.islands, t.largest_island, t.multi_body_islands
    );
    engine.world.set_phase_timing(false);
}

/// §1 / §2 的 `Game` 钩子与 `App` 两种组装方式。
struct Skeleton;

impl Game for Skeleton {
    fn init(&mut self, _engine: &mut Engine) {}
    fn update(&mut self, _engine: &mut Engine, _dt: f32) {}
    fn fixed_update(&mut self, engine: &mut Engine, dt: f32) {
        engine.world.step(dt);
    }
    fn draw(&mut self, _engine: &mut Engine) {}
}

fn closures() {
    let _app = App::new(
        |engine: &mut Engine| {
            let _ = engine.assets.unit_cube;
        },
        |engine: &mut Engine, dt: f32| {
            engine.world.step(dt);
        },
    );
    let _game = Skeleton;
    let _config = EngineConfig {
        title: "我的场景".to_string(),
        width: 1280,
        height: 720,
        ..Default::default()
    };
}

/// `Game` 的默认实现要全有（写代码时只覆盖需要的钩子）。
fn default_hooks_compile() {
    struct OnlyFixed;
    impl Game for OnlyFixed {
        fn fixed_update(&mut self, engine: &mut Engine, dt: f32) {
            engine.world.step(dt);
        }
    }
    let _ = OnlyFixed;
}

/// 这个测试文件的价值全在「编译通过」，运行时不碰 GPU。
#[test]
fn doc_snippets_compile() {
    // 故意留空：`cargo test` 能跑起来、`cargo check --all-targets` 钉住签名。
}
