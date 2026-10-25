//! 摄像头/视频背景：它必须在清屏色之上、场景之下，并按 cover 裁切而不是拉伸。

use quench3d_math::Vec3;
use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;
use quench3d_render::GpuOptions;

const W: u32 = 64;
const H: u32 = 32;

fn color_at(image: &ImageData, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * image.width + x) * 4) as usize;
    [
        image.rgba[i],
        image.rgba[i + 1],
        image.rgba[i + 2],
        image.rgba[i + 3],
    ]
}

#[test]
fn background_replaces_the_clear_color_and_crops_to_cover() {
    let ctx = with_gpu_tests("摄像头背景", || {
        pollster::block_on(GpuContext::headless(
            W,
            H,
            GpuOptions {
                msaa_samples: 1,
                ..Default::default()
            },
        ))
    });
    let Some(ctx) = ctx else { return };
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
            sky: true,
            ..Default::default()
        },
    );
    // 2×2：左半红、右半绿。窗口是 2:1，图像是 1:1；cover 应裁掉上下，
    // 保留完整的左右两半，且颜色不能被拉成渐变。
    let rgba = [
        220, 20, 20, 255, 20, 220, 20, 255, 220, 20, 20, 255, 20, 220, 20, 255,
    ];
    assert!(renderer.set_background_rgba(&ctx, 2, 2, &rgba));
    assert!(renderer.has_background());
    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 0.0, 3.0);
    camera.look_at(Vec3::ZERO);
    let lighting = Lighting::none();
    let image = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("背景渲染");
    let left = color_at(&image, 8, 16);
    let right = color_at(&image, W - 8, 16);
    assert!(left[0] > left[1] + 40, "左侧应来自红色半边，实际 {left:?}");
    assert!(
        right[1] > right[0] + 40,
        "右侧应来自绿色半边，实际 {right:?}"
    );

    // 场景几何必须盖在摄像头背景上（背景在天空之前、几何之后）。
    let mesh = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let mut desc = MaterialDesc::from_srgb(0.1, 0.1, 1.0, 1.0).unlit();
    desc.emissive = Vec3::Z;
    let material = renderer.add_material(&ctx, desc);
    renderer.begin_frame();
    renderer.draw(mesh, material, InstanceDesc::new(Transform::IDENTITY));
    let covered = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("背景+几何渲染");
    let center = color_at(&covered, W / 2, H / 2);
    assert!(
        center[2] > center[0] + 40,
        "中心应被蓝色立方体盖住，实际 {center:?}"
    );

    renderer.clear_background();
    assert!(!renderer.has_background());
    let cleared = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("清背景渲染");
    let corner = color_at(&cleared, 8, 16);
    assert!(
        corner != left && corner != right,
        "清背景后不该还是摄像头画面，实际 {corner:?}"
    );
}
