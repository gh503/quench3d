//! GPU 视锥剔除 + 间接绘制的行为验证。
//!
//! 关键点：GPU 拿的是**完整**实例列表（包括相机背后的实例），CPU 的 `visible`
//! 只服务阴影/透明。所以间接参数里的 `instance_count` 必须小于输入实例数；
//! 如果 compute 没跑或把整批原样输出，这个测试会红。

use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;
use quench3d_render::{GpuOptions, RendererOptions};

const W: u32 = 96;
const H: u32 = 96;

fn setup() -> Option<(GpuContext, Renderer)> {
    let ctx = with_gpu_tests("GPU 视锥剔除 + 间接绘制", || {
        pollster::block_on(GpuContext::headless(
            W,
            H,
            GpuOptions {
                msaa_samples: 1,
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
            gpu_culling: true,
            ..Default::default()
        },
    );
    Some((ctx, renderer))
}

#[test]
fn gpu_culling_compacts_instances_and_keeps_the_image() {
    let Some((mut ctx, mut renderer)) = setup() else {
        return;
    };
    if !renderer.gpu_culling_supported() {
        eprintln!("跳过：后端不支持 compute shader");
        return;
    }
    let mat = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.8, 0.5, 0.2, 1.0).unlit());
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    // 同一批 4 个实例：2 个在相机前方、2 个在相机背后。
    for x in [-10.0f32, 10.0] {
        renderer.draw(
            cube,
            mat,
            InstanceDesc::from_position(vec3(x, 0.0, 0.0), 8.0),
        );
    }
    for x in [-10.0f32, 10.0] {
        renderer.draw(
            cube,
            mat,
            InstanceDesc::from_position(vec3(x, 0.0, 100.0), 8.0),
        );
    }
    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 60.0);
    camera.look_at(Vec3::ZERO);
    let lighting = Lighting {
        exposure: 1.0,
        ..Default::default()
    };

    let gpu_image = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("GPU 剔除渲染");
    let args = renderer.read_gpu_cull_args(&ctx, 1).expect("回读间接参数");
    eprintln!("args = {args:?}");
    assert_eq!(args.len(), 5, "应当有 1 组 DrawIndexedIndirectArgs");
    assert_eq!(args[0] as usize, 36, "立方体 12 个三角形");
    assert_eq!(args[1], 2, "4 个实例里只有 2 个在视锥内");
    assert_eq!(args[4], 0, "批次从第 0 个实例开始");

    renderer.options.gpu_culling = false;
    let cpu_image = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("CPU 剔除基线");
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            assert_eq!(
                gpu_image.rgba[i..i + 3],
                cpu_image.rgba[i..i + 3],
                "GPU 剔除后的画面必须与 CPU 路径一致（({x},{y})）"
            );
        }
    }
}
