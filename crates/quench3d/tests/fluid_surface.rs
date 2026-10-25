//! 流体自由表面：从真实 PBF 粒子提取的网格必须能上传到渲染器并替换占位网格。

use quench3d::prelude::*;
use quench3d_physics::{FluidDesc, PhysicsWorld};
use quench3d_render::context::with_gpu_tests;
use quench3d_render::{GpuContext, GpuOptions, Renderer, RendererOptions};

#[test]
fn fluid_surface_replaces_the_placeholder_mesh() {
    let ctx = with_gpu_tests("流体自由表面", || {
        pollster::block_on(GpuContext::headless(
            32,
            32,
            GpuOptions {
                msaa_samples: 1,
                ..Default::default()
            },
        ))
    });
    let Some(ctx) = ctx else { return };
    let ctx = match ctx {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            return;
        }
    };
    let mut renderer = Renderer::new(
        &ctx,
        RendererOptions {
            grid: false,
            shadows: false,
            sky: false,
            ..Default::default()
        },
    );
    let mut world = PhysicsWorld::new();
    world.set_fixed_dt(1.0 / 60.0);
    world.add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO));
    world
        .add_fluid(FluidDesc {
            center: Vec3::new(0.0, 0.35, 0.0),
            half_extents: Vec3::splat(0.25),
            spacing: 0.08,
            radius: 0.04,
            ..FluidDesc::default()
        })
        .expect("流体参数合法");
    for _ in 0..12 {
        world.step(1.0 / 60.0);
    }

    let placeholder = renderer.add_mesh(&ctx, &MeshData::cube(0.1));
    let material = renderer.add_material(
        &ctx,
        MaterialDesc {
            alpha_blend: true,
            double_sided: true,
            ..MaterialDesc::default()
        },
    );
    let mut scene = Scene::new();
    let object = scene.add_fluid_mesh(placeholder, material, 0, 24, 0.45, "流体表面");
    assert_eq!(
        renderer.mesh_index_count(placeholder),
        Some(36),
        "占位立方体应有 36 个索引"
    );
    let updated = scene.refresh_fluid_surfaces(&world, &mut renderer, &ctx);
    assert_eq!(updated, 1, "应更新一个连续水面");
    let count = renderer.mesh_index_count(placeholder).unwrap();
    assert!(count > 100, "自由表面应有足够三角形，实际 {count} 个索引");
    assert_eq!(scene.objects[object].transform, Transform::IDENTITY);
    // 流体没步进时版本号不变，不应重新提取/上传。
    assert_eq!(scene.refresh_fluid_surfaces(&world, &mut renderer, &ctx), 0);
    world.step(1.0 / 60.0);
    assert_eq!(scene.refresh_fluid_surfaces(&world, &mut renderer, &ctx), 1);
}
