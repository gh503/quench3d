//! 自动 LOD 的 GPU 验证：同一网格、同一材质，相机拉远后实际绘制三角形数下降，
//! 但画面仍有内容。
//!
//! **显式 opt-in**（与 `post_pipeline.rs` 相同）：只有 `QUENCH3D_GPU_TESTS=1`
//! 才创建 wgpu 上下文；没有 GPU 的 CI 上整条测试跳过。

use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;

const W: u32 = 64;
const H: u32 = 64;

fn context() -> Option<GpuContext> {
    let ctx = with_gpu_tests(
        "自动 LOD 的 GPU 验证（真的创建 wgpu 上下文）",
        || {
            pollster::block_on(GpuContext::headless(
                W,
                H,
                GpuOptions {
                    msaa_samples: 1,
                    ..Default::default()
                },
            ))
        },
    )?;
    match ctx {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            None
        }
    }
}

#[test]
fn auto_lod_draws_fewer_triangles_when_far_away() {
    let Some(mut ctx) = context() else {
        return;
    };
    let mut renderer = Renderer::new(
        &ctx,
        RendererOptions {
            grid: false,
            shadows: false,
            sky: false,
            auto_lod: true,
            ..Default::default()
        },
    );
    let mesh = renderer.add_mesh(&ctx, &MeshData::sphere(1.0, 64, 32));
    let mat = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.6, 0.7, 0.8, 1.0).unlit());
    renderer.draw(
        mesh,
        mat,
        InstanceDesc::from_position(vec3(0.0, 0.0, 0.0), 1.0),
    );

    let lighting = Lighting::default();
    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 3.0);
    camera.look_at(Vec3::ZERO);
    let near = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("近景渲染");
    let near_tris = renderer.stats().triangles;

    camera.position = vec3(0.0, 0.0, 40.0);
    camera.look_at(Vec3::ZERO);
    let far = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("远景渲染");
    let far_tris = renderer.stats().triangles;

    assert!(near_tris > 3_000, "近景应当画完整球：{near_tris} 个三角形");
    assert!(
        far_tris * 2 < near_tris,
        "远景应当切到粗 LOD：近 {near_tris}、远 {far_tris}"
    );
    // 关掉开关必须回到全量网格：同一个相机、同一个批次，三角形数恢复。
    renderer.options.auto_lod = false;
    let off = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("关闭 LOD 渲染");
    assert_eq!(
        renderer.stats().triangles,
        MeshData::sphere(1.0, 64, 32).triangle_count() as u64,
        "auto_lod=false 时必须画原始网格"
    );
    assert_eq!(off.width, W);

    let center = ((H / 2 * W + W / 2) * 4) as usize;
    assert!(
        far.rgba[center] > 0 || far.rgba[center + 1] > 0 || far.rgba[center + 2] > 0,
        "远景切 LOD 后不能把物体画丢"
    );
    assert_eq!(near.width, W);
    assert_eq!(far.height, H);
}
