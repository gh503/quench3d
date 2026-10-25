//! 多盏投影聚光灯的行为验证：每盏灯一层阴影图，shader 按层采样。
//!
//! 关键判据：两盏灯都勾投影时，右侧那盏的阴影必须真的出现；只勾第一盏时右侧
//! 变亮。如果 shader 只看 layer 0（或把第二盏的 packed index 写错），两幅图在
//! 右半边会**逐位相同**，测试会红。

use quench3d_math::{vec3, Vec3};
use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;
use quench3d_render::{GpuOptions, RendererOptions};

const W: u32 = 128;
const H: u32 = 96;

fn setup() -> Option<(GpuContext, Renderer)> {
    let ctx = with_gpu_tests("多盏投影聚光灯", || {
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
            shadows: true,
            sky: false,
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
fn two_spot_lights_use_independent_shadow_layers() {
    let Some((mut ctx, mut renderer)) = setup() else {
        return;
    };
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let white = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.8, 0.8, 0.8, 1.0));
    // 地面：顶面 y = 0。
    renderer.draw(
        cube,
        white,
        InstanceDesc::new(Transform {
            position: vec3(0.0, -0.5, 0.0),
            scale: vec3(40.0, 1.0, 40.0),
            ..Transform::IDENTITY
        }),
    );
    // 两盏灯各自下方一个遮挡块。
    for x in [-3.0f32, 3.0] {
        renderer.draw(
            cube,
            white,
            InstanceDesc::new(Transform {
                position: vec3(x, 2.0, 0.0),
                scale: vec3(2.0, 2.0, 2.0),
                ..Transform::IDENTITY
            }),
        );
    }
    let spot = |x: f32, cast: bool| {
        SpotLight::new(
            vec3(x, 7.0, 0.0),
            vec3(0.0, -1.0, 0.0),
            Vec3::ONE,
            25.0,
            20.0,
            8.0,
            20.0,
        )
        .with_cast_shadows(cast)
    };
    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 9.0, 12.0);
    camera.look_at(Vec3::ZERO);
    let lighting = |left: bool, right: bool| {
        let mut l = Lighting::none();
        l.sky.zenith = Vec3::ZERO;
        l.sky.horizon = Vec3::ZERO;
        l.sky.ground = Vec3::ZERO;
        l.spot_lights = vec![spot(-3.0, left), spot(3.0, right)];
        l
    };

    let both = renderer
        .capture(&mut ctx, W, H, &camera, &lighting(true, true))
        .expect("两盏都投影");
    let left_only = renderer
        .capture(&mut ctx, W, H, &camera, &lighting(true, false))
        .expect("只有左灯投影");

    // 右半边应有第二盏灯的阴影被去掉后的亮度差。
    let mut changed = 0;
    for y in 0..H {
        for x in (W / 2)..W {
            let a = pixel(&both, x, y);
            let b = pixel(&left_only, x, y);
            if a != b {
                changed += 1;
            }
        }
    }
    assert!(
        changed > 40,
        "第二盏灯的投影没有产生任何右半边像素变化（{changed} 个）——shader 可能只在读 layer 0"
    );

    // 两层的深度图都必须真的有遮挡物（不是全 1 的空图）。
    let (_, _, layer0) = renderer
        .read_spot_shadow_map_layer(&ctx, 0)
        .expect("读 layer 0");
    let (_, _, layer1) = renderer
        .read_spot_shadow_map_layer(&ctx, 1)
        .expect("读 layer 1");
    let occluded0 = layer0.iter().filter(|d| **d < 0.999).count();
    let occluded1 = layer1.iter().filter(|d| **d < 0.999).count();
    assert!(occluded0 > 100, "layer 0 没有遮挡物：{occluded0}");
    assert!(occluded1 > 100, "layer 1 没有遮挡物：{occluded1}");
    assert!(
        renderer.read_spot_shadow_map_layer(&ctx, 4).is_err(),
        "越界层必须报错"
    );
}
