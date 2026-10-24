//! 材质开关的**像素级**验证：alpha 混合与双面渲染必须真的改变画面。
//!
//! 这两个开关此前是死的：
//! - `needs_alpha_blend()` 只看 `base_color.w`，而 glTF 的 `alphaMode: BLEND` 材质
//!   颜色因子通常是 1.0（alpha 在贴图里）→ 半透明贴图被当成不透明；
//! - `double_sided` 被存下来但渲染器从不消费，不透明一律 `cull_mode: Back`
//!   → 平面的背面永远看不见（着色器其实已经按 `front_facing` 翻转了法线）。
//!
//! 所以这里不看代码路径，而是**对比同一张图里左右两半的像素**：
//! 左边开开关、右边不开，同一相机、同一光照、同一帧，差异只可能来自开关。
//! **显式 opt-in**：这个二进制要真的创建 wgpu 上下文，所以只有设了 `QUENCH3D_GPU_TESTS=1`
//! 才执行（见 `with_gpu_tests`），否则整条测试体不跑并打印跳过原因 —— 在没有 GPU 的
//! Windows runner 上建上下文是**原生崩溃**（`0xc0000005`），`Result::Err` 接不住。

use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;
use quench3d_render::RendererOptions;

const W: u32 = 96;
const H: u32 = 64;

/// 建一个无窗上下文；没显式打开开关、或本机没有 GPU 时返回 `None`（测试跳过）。
///
/// **创建上下文那一步在闭包里面**：没设 `QUENCH3D_GPU_TESTS=1` 时它一次都不会被调用。
fn setup() -> Option<(GpuContext, Renderer)> {
    let ctx = with_gpu_tests(
        "材质开关的像素级验证（真的创建 wgpu 上下文）",
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
            ..Default::default()
        },
    );
    Some((ctx, renderer))
}

/// 两半分别取平均颜色（0~1）。
fn halves_mean(image: &ImageData) -> ([f32; 3], [f32; 3]) {
    let mut left = [0.0f64; 3];
    let mut right = [0.0f64; 3];
    let mut ln = 0.0f64;
    let mut rn = 0.0f64;
    for y in 0..image.height {
        for x in 0..image.width {
            let i = ((y * image.width + x) * 4) as usize;
            let px = [
                image.rgba[i] as f64 / 255.0,
                image.rgba[i + 1] as f64 / 255.0,
                image.rgba[i + 2] as f64 / 255.0,
            ];
            if x < image.width / 2 {
                for c in 0..3 {
                    left[c] += px[c];
                }
                ln += 1.0;
            } else {
                for c in 0..3 {
                    right[c] += px[c];
                }
                rn += 1.0;
            }
        }
    }
    let norm = |v: [f64; 3], n: f64| [v[0] / n, v[1] / n, v[2] / n];
    (
        norm(left, ln.max(1.0)).map(|v| v as f32),
        norm(right, rn.max(1.0)).map(|v| v as f32),
    )
}

#[test]
fn alpha_blend_flag_makes_texture_alpha_show_through() {
    let Some((mut ctx, mut renderer)) = setup() else {
        return;
    };

    // 贴图 alpha ≈ 0.35 的红色。两边的贴图与颜色完全相同，只有 alpha_blend 开关不同。
    let rgba: Vec<u8> = [255u8, 0, 0, 90].repeat(4);
    let tex = renderer.add_texture(
        &ctx,
        &ImageData {
            width: 2,
            height: 2,
            rgba,
        },
        true,
    );
    let back = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.10, 0.90, 0.20, 1.0).unlit());
    let front_blend = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(1.0, 0.0, 0.0, 1.0)
            .with_base_color_texture(tex)
            .alpha_blend()
            .unlit(),
    );
    let front_opaque = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(1.0, 0.0, 0.0, 1.0)
            .with_base_color_texture(tex)
            .unlit(),
    );

    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    // 左：绿底板 + 半透明红块（走混合）；右：绿底板 + 同一个红块（不走混合）
    for (x, front) in [(-1.5f32, front_blend), (1.5, front_opaque)] {
        renderer.draw(
            cube,
            back,
            InstanceDesc::from_position(vec3(x, 0.0, -1.0), 4.0),
        );
        renderer.draw(
            cube,
            front,
            InstanceDesc::from_position(vec3(x, 0.0, 1.0), 2.4),
        );
    }

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 4.5);
    camera.look_at(vec3(0.0, 0.0, 0.0));

    let image = renderer
        .capture(&mut ctx, W, H, &camera, &Lighting::studio())
        .expect("离屏渲染");
    let (left, right) = halves_mean(&image);

    // 混合那一半能看见后面的绿色，不混合的那一半是纯红
    assert!(
        left[1] > right[1] + 0.10,
        "alpha 混合没生效：左（混合）G={:.3} 应当明显高于右（不混合）G={:.3}",
        left[1],
        right[1]
    );
    // 反过来看红色：不混合的一侧被纯红盖住，红通道应当明显更高
    assert!(
        right[0] > left[0] * 1.15,
        "不混合的一侧应当更红：左 R={:.3}，右 R={:.3}",
        left[0],
        right[0]
    );
}

#[test]
fn ao_texture_darkens_only_the_ambient_term() {
    let Some((mut ctx, mut renderer)) = setup() else {
        return;
    };

    // 黑 AO 贴图：只压环境光；太阳关掉，避免直接光盖住差异。
    let black: Vec<u8> = [0u8, 0, 0, 255].repeat(4);
    let ao = renderer.add_texture(
        &ctx,
        &ImageData {
            width: 2,
            height: 2,
            rgba: black,
        },
        false,
    );
    let plain = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(0.5, 0.5, 0.5, 1.0).pbr(0.0, 0.9),
    );
    let occluded = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(0.5, 0.5, 0.5, 1.0)
            .pbr(0.0, 0.9)
            .with_ao_texture(ao),
    );
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    for (x, material) in [(-1.2f32, occluded), (1.2, plain)] {
        renderer.draw(
            cube,
            material,
            InstanceDesc::from_position(vec3(x, 0.0, 0.0), 1.6),
        );
    }

    let mut lighting = Lighting::studio();
    lighting.sun = None;
    lighting.ambient_intensity = 1.0;
    lighting.sky.enabled = false;
    lighting.exposure = 1.0;

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 5.0);
    camera.look_at(vec3(0.0, 0.0, 0.0));
    let image = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染");
    let (left, right) = halves_mean(&image);
    assert!(
        right[0] > left[0] * 1.25,
        "AO 贴图没有压暗环境光：左（有 AO）R={:.3} 应明显低于右（无 AO）R={:.3}",
        left[0],
        right[0]
    );
}

#[test]
fn double_sided_flag_renders_back_faces() {
    let Some((mut ctx, mut renderer)) = setup() else {
        return;
    };

    // 平面网格的法线朝 +Y；相机在下方往上看 → 看到的是背面。
    // 用洋红（纯色 unlit）方便统计"被画出来的像素"。
    let single = renderer.add_material(&ctx, MaterialDesc::from_srgb(1.0, 0.0, 1.0, 1.0).unlit());
    let double = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(1.0, 0.0, 1.0, 1.0)
            .unlit()
            .double_sided(),
    );
    let plane = renderer.add_mesh(&ctx, &MeshData::plane(1.0, 1));
    // 左：单面（应当被背面剔除）；右：双面（应当看得见）
    renderer.draw(
        plane,
        single,
        InstanceDesc::from_position(vec3(-1.5, 0.0, 0.0), 2.6),
    );
    renderer.draw(
        plane,
        double,
        InstanceDesc::from_position(vec3(1.5, 0.0, 0.0), 2.6),
    );

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, -5.0, 2.0);
    camera.look_at(vec3(0.0, 0.0, 0.0));

    let image = renderer
        .capture(&mut ctx, W, H, &camera, &Lighting::studio())
        .expect("离屏渲染");

    let mut left_pixels = 0u32;
    let mut right_pixels = 0u32;
    for y in 0..image.height {
        for x in 0..image.width {
            let i = ((y * image.width + x) * 4) as usize;
            let (r, g, b) = (
                image.rgba[i] as f32 / 255.0,
                image.rgba[i + 1] as f32 / 255.0,
                image.rgba[i + 2] as f32 / 255.0,
            );
            // 洋红：红蓝都高、绿很低
            if r > 0.35 && b > 0.35 && g < 0.35 {
                if x < image.width / 2 {
                    left_pixels += 1;
                } else {
                    right_pixels += 1;
                }
            }
        }
    }

    assert!(
        right_pixels > 200,
        "双面材质没有画出背面（只数到 {right_pixels} 个像素）"
    );
    assert!(
        left_pixels < right_pixels / 10,
        "单面材质不该画出背面：左 {left_pixels} vs 右 {right_pixels}"
    );
}

/// UV 偏移/旋转必须真的改变采样结果，而不是只写进 uniform。
#[test]
fn uv_offset_and_rotation_change_pixels() {
    let Some((mut ctx, mut renderer)) = setup() else {
        return;
    };
    // 2x2 高对比棋盘：偏移半个贴图后，采样到的颜色必然换一批。
    let rgba = vec![
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
    ];
    let tex = renderer.add_texture(
        &ctx,
        &ImageData {
            width: 2,
            height: 2,
            rgba,
        },
        true,
    );
    let base = MaterialDesc::default().with_base_color_texture(tex).unlit();
    let material = renderer.add_material(&ctx, base.clone());
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    renderer.draw(
        cube,
        material,
        InstanceDesc::from_position(vec3(0.0, 0.0, 0.0), 3.0),
    );
    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 5.0);
    camera.look_at(Vec3::ZERO);

    let before = renderer
        .capture(&mut ctx, W, H, &camera, &Lighting::studio())
        .expect("离屏渲染");

    let shifted = MaterialDesc {
        uv_offset: [0.5, 0.0],
        ..base.clone()
    };
    renderer.update_material(&ctx, material, shifted);
    let after_offset = renderer
        .capture(&mut ctx, W, H, &camera, &Lighting::studio())
        .expect("离屏渲染");

    let mut offset_diff = 0u32;
    for (a, b) in before.rgba.chunks(4).zip(after_offset.rgba.chunks(4)) {
        let d: u32 =
            a[0].abs_diff(b[0]) as u32 + a[1].abs_diff(b[1]) as u32 + a[2].abs_diff(b[2]) as u32;
        if d > 24 {
            offset_diff += 1;
        }
    }
    assert!(
        offset_diff > 100,
        "UV 偏移没有改变采样：只有 {offset_diff} 个像素不同"
    );

    let rotated = MaterialDesc {
        uv_rotation: std::f32::consts::FRAC_PI_4,
        ..base.clone()
    };
    renderer.update_material(&ctx, material, rotated);
    let after_rotation = renderer
        .capture(&mut ctx, W, H, &camera, &Lighting::studio())
        .expect("离屏渲染");
    let mut rotation_diff = 0u32;
    for (a, b) in after_offset
        .rgba
        .chunks(4)
        .zip(after_rotation.rgba.chunks(4))
    {
        let d: u32 =
            a[0].abs_diff(b[0]) as u32 + a[1].abs_diff(b[1]) as u32 + a[2].abs_diff(b[2]) as u32;
        if d > 24 {
            rotation_diff += 1;
        }
    }
    assert!(
        rotation_diff > 100,
        "UV 旋转没有改变采样：只有 {rotation_diff} 个像素不同"
    );
}
