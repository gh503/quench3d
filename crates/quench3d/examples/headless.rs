//! 无窗口集成验证：物理 + 场景 + 渲染串起来跑一遍，输出 PNG。
//!
//! ```text
//! cargo run --release --example headless -- [输出目录]
//! ```
//!
//! 它做三件事：
//! 1. 搭一个「地面 + 金字塔箱子 + 球 + 斜面」的场景；
//! 2. 用固定步长推进物理若干秒，并断言没有物体穿地、没有 NaN；
//! 3. 渲染 PNG 到 `target/screenshots/`，供人工/脚本比对。

#[cfg(not(target_arch = "wasm32"))]
use quench3d::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use std::f32::consts::FRAC_PI_6;

#[cfg(target_arch = "wasm32")]
fn main() {
    eprintln!("headless 示例仅在桌面端运行（需要文件系统）");
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/screenshots".to_string());
    let suffix = std::env::var("F3D_SUFFIX").unwrap_or_default();
    std::fs::create_dir_all(&out_dir).ok();

    let config = EngineConfig {
        width: 900,
        height: 560,
        msaa_samples: 4,
        ..Default::default()
    };

    // ---------- 1. 第一次渲染：初始状态 ----------
    let mut settled_transforms = Vec::new();
    let before = quench3d::render_offscreen(900, 560, &config, |engine| {
        build_scene(engine);
        apply_debug_flags(engine);
        engine.scene.sync_from_physics(&engine.world);
        frame_camera(engine);
    })
    .map_err(|e| format!("离屏渲染失败: {e}"))?;
    quench3d::write_png(&before, format!("{out_dir}/engine_before{suffix}.png"))?;

    // ---------- 2. 推进物理 4 秒 ----------
    let after = quench3d::render_offscreen(900, 560, &config, |engine| {
        build_scene(engine);
        apply_debug_flags(engine);
        let dt = 1.0 / 60.0;
        let mut steps = 0;
        for _ in 0..240 {
            engine.world.step(dt);
            steps += 1;
        }
        engine.scene.sync_from_physics(&engine.world);
        frame_camera(engine);

        // 统计与断言
        let mut below_ground = 0;
        let mut dynamic = 0;
        let mut sleeping = 0;
        let mut nan = 0;
        let mut max_speed = 0.0f32;
        for (_, body) in engine.world.bodies() {
            let p = body.position();
            if !p.is_finite() || !body.linear_velocity().is_finite() {
                nan += 1;
                continue;
            }
            if body.is_dynamic() {
                dynamic += 1;
                if body.is_sleeping() {
                    sleeping += 1;
                }
                max_speed = max_speed.max(body.linear_velocity().length());
                // 半径/半高的粗略下界：中心不该跑到地面以下太多
                if p.y < -0.35 {
                    below_ground += 1;
                }
                settled_transforms.push(p);
            }
        }
        let cam = engine.camera();
        println!(
            "相机            : pos ({:.2}, {:.2}, {:.2}) fov {:.1}° near {:.3} far {:.1} 前向 ({:.2},{:.2},{:.2})",
            cam.position.x, cam.position.y, cam.position.z,
            cam.fov_y.to_degrees(), cam.near, cam.far,
            cam.forward().x, cam.forward().y, cam.forward().z
        );
        println!("物理步数        : {steps}");
        println!("动态刚体        : {dynamic}（已休眠 {sleeping}）");
        println!("穿地刚体        : {below_ground}");
        println!("NaN/Inf 刚体    : {nan}");
        println!("最大速度        : {max_speed:.3} m/s");
        println!("接触流形        : {}", engine.world.contacts().len());
        if std::env::var("F3D_LIST").is_ok() {
            for obj in &engine.scene.objects {
                let t = obj.transform;
                println!(
                    "  {:<10} pos ({:6.2},{:6.2},{:6.2}) scale ({:5.2},{:5.2},{:5.2}) 可见 {}",
                    obj.name, t.position.x, t.position.y, t.position.z,
                    t.scale.x, t.scale.y, t.scale.z, obj.visible
                );
            }
        }

        // 角色控制器：让一个胶囊沿 +X 走 40 步
        let mut controller = CharacterController {
            radius: 0.4,
            height: 1.8,
            ..Default::default()
        };
        controller.snap_to_ground = true;
        let mut feet = Vec3::new(-6.0, 0.1, -6.0);
        let mut grounded_frames = 0;
        for _ in 0..120 {
            let result = controller.move_and_slide(&mut engine.world, feet, Vec3::new(0.05, -0.02, 0.0));
            feet += result.translation;
            if result.grounded {
                grounded_frames += 1;
            }
        }
        println!(
            "角色控制器      : 走到 ({:.2}, {:.2}, {:.2})，落地帧 {grounded_frames}/120",
            feet.x, feet.y, feet.z
        );
        assert_eq!(nan, 0, "物理产生了 NaN/Inf");
        assert_eq!(below_ground, 0, "有刚体穿过了地面");
        assert!(feet.y > -0.4, "角色掉出世界了: y={}", feet.y);
    })
    .map_err(|e| format!("离屏渲染失败: {e}"))?;
    quench3d::write_png(&after, format!("{out_dir}/engine_after{suffix}.png"))?;

    println!("已输出 {out_dir}/engine_before{suffix}.png 与 {out_dir}/engine_after{suffix}.png");
    Ok(())
}

/// 地面方块大小（可用 `F3D_GROUND=<n>` 调整，用于排查可视范围）。
#[cfg(not(target_arch = "wasm32"))]
fn ground_scale() -> f32 {
    std::env::var("F3D_GROUND")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(70.0)
}

/// 固定机位：离得近一点，让地面填满下半屏。
/// 调试开关：`F3D_NO_SKY=1 F3D_NO_GRID=1 F3D_NO_GROUND=1`。
#[cfg(not(target_arch = "wasm32"))]
fn apply_debug_flags(engine: &mut Engine) {
    if std::env::var("F3D_NO_SKY").is_ok() {
        engine.scene.lighting.sky.enabled = false;
    }
    if std::env::var("F3D_NO_GRID").is_ok() {
        engine.scene.lighting.sky.show_grid = false;
    }
    if std::env::var("F3D_NO_GROUND").is_ok() {
        if let Some(i) = engine.scene.find("地面") {
            engine.scene.objects[i].visible = false;
        }
    }
    if std::env::var("F3D_NO_SHADOW").is_ok() {
        if let Some(sun) = engine.scene.lighting.sun.as_mut() {
            sun.cast_shadows = false;
        }
    }
    if let Ok(v) = std::env::var("F3D_EXPOSURE") {
        if let Ok(x) = v.parse::<f32>() {
            engine.scene.lighting.exposure = x;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn frame_camera(engine: &mut Engine) {
    engine.orbit.focus = Vec3::new(0.0, 1.2, 0.0);
    engine.orbit.distance = 15.0;
    engine.orbit.yaw = 0.62;
    engine.orbit.pitch = 0.30;
}

/// 搭一个能同时考验窄相、约束与睡眠的场景。
#[cfg(not(target_arch = "wasm32"))]
fn build_scene(engine: &mut Engine) {
    let box_mesh = engine.assets.unit_cube;
    let sphere_mesh = engine.assets.unit_sphere;
    let cyl_mesh = engine.assets.unit_cylinder;

    let colors = [
        Vec4::new(0.88, 0.42, 0.28, 1.0),
        Vec4::new(0.26, 0.60, 0.88, 1.0),
        Vec4::new(0.42, 0.80, 0.46, 1.0),
        Vec4::new(0.94, 0.78, 0.26, 1.0),
    ];
    let mats: Vec<MaterialId> = colors
        .iter()
        .enumerate()
        .map(|(i, c)| {
            engine.assets.solid_material(
                &mut engine.renderer,
                &engine.gpu,
                *c,
                0.1,
                0.35 + 0.1 * i as f32,
            )
        })
        .collect();

    // 静态地面 + 视觉地板
    engine
        .world
        .add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }));
    let floor_mat = engine.assets.solid_material(
        &mut engine.renderer,
        &engine.gpu,
        Vec4::new(0.48, 0.50, 0.54, 1.0),
        0.0,
        0.9,
    );
    if std::env::var("F3D_PLANE_GROUND").is_ok() {
        engine.scene.add_named(
            engine.assets.unit_plane,
            floor_mat,
            Transform::new(
                Vec3::ZERO,
                Quat::IDENTITY,
                Vec3::new(ground_scale(), 1.0, ground_scale()),
            ),
            "地面",
        );
    } else {
        engine.scene.add_named(
            box_mesh,
            floor_mat,
            Transform::new(
                Vec3::new(0.0, -0.5, 0.0),
                Quat::IDENTITY,
                Vec3::new(ground_scale(), 1.0, ground_scale()),
            ),
            "地面",
        );
    }

    // 斜面
    let ramp_half = Vec3::new(3.5, 0.25, 2.5);
    let ramp = engine.world.add_builder(
        BodyBuilder::r#static(Shape::Box {
            half_extents: ramp_half,
        })
        .transform(Transform::new(
            Vec3::new(-5.5, 1.6, 0.0),
            Quat::from_euler_yxz(0.0, 0.0, -FRAC_PI_6),
            Vec3::ONE,
        )),
    );
    let index = engine
        .scene
        .add_with_body(box_mesh, mats[1], ramp, &engine.world, "斜面");
    engine.scene.objects[index].transform.scale = ramp_half * 2.0;

    if std::env::var("F3D_ONLY_GROUND").is_ok() {
        return;
    }

    // 金字塔：5 层箱子
    let half = 0.5;
    for level in 0..5 {
        let count = 5 - level;
        for i in 0..count {
            let x = (i as f32 - (count as f32 - 1.0) * 0.5) * (half * 2.0 + 0.03);
            let y = half + level as f32 * (half * 2.0 + 0.004);
            let body = engine.world.add_builder(
                BodyBuilder::dynamic(Shape::Box {
                    half_extents: Vec3::splat(half),
                })
                .position(Vec3::new(x, y, 2.0))
                .friction(0.6)
                .restitution(0.05),
            );
            let idx = engine.scene.add_with_body(
                box_mesh,
                mats[level % mats.len()],
                body,
                &engine.world,
                "箱子",
            );
            engine.scene.objects[idx].transform.scale = Vec3::splat(half * 2.0);
        }
    }

    // 从斜面滚下的球
    for i in 0..4 {
        let radius = 0.45;
        let body = engine.world.add_builder(
            BodyBuilder::dynamic(Shape::Sphere { radius })
                .position(Vec3::new(-9.0, 5.0 + i as f32 * 1.2, 0.0))
                .friction(0.5)
                .restitution(0.35),
        );
        let idx = engine
            .scene
            .add_with_body(sphere_mesh, mats[2], body, &engine.world, "球");
        engine.scene.objects[idx].transform.scale = Vec3::splat(radius * 2.0);
    }

    // 悬空落下的圆柱
    for i in 0..3 {
        let body = engine.world.add_builder(
            BodyBuilder::dynamic(Shape::Cylinder {
                half_height: 0.6,
                radius: 0.4,
            })
            .position(Vec3::new(4.0 + i as f32 * 1.1, 4.0 + i as f32 * 0.8, -2.0))
            .friction(0.5),
        );
        let idx = engine
            .scene
            .add_with_body(cyl_mesh, mats[3], body, &engine.world, "圆柱");
        engine.scene.objects[idx].transform.scale = Vec3::new(0.8, 1.2, 0.8);
    }
}
