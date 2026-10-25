//! 加权混合 OIT（WBOIT）的行为验证。
//!
//! 两个半透明立方体占据**同一深度**、颜色不同：旧 alpha 管线的结果由绘制顺序
//! 决定（总有一种颜色压住另一种）；WBOIT 与顺序无关，中心应同时保留两种颜色。
//! 这正是 OIT 要解决的“玻璃叠玻璃”问题。

use quench3d_math::Vec3;
use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;
use quench3d_render::{GpuOptions, RendererOptions};

const W: u32 = 96;
const H: u32 = 96;

fn setup(oit: bool, msaa_samples: u32) -> Option<(GpuContext, Renderer)> {
    let ctx = with_gpu_tests("WBOIT 半透明", || {
        pollster::block_on(GpuContext::headless(
            W,
            H,
            GpuOptions {
                msaa_samples,
                ..Default::default()
            },
        ))
    })?;
    let ctx = match ctx {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            return None;
        }
    };
    let renderer = Renderer::new(
        &ctx,
        RendererOptions {
            grid: false,
            shadows: false,
            sky: false,
            oit,
            ..Default::default()
        },
    );
    Some((ctx, renderer))
}

fn pixel(image: &ImageData, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * image.width + x) * 4) as usize;
    [
        image.rgba[i],
        image.rgba[i + 1],
        image.rgba[i + 2],
        image.rgba[i + 3],
    ]
}

#[test]
fn oit_blends_same_depth_transparents_independently_of_draw_order() {
    let Some((mut ctx, mut renderer)) = setup(true, 1) else {
        return;
    };
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let mk = |renderer: &mut Renderer, r: f32, b: f32| {
        renderer.add_material(
            &ctx,
            MaterialDesc::from_srgb(r, 0.0, b, 0.5)
                .unlit()
                .alpha_blend(),
        )
    };
    // 材质下标决定同深度时的绘制顺序：A 是红(0)→蓝(1)，B 是蓝(1)→红(2)。
    let red0 = mk(&mut renderer, 1.0, 0.0);
    let blue1 = mk(&mut renderer, 0.0, 1.0);
    let red2 = mk(&mut renderer, 1.0, 0.0);

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 0.0, 5.0);
    camera.look_at(Vec3::ZERO);
    let mut lighting = Lighting::none();
    lighting.sky.zenith = Vec3::ZERO;
    lighting.sky.horizon = Vec3::ZERO;
    lighting.sky.ground = Vec3::ZERO;

    renderer.draw(cube, red0, InstanceDesc::new(Transform::IDENTITY));
    renderer.draw(cube, blue1, InstanceDesc::new(Transform::IDENTITY));
    let a = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("OIT 顺序 A");
    renderer.begin_frame();
    renderer.draw(cube, blue1, InstanceDesc::new(Transform::IDENTITY));
    renderer.draw(cube, red2, InstanceDesc::new(Transform::IDENTITY));
    let b = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("OIT 顺序 B");

    let ca = pixel(&a, W / 2, H / 2);
    let cb = pixel(&b, W / 2, H / 2);
    for k in 0..3 {
        assert!(
            ca[k].abs_diff(cb[k]) <= 4,
            "OIT 必须与绘制顺序无关：A={ca:?} B={cb:?}"
        );
    }

    // 对照：关掉 OIT，同样的两种顺序必须给出不同结果。
    renderer.options.oit = false;
    renderer.begin_frame();
    renderer.draw(cube, red0, InstanceDesc::new(Transform::IDENTITY));
    renderer.draw(cube, blue1, InstanceDesc::new(Transform::IDENTITY));
    let la = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("旧 alpha 顺序 A");
    renderer.begin_frame();
    renderer.draw(cube, blue1, InstanceDesc::new(Transform::IDENTITY));
    renderer.draw(cube, red2, InstanceDesc::new(Transform::IDENTITY));
    let lb = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("旧 alpha 顺序 B");
    let lca = pixel(&la, W / 2, H / 2);
    let lcb = pixel(&lb, W / 2, H / 2);
    assert!(
        lca != lcb,
        "旧顺序混合应当在两种顺序下不同（作为对照），实际 A={lca:?} B={lcb:?}"
    );
}

/// MSAA > 1 时 OIT 的 depth test 必须用解析后的单采样深度：目标本身是单采样，
/// 直接绑多采样深度会创建管线/开始 pass 时被 wgpu 拒绝。
#[test]
fn oit_works_with_msaa4_resolved_depth() {
    let Some((mut ctx, mut renderer)) = setup(true, 4) else {
        return;
    };
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let red = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(1.0, 0.0, 0.0, 0.5)
            .unlit()
            .alpha_blend(),
    );
    let blue = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(0.0, 0.0, 1.0, 0.5)
            .unlit()
            .alpha_blend(),
    );
    renderer.draw(cube, red, InstanceDesc::new(Transform::IDENTITY));
    renderer.draw(cube, blue, InstanceDesc::new(Transform::IDENTITY));
    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 0.0, 5.0);
    camera.look_at(Vec3::ZERO);
    let mut lighting = Lighting::none();
    lighting.sky.zenith = Vec3::ZERO;
    lighting.sky.horizon = Vec3::ZERO;
    lighting.sky.ground = Vec3::ZERO;
    let image = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("MSAA=4 OIT");
    let c = pixel(&image, W / 2, H / 2);
    assert!(
        c[0] > 35 && c[2] > 35,
        "MSAA=4 的 OIT 中心必须同时含红/蓝，实际 {c:?}"
    );
}
