//! 关节展示：铰链门（带马达）、滑块平台、弹簧、焊接、球窝摆。
//!
//! 桌面端：
//! ```text
//! cargo run --release --example joints
//! ```
//!
//! 操作：`Tab` 切相机、`R` 对准场景、`空格` 反向驱动铰链马达、`P` 暂停。
//! 五个展示区排成一排，从相机默认视角可以一次看全。

use quench3d::prelude::*;
use quench3d::CameraMode;

/// 每个展示区的中心 X 坐标。
const STATIONS: [f32; 5] = [-8.0, -4.0, 0.0, 4.0, 8.0];

struct JointsDemo {
    /// 铰链关节（空格切换马达正反转时重建它）。
    hinge: Option<JointHandle>,
    /// 铰链两端刚体，重建关节时需要。
    hinge_bodies: (BodyHandle, BodyHandle),
    motor_target: f32,
}

impl JointsDemo {
    fn new() -> Self {
        Self {
            hinge: None,
            hinge_bodies: (BodyHandle::new(0, 0), BodyHandle::new(0, 0)),
            motor_target: 1.5,
        }
    }

    /// 按当前 `motor_target` 重建铰链关节。
    fn rebuild_hinge(&mut self, world: &mut PhysicsWorld) {
        if let Some(handle) = self.hinge.take() {
            world.remove_joint(handle);
        }
        let (a, b) = self.hinge_bodies;
        self.hinge = Some(world.add_joint(Joint::Hinge {
            a,
            b,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: Vec3::new(-1.0, 0.0, 0.0),
            local_axis_a: Vec3::Y,
            local_axis_b: Vec3::Y,
            limits: Some((-2.4, 2.4)),
            motor: Some(HingeMotor {
                target_velocity: self.motor_target,
                max_impulse: 8.0,
            }),
        }));
    }
}

impl JointsDemo {
    /// 静态刚体 + 挂到场景里。
    fn add_static(
        engine: &mut Engine,
        mesh: MeshId,
        material: MaterialId,
        transform: Transform,
        name: &str,
    ) -> BodyHandle {
        let body = engine.world.add_builder(
            BodyBuilder::r#static(Shape::Box {
                half_extents: transform.scale * 0.5,
            })
            .position(transform.position)
            .rotation(transform.rotation),
        );
        let index = engine
            .scene
            .add_with_body(mesh, material, body, &engine.world, name);
        engine.scene.objects[index].transform = transform;
        body
    }

    /// 动态刚体 + 挂到场景里（形状尺寸与视觉缩放一致）。
    fn add_dynamic(
        engine: &mut Engine,
        mesh: MeshId,
        material: MaterialId,
        transform: Transform,
        shape: Shape,
        name: &str,
    ) -> BodyHandle {
        let body = engine.world.add_builder(
            BodyBuilder::dynamic(shape)
                .position(transform.position)
                .rotation(transform.rotation)
                .friction(0.6)
                .restitution(0.1),
        );
        let index = engine
            .scene
            .add_with_body(mesh, material, body, &engine.world, name);
        engine.scene.objects[index].transform = transform;
        body
    }
}

impl Game for JointsDemo {
    fn init(&mut self, engine: &mut Engine) {
        let cube = engine.assets.unit_cube;
        let plane = engine.assets.unit_plane;
        let mut solid = |r: f32, g: f32, b: f32, metallic: f32, roughness: f32| -> MaterialId {
            engine.assets.solid_material(
                &mut engine.renderer,
                &engine.gpu,
                Vec4::new(r, g, b, 1.0),
                metallic,
                roughness,
            )
        };
        let wood = solid(0.72, 0.52, 0.30, 0.0, 0.75);
        let steel = solid(0.62, 0.66, 0.72, 0.85, 0.28);
        let rubber = solid(0.30, 0.42, 0.62, 0.0, 0.65);
        let accent = solid(0.82, 0.34, 0.28, 0.15, 0.42);
        let ground_mat = solid(0.45, 0.48, 0.52, 0.0, 0.92);

        // 地面
        let ground = engine
            .world
            .add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }));
        let index = engine
            .scene
            .add_with_body(plane, ground_mat, ground, &engine.world, "地面");
        engine.scene.objects[index].transform.scale = Vec3::splat(40.0);

        // ---------------- ① 铰链门（带马达） ----------------
        let x = STATIONS[0];
        Self::add_static(
            engine,
            cube,
            steel,
            Transform::new(
                Vec3::new(x - 1.0, 1.0, 0.0),
                Quat::IDENTITY,
                Vec3::new(0.25, 2.0, 0.25),
            ),
            "门框",
        );
        let door = Self::add_dynamic(
            engine,
            cube,
            wood,
            Transform::new(
                Vec3::new(x + 0.5, 1.0, 0.0),
                Quat::IDENTITY,
                Vec3::new(2.0, 1.9, 0.15),
            ),
            Shape::Box {
                half_extents: Vec3::new(1.0, 0.95, 0.075),
            },
            "门",
        );
        // 铰链轴在门框一侧：锚点取门的左边缘（本地点 -X 方向）
        let frame = engine
            .scene
            .find("门框")
            .and_then(|i| engine.scene.objects[i].body)
            .expect("门框刚体");
        self.hinge_bodies = (frame, door);
        self.rebuild_hinge(&mut engine.world);

        // ---------------- ② 垂直滑块（带轴向限位） ----------------
        let x = STATIONS[1];
        let rail = Self::add_static(
            engine,
            cube,
            steel,
            Transform::new(
                Vec3::new(x - 0.35, 2.2, 0.0),
                Quat::IDENTITY,
                Vec3::new(0.16, 4.0, 0.16),
            ),
            "导轨",
        );
        let slider = Self::add_dynamic(
            engine,
            cube,
            accent,
            Transform::new(
                Vec3::new(x + 0.05, 3.6, 0.0),
                Quat::IDENTITY,
                Vec3::new(0.56, 0.56, 0.56),
            ),
            Shape::Box {
                half_extents: Vec3::splat(0.28),
            },
            "滑块",
        );
        // 轴向限位 ±1.5：滑块会在重力作用下落到下界，而不是无限下滑
        engine.world.add_joint(Joint::Slider {
            a: rail,
            b: slider,
            // 锚点取在导轨中心 + (0.40, 1.40, 0) = 滑块中心（创建时两端重合）
            local_anchor_a: Vec3::new(0.40, 1.40, 0.0),
            local_anchor_b: Vec3::ZERO,
            local_axis_a: Vec3::Y,
            local_axis_b: Vec3::Y,
            limits: Some((-1.5, 1.5)),
        });

        // ---------------- ③ 弹簧（软距离关节） ----------------
        let x = STATIONS[2];
        let anchor = Self::add_static(
            engine,
            cube,
            steel,
            Transform::new(
                Vec3::new(x, 3.2, 0.0),
                Quat::IDENTITY,
                Vec3::new(0.4, 0.2, 0.4),
            ),
            "弹簧挂点",
        );
        let bob = Self::add_dynamic(
            engine,
            cube,
            rubber,
            Transform::new(Vec3::new(x, 1.4, 0.0), Quat::IDENTITY, Vec3::splat(0.7)),
            Shape::Box {
                half_extents: Vec3::splat(0.35),
            },
            "弹簧配重",
        );
        engine.world.add_joint(Joint::Distance {
            a: anchor,
            b: bob,
            local_anchor_a: Vec3::new(0.0, -0.1, 0.0),
            local_anchor_b: Vec3::ZERO,
            rest_length: 1.2,
            stiffness: 55.0, // > 0 走弹簧，0 是刚性杆
            damping: 2.4,
        });

        // ---------------- ④ 焊接（固定关节） ----------------
        let x = STATIONS[3];
        let base = Self::add_dynamic(
            engine,
            cube,
            wood,
            Transform::new(
                Vec3::new(x, 1.0, 0.0),
                Quat::IDENTITY,
                Vec3::new(1.6, 0.3, 1.6),
            ),
            Shape::Box {
                half_extents: Vec3::new(0.8, 0.15, 0.8),
            },
            "焊接底座",
        );
        let top = Self::add_dynamic(
            engine,
            cube,
            accent,
            Transform::new(
                Vec3::new(x, 1.55, 0.0),
                Quat::from_rotation_y(0.4),
                Vec3::new(0.8, 0.8, 0.8),
            ),
            Shape::Box {
                half_extents: Vec3::splat(0.4),
            },
            "焊接块",
        );
        engine.world.add_joint(Joint::Fixed {
            a: base,
            b: top,
            local_anchor_a: Vec3::new(0.0, 0.15, 0.0),
            local_anchor_b: Vec3::new(0.0, -0.4, 0.0),
            local_frame_a: Quat::IDENTITY,
            local_frame_b: Quat::IDENTITY,
        });

        // ---------------- ⑤ 球窝摆（链条） ----------------
        let x = STATIONS[4];
        let mut previous = Self::add_static(
            engine,
            cube,
            steel,
            Transform::new(
                Vec3::new(x, 3.6, 0.0),
                Quat::IDENTITY,
                Vec3::new(0.3, 0.2, 0.3),
            ),
            "摆点",
        );
        for i in 0..3 {
            let y = 3.0 - i as f32 * 0.7;
            let link = Self::add_dynamic(
                engine,
                cube,
                if i % 2 == 0 { steel } else { rubber },
                Transform::new(Vec3::new(x, y, 0.0), Quat::IDENTITY, Vec3::splat(0.5)),
                Shape::Box {
                    half_extents: Vec3::splat(0.25),
                },
                "摆节",
            );
            engine.world.add_joint(Joint::Ball {
                a: previous,
                b: link,
                local_anchor_a: if i == 0 {
                    Vec3::new(0.0, -0.1, 0.0)
                } else {
                    Vec3::new(0.0, -0.35, 0.0)
                },
                local_anchor_b: Vec3::new(0.0, 0.35, 0.0),
            });
            if i == 2 {
                // 给摆一个初始推力，避免完全静止看不出效果
                if let Some(body) = engine.world.body_mut(link) {
                    body.apply_impulse(Vec3::new(3.5, 0.0, 0.0));
                }
            }
            previous = link;
        }

        engine.camera_mode = CameraMode::Orbit;
        engine.orbit.focus_on(&Aabb::from_center_half_extents(
            Vec3::new(0.0, 2.0, 0.0),
            Vec3::new(9.0, 2.0, 2.0),
        ));
        // 稍微抬高视角、拉开一点距离，五个展示区一次看全
        engine.orbit.distance = 12.5;
        engine.orbit.yaw = 0.42;
        engine.orbit.pitch = 0.26;
    }

    fn update(&mut self, engine: &mut Engine, _dt: f32) {
        if engine.input.key_pressed(KeyCode::Space) {
            self.motor_target = -self.motor_target;
            self.rebuild_hinge(&mut engine.world);
        }
    }

    fn draw(&mut self, engine: &mut Engine) {
        engine.debug.physics_contacts = true;
    }
}

/// 无窗口自检：跑 240 步，检查关节是否把物体拉住（不飞散、无 NaN），并出图。
///
/// ```text
/// F3D_JOINTS_HEADLESS=1 cargo run -p quench3d --example joints
/// ```
#[cfg(not(target_arch = "wasm32"))]
fn headless_check() -> Result<(), Box<dyn std::error::Error>> {
    let config = EngineConfig {
        width: 900,
        height: 520,
        ..Default::default()
    };
    let mut demo = JointsDemo::new();
    let image = quench3d::render_offscreen(900, 520, &config, |engine| {
        demo.init(engine);
        engine.camera_mode = CameraMode::Orbit;
        for step in 0..240 {
            engine.world.step(1.0 / 60.0);
            if step == 60 {
                demo.motor_target = -demo.motor_target;
                demo.rebuild_hinge(&mut engine.world);
            }
        }
        engine.scene.sync_from_physics(&engine.world);

        // 统计：有没有物体飞出去 / 出现 NaN
        let mut max_speed = 0.0f32;
        let mut max_dist = 0.0f32;
        let mut nan = 0;
        for (_, body) in engine.world.bodies() {
            let p = body.position();
            if !p.x.is_finite() || !p.y.is_finite() || !p.z.is_finite() {
                nan += 1;
                continue;
            }
            max_speed = max_speed.max(body.linear_velocity().length());
            max_dist = max_dist.max(Vec3::new(p.x, 0.0, p.z).length());
        }
        println!(
            "关节自检: 刚体 {} 关节 {} 接触 {} | 最大速度 {:.2} m/s 最远水平距离 {:.2} m NaN {}",
            engine.world.body_count(),
            engine.world.joint_count(),
            engine.world.contacts().len(),
            max_speed,
            max_dist,
            nan
        );
        for name in ["门", "滑块", "弹簧配重", "焊接块", "摆节"] {
            if let Some(i) = engine.scene.find(name) {
                let t = engine.scene.objects[i].transform.position;
                println!("  {name:<6} -> ({:6.2}, {:6.2}, {:6.2})", t.x, t.y, t.z);
            }
        }
        assert_eq!(nan, 0, "出现 NaN");
        assert!(max_dist < 30.0, "有物体飞离场景: {max_dist}");
        assert!(max_speed < 60.0, "速度异常: {max_speed}");
    })?;
    quench3d::write_png(&image, "target/screenshots/joints.png")?;
    println!("已输出 target/screenshots/joints.png");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var("F3D_JOINTS_HEADLESS").is_ok() {
        return headless_check();
    }
    quench3d::run(
        EngineConfig {
            title: "Quench3D — 关节展示".to_string(),
            width: 1280,
            height: 720,
            ..Default::default()
        },
        JointsDemo::new(),
    )
}
