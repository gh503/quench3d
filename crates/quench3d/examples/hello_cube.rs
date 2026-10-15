//! 最小示例：一个旋转的立方体 + 地面 + 实时阴影。
//!
//! 运行：
//! ```text
//! cargo run --release --example hello_cube
//! ```
//! 操作：中键/Alt+左键拖拽旋转视角，滚轮缩放，Tab 切换自由视角。

use quench3d::prelude::*;
use std::f32::consts::FRAC_PI_4;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = EngineConfig {
        title: "Quench3D — 你好，立方体".to_string(),
        width: 1280,
        height: 720,
        ..Default::default()
    };

    quench3d::run(
        config,
        App::new(
            // ---- 只执行一次：搭建场景 ----
            |engine: &mut Engine| {
                // 一个吃光的球体当作太阳方向指示物的反面：这里放地面 + 立方体
                let ground = engine.assets.unit_cube;
                let ground_mat = engine.assets.solid_material(
                    &mut engine.renderer,
                    &engine.gpu,
                    Vec4::new(0.42, 0.44, 0.48, 1.0),
                    0.0,
                    0.9,
                );
                engine.scene.add_named(
                    ground,
                    ground_mat,
                    Transform::new(
                        Vec3::new(0.0, -0.5, 0.0),
                        Quat::IDENTITY,
                        Vec3::new(14.0, 1.0, 14.0),
                    ),
                    "地面",
                );

                // 物理世界里也放一块静态地面，方便以后加刚体
                engine
                    .world
                    .add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }));

                let cube = engine.assets.unit_cube;
                let cube_mat = engine.assets.solid_material(
                    &mut engine.renderer,
                    &engine.gpu,
                    Vec4::new(0.90, 0.32, 0.22, 1.0),
                    0.15,
                    0.35,
                );
                engine.scene.add_named(
                    cube,
                    cube_mat,
                    Transform::new(Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY, Vec3::splat(1.4)),
                    "立方体",
                );

                engine.focus_scene();
            },
            // ---- 每帧 ----
            |engine: &mut Engine, dt: f32| {
                if let Some(index) = engine.scene.find("立方体") {
                    let t = engine.scene.objects[index].transform;
                    let yaw = engine.time * 0.8;
                    let pitch = (engine.time * 0.5).sin() * FRAC_PI_4;
                    engine.scene.objects[index].transform =
                        Transform::new(t.position, Quat::from_euler_yxz(yaw, pitch, 0.0), t.scale);
                }
                let _ = dt;
            },
        ),
    )
}
