//! 盒投影贴花的行为验证：贴花必须落在**深度缓冲里的表面**上，而不是一张浮空四边形。

use quench3d_math::{vec3, Vec3};
use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;
use quench3d_render::{DecalDesc, GpuOptions, RendererOptions};

const W: u32 = 96;
const H: u32 = 96;

fn setup(msaa_samples: u32) -> Option<(GpuContext, Renderer)> {
    let ctx = with_gpu_tests("盒投影贴花", || {
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
    let mut renderer = Renderer::new(
        &ctx,
        RendererOptions {
            grid: false,
            shadows: false,
            sky: false,
            ..Default::default()
        },
    );
    // 一块大而平的地面：顶面 y = 0。
    let floor_mesh = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let floor_mat =
        renderer.add_material(&ctx, MaterialDesc::from_srgb(0.45, 0.45, 0.45, 1.0).unlit());
    renderer.draw(
        floor_mesh,
        floor_mat,
        InstanceDesc::new(Transform {
            position: vec3(0.0, -0.5, 0.0),
            scale: vec3(40.0, 1.0, 40.0),
            ..Transform::IDENTITY
        }),
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
fn projected_decal_lands_on_the_depth_surface_and_can_be_removed() {
    let Some((mut ctx, mut renderer)) = setup(1) else {
        return;
    };
    let texture = renderer.add_texture(
        &ctx,
        &ImageData::from_rgba8(4, 4, [255, 0, 0, 255].repeat(16)).expect("贴花纹理"),
        true,
    );
    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 8.0, 8.0);
    camera.look_at(Vec3::ZERO);
    let lighting = Lighting {
        exposure: 1.0,
        ..Default::default()
    };

    let base = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("无贴花基线");
    let base_center = pixel(&base, W / 2, H / 2);
    assert!(
        base_center[0] < 200,
        "基线中心不该已经是红的：{base_center:?}"
    );

    renderer.draw_decal(DecalDesc::new(
        texture,
        vec3(0.0, 0.02, 0.0),
        vec3(6.0, 1.0, 6.0),
    ));
    let with_decal = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("贴花渲染");
    let center = pixel(&with_decal, W / 2, H / 2);
    assert!(
        center[0] > 180 && center[1] < 90 && center[2] < 90,
        "地面中心没有被红色贴花覆盖：{center:?}"
    );

    // 屏幕角落在地面/贴花盒之外，不能被染色。
    let corner = pixel(&with_decal, 2, 2);
    let base_corner = pixel(&base, 2, 2);
    assert!(
        corner == base_corner,
        "贴花盒之外必须逐位不变：角落 {corner:?} vs 基线 {base_corner:?}"
    );

    // 移除后逐位回到基线。
    renderer.clear_decals();
    let restored = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("移除贴花");
    let changed = restored
        .rgba
        .iter()
        .zip(&base.rgba)
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        changed, 0,
        "移除贴花后必须逐位回到基线（差异字节 {changed}）"
    );
}

/// MSAA > 1 时贴花必须读解析后的单采样深度，而不是多采样深度附件。
#[test]
fn projected_decal_reads_resolved_msaa_depth() {
    let Some((mut ctx, mut renderer)) = setup(4) else {
        return;
    };
    let texture = renderer.add_texture(
        &ctx,
        &ImageData::from_rgba8(4, 4, [255, 0, 0, 255].repeat(16)).expect("贴花纹理"),
        true,
    );
    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 8.0, 8.0);
    camera.look_at(Vec3::ZERO);
    let lighting = Lighting {
        exposure: 1.0,
        ..Default::default()
    };
    renderer.draw_decal(DecalDesc::new(
        texture,
        vec3(0.0, 0.02, 0.0),
        vec3(6.0, 1.0, 6.0),
    ));
    let image = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("MSAA 贴花渲染");
    let center = pixel(&image, W / 2, H / 2);
    assert!(
        center[0] > 180 && center[1] < 90 && center[2] < 90,
        "MSAA=4 时地面中心没有被红色贴花覆盖：{center:?}"
    );
}
