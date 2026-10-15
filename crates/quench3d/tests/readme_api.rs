//! 文档 API 守卫：把 README 第 3 节的代码片段原样编译，防止文档与 API 漂移。
//!
//! 本文件没有断言——它能通过编译就说明 README 里出现的每个类型、方法、参数
//! 顺序都是真实存在的。改 API 时如果忘了改文档，这里会直接编译失败。
#![allow(dead_code, unused_variables, clippy::all)]

use quench3d::prelude::*;

/// README「最小可运行代码」：闭包形式直接传给 `quench3d::run`。
fn minimal_example() -> Result<(), Box<dyn std::error::Error>> {
    quench3d::run(
        EngineConfig {
            title: "我的第一个场景".into(),
            ..Default::default()
        },
        |_engine: &mut Engine, _dt: f32| {},
    )
}

/// README「场景与资源」。
fn scene_and_assets(
    engine: &mut Engine,
    world: &mut PhysicsWorld,
    body: BodyHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let mesh = engine.assets.unit_cube;
    let mat = engine.assets.solid_material(
        &mut engine.renderer,
        &engine.gpu,
        Vec4::new(0.9, 0.4, 0.3, 1.0),
        0.0,
        0.6,
    );
    let id = engine
        .scene
        .add(mesh, mat, Transform::from_translation(Vec3::Y * 2.0));
    engine.scene.objects[id].transform.scale = Vec3::new(2.0, 1.0, 2.0);
    engine.scene.objects[id].visible = false;

    // 带物理体：之后 sync_from_physics 会自动把刚体位姿写回变换
    engine.scene.add_with_body(mesh, mat, body, world, "箱子");

    // 自定义网格与材质
    let mesh = engine
        .renderer
        .add_mesh(&engine.gpu, &MeshData::torus(1.0, 0.3, 32, 16));
    let mat = engine.renderer.add_material(
        &engine.gpu,
        MaterialDesc::from_srgb(1.0, 0.8, 0.2, 1.0).pbr(0.15, 0.9),
    );

    // glTF / glb / OBJ 模型：自动按扩展名选择解析器
    let ids = engine.load_model("chair.glb", Transform::from_translation(Vec3::Y * 1.0))?;
    engine
        .orbit
        .focus_on(&engine.scene.bounds_of(&engine.renderer, &ids).unwrap());

    // 关卡地形：同时生成静态三角网格碰撞体
    let (ids, terrain) = engine.load_model_as_static("level.obj", Transform::IDENTITY, 0.9)?;
    Ok(())
}

/// README「物理」。
fn physics(world: &mut PhysicsWorld) {
    let shapes = [
        Shape::Sphere { radius: 0.5 },
        Shape::Box {
            half_extents: Vec3::splat(0.5),
        },
        Shape::Capsule {
            half_height: 0.6,
            radius: 0.3,
        },
        Shape::Cylinder {
            half_height: 0.5,
            radius: 0.5,
        },
        Shape::ConvexHull {
            points: vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z],
        },
        Shape::Plane { normal: Vec3::Y },
        Shape::TriMesh {
            vertices: vec![Vec3::ZERO, Vec3::X, Vec3::Z],
            indices: vec![0, 1, 2],
        },
    ];
    let _ = shapes;

    let body = world.add_builder(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
            .position(Vec3::Y * 5.0)
            .density(1.0)
            .restitution(0.6)
            .friction(0.4)
            .linear_damping(0.02)
            .can_sleep(true),
    );

    world.step(1.0 / 60.0);
    let dt = 1.0 / 60.0;
    world.step_with_accumulator(dt);

    if let Some(hit) = world.raycast(Vec3::ZERO, Vec3::Y, 100.0) {
        println!(
            "命中 {:?} 于 {:?}，距离 {}",
            hit.handle, hit.point, hit.distance
        );
    }
    for c in world.contacts() {
        println!(
            "{:?} <-> {:?} 法线 {:?} 冲量 {}",
            c.a, c.b, c.normal, c.impulse
        );
    }

    // 关节
    let a = BodyHandle::new(0, 0);
    let _ = Joint::Hinge {
        a,
        b: a,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        local_axis_a: Vec3::Y,
        local_axis_b: Vec3::Y,
        limits: Some((-1.0, 1.0)),
        motor: None,
    };

    // 角色控制器
    let character = CharacterController {
        radius: 0.35,
        height: 1.7,
        ..Default::default()
    };
    let result = character.move_and_slide(world, Vec3::ZERO, Vec3::Z);
    if result.grounded {}
}

/// README「相机与输入」。
fn camera_and_input(engine: &mut Engine, bounds: Aabb) {
    engine.camera_mode = CameraMode::Orbit;
    engine.orbit.focus_on(&bounds);
    engine.fly.speed = 12.0;

    if engine.input.key_down(KeyCode::KeyW) {}
    if engine.input.mouse_button_pressed(MouseButton::Left) {
        let ray = engine
            .camera()
            .screen_ray(engine.input.mouse_pos(), engine.viewport());
        if let Some(hit) = engine.world.raycast(ray.origin, ray.dir, 200.0) {
            println!("点到了 {:?}", hit.handle);
        }
    }
    let _ = engine.input.mouse_motion();
    let _ = engine.input.scroll();
    let _ = engine.screenshot(1920, 1080, "shot.png");
}

#[test]
fn readme_snippets_compile() {
    // 编译通过即为通过：本文件就是 README 的 API 使用者。
}
