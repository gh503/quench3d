//! 逐三角形拾取：旋转过的长条包围盒会盖住大片"其实没有面"的区域，
//! 点在这些空处不能选中它。没有 GPU 或没开 `QUENCH3D_GPU_TESTS=1` 时整条跳过。
#![cfg(not(target_arch = "wasm32"))]

use quench3d::prelude::*;
use quench3d::render::context::with_gpu_tests;
use quench3d_editor::doc::Doc;

#[test]
fn picking_uses_triangles_not_the_loose_aabb() {
    let gpu = with_gpu_tests("编辑器逐三角形拾取", || {
        pollster::block_on(quench3d::render::GpuContext::headless(
            64,
            64,
            Default::default(),
        ))
    });
    let Some(gpu) = gpu else {
        return;
    };
    let gpu = match gpu {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            return;
        }
    };
    let mut engine = Engine::new(
        gpu,
        &EngineConfig {
            width: 64,
            height: 64,
            renderer: quench3d::render::RendererOptions {
                keep_cpu_meshes: true,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let mesh = engine.assets.unit_cube;
    let material = engine.assets.solid_material(
        &mut engine.renderer,
        &engine.gpu,
        Vec4::new(0.8, 0.4, 0.2, 1.0),
        0.0,
        0.5,
    );
    // 2.0 × 0.2 × 0.2 的长条，绕 Y 转 45°：包围盒是一个 ~1.56 见方的方块，
    // 但真实几何只是其中一条对角线。
    let mut transform = Transform::from_scale(Vec3::new(2.0, 0.2, 0.2));
    transform.rotation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_4);
    let object = engine.scene.add(mesh, material, transform);
    let doc = Doc::new();

    let center = Ray::new(Vec3::new(0.0, 10.0, 0.0), Vec3::NEG_Y);
    assert_eq!(
        doc.pick(&engine, &center),
        Some(object),
        "中心射线必须命中长条"
    );

    // 这个点在包围盒内，但在旋转后的长条外（对角线的另一侧）。
    let corner = Ray::new(Vec3::new(0.7, 10.0, 0.7), Vec3::NEG_Y);
    let bounds = engine.scene.bounds_of(&engine.renderer, &[object]).unwrap();
    assert!(
        corner.intersect_aabb(&bounds).is_some(),
        "测试前提：这个点必须在包围盒内"
    );
    assert_eq!(
        doc.pick(&engine, &corner),
        None,
        "包围盒内的空白处不能选中长条"
    );

    // 直接查渲染器的三角形命中，最近表面在 y=+0.1。
    let hit = engine
        .renderer
        .raycast_mesh(mesh, &transform.matrix(), &center)
        .expect("中心射线应有三角形命中");
    assert!((hit - 10.0).abs() < 0.2, "命中参数应到顶面，实际 {hit}");
}
