//! `Rgba16Float` 浮点贴图的行为验证：HDR 值（> 1）必须真的进入着色器，
//! 而不是被 8 位路径截断/压暗。

use quench3d_math::Vec3;
use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;
use quench3d_render::{FloatImage, GpuOptions, RendererOptions};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn float_texture_keeps_values_above_one() {
    let ctx = with_gpu_tests("Rgba16Float 贴图", || {
        pollster::block_on(GpuContext::headless(
            W,
            H,
            GpuOptions {
                msaa_samples: 1,
                ..Default::default()
            },
        ))
    });
    let Some(ctx) = ctx else {
        return;
    };
    let mut ctx = match ctx {
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
    // 线性亮度 4.0：8 位 LDR 最大值是 1.0，只有浮点贴图能保留。
    let tex = renderer.add_texture_float(
        &ctx,
        &FloatImage::from_rgba32f(1, 1, vec![[0.0, 4.0, 0.0, 1.0]]).expect("浮点图"),
    );
    let mut desc = MaterialDesc::from_srgb(1.0, 1.0, 1.0, 1.0).unlit();
    desc.base_color_texture = Some(tex);
    let material = renderer.add_material(&ctx, desc);
    let mesh = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    renderer.begin_frame();
    renderer.draw(mesh, material, InstanceDesc::new(Transform::IDENTITY));

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 0.0, 3.0);
    camera.look_at(Vec3::ZERO);
    let mut lighting = Lighting::none();
    lighting.sky.zenith = Vec3::ZERO;
    lighting.sky.horizon = Vec3::ZERO;
    lighting.sky.ground = Vec3::ZERO;
    let image = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("浮点贴图渲染");
    let i = ((H / 2 * W + W / 2) * 4) as usize;
    let px = [
        image.rgba[i],
        image.rgba[i + 1],
        image.rgba[i + 2],
        image.rgba[i + 3],
    ];
    assert!(
        px[1] > 150 && px[0] < 80 && px[2] < 80,
        "线性 4.0 的绿光必须被 tonemap 成明显亮绿，实际 {px:?}"
    );
}
