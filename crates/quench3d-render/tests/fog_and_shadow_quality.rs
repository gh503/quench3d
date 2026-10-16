//! 雾与阴影强度的**像素级**验证（场景格式 `lighting.fog` / `lighting.shadow_quality`）。
//!
//! 这两项以前在引擎里有、场景文件里配不到（格式只暴露了 `sky: bool` 和 `shadows: bool`）。
//! 把它们接到格式上之后，必须证明"接上了真的会改变画面"，否则又是一条**静默失效**：
//! 字段能写、校验能过、像素一点没变。
//!
//! 判据都用"两帧相减"，不写死具体像素值：
//! * 雾：把远处物体的像素颜色往雾色上拉（两帧差异显著，且方向是"更红"）；
//! * 阴影强度：只影响被遮挡的那块地（两帧差异显著，且方向是"更暗"）。
//!
//! **显式 opt-in**：这个二进制要真的创建 wgpu 上下文，所以只有设了 `QUENCH3D_GPU_TESTS=1`
//! 才执行（见 `with_gpu_tests`），否则整条测试体不跑并打印跳过原因 —— 在没有 GPU 的
//! Windows runner 上建上下文是**原生崩溃**（`0xc0000005`），`Result::Err` 接不住。

use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;
use quench3d_render::RendererOptions;

const W: u32 = 128;
const H: u32 = 96;

/// 建一个无窗上下文；没显式打开开关、或本机没有 GPU 时返回 `None`（测试跳过）。
///
/// **创建上下文那一步在闭包里面**：没设 `QUENCH3D_GPU_TESTS=1` 时它一次都不会被调用。
fn setup(shadows: bool) -> Option<(GpuContext, Renderer)> {
    let ctx = with_gpu_tests(
        "雾与阴影强度的像素级验证（真的创建 wgpu 上下文）",
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
            sky: false,
            shadows,
            ..Default::default()
        },
    );
    Some((ctx, renderer))
}

/// 没有太阳、没有环境光、天空也是黑的：只剩我们要测的那一项在起作用。
fn dark_lighting() -> Lighting {
    let mut lighting = Lighting::none();
    lighting.sky.enabled = false;
    lighting.sky.zenith = Vec3::ZERO;
    lighting.sky.horizon = Vec3::ZERO;
    lighting.sky.ground = Vec3::ZERO;
    lighting
}

/// 两帧之间"明显不一样"的像素数，以及整幅图的亮度变化（第二帧 - 第一帧）。
fn diff(a: &ImageData, b: &ImageData) -> (usize, i64) {
    let mut changed = 0usize;
    let mut delta = 0i64;
    for (pa, pb) in a
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.rgba.as_chunks::<4>().0)
    {
        let d = (0..3)
            .map(|c| (pa[c] as i32 - pb[c] as i32).abs())
            .max()
            .unwrap_or(0);
        if d > 8 {
            changed += 1;
        }
        let lum_a: i64 = (0..3).map(|c| pa[c] as i64).sum();
        let lum_b: i64 = (0..3).map(|c| pb[c] as i64).sum();
        delta += lum_b - lum_a;
    }
    (changed, delta)
}

/// 红主导的像素数：雾色是纯红时，被雾拉过去的像素会变成红色主导。
fn red_dominant(image: &ImageData) -> usize {
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| {
            px[0] > 60 && px[0] > px[1].saturating_add(25) && px[0] > px[2].saturating_add(25)
        })
        .count()
}

#[test]
fn fog_pulls_distant_pixels_toward_the_fog_color() {
    let Some((mut ctx, mut renderer)) = setup(false) else {
        return;
    };

    // 一个离相机 6 米左右的箱子：雾是"指数距离雾"，距离越远越浓。
    let mesh = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let material = renderer.add_material(&ctx, MaterialDesc::from_srgb(1.0, 1.0, 1.0, 1.0));
    renderer.draw(mesh, material, InstanceDesc::from_position(Vec3::ZERO, 1.0));

    // 大太阳从相机侧后方照，箱子是亮的（不然雾没有东西可以拉）
    let mut base = dark_lighting();
    base.sun = Some(DirectionalLight {
        direction: Vec3::new(0.2, -0.3, -1.0).normalize_or_zero(),
        color: Vec3::ONE,
        intensity: 3.0,
        cast_shadows: false,
        ..DirectionalLight::default()
    });

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 0.0, 6.0);
    camera.look_at(Vec3::ZERO);

    let clear = renderer
        .capture(&mut ctx, W, H, &camera, &base)
        .expect("离屏渲染");
    assert_eq!(red_dominant(&clear), 0, "没有雾时不该有偏红像素");

    // 纯红浓雾：1 - exp(-0.6 * 6) ≈ 0.97，几乎把箱子整个染红
    let mut foggy = base.clone();
    foggy.fog = FogParams {
        color: Vec3::new(1.0, 0.0, 0.0),
        density: 0.6,
    };
    let fogged = renderer
        .capture(&mut ctx, W, H, &camera, &foggy)
        .expect("离屏渲染");

    let (changed, _) = diff(&clear, &fogged);
    assert!(
        changed > 200,
        "开了雾但画面几乎没变（{changed} 个像素变化）——`lighting.fog` 接上了吗？"
    );
    assert!(
        red_dominant(&fogged) > 200,
        "浓红雾没有把箱子染红：红主导像素 {}",
        red_dominant(&fogged)
    );

    // 密度 0 = 关闭：必须与不写 fog 那一节**逐位相同**（"不写 = 关闭"的承诺）。
    let mut off = base.clone();
    off.fog = FogParams {
        color: Vec3::new(1.0, 0.0, 0.0),
        density: 0.0,
    };
    let off = renderer
        .capture(&mut ctx, W, H, &camera, &off)
        .expect("离屏渲染");
    assert_eq!(
        off.rgba, clear.rgba,
        "雾密度 0 必须与不写雾逐位相同（否则「不写 = 关闭」是假的）"
    );
}

#[test]
fn shadow_strength_only_darkens_the_shadowed_pixels() {
    let Some((mut ctx, mut renderer)) = setup(true) else {
        return;
    };

    // 用一块大而薄的"地板"接阴影：把单位立方体在 xy 上压扁、放大。
    let mesh = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let ground = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.85, 0.85, 0.85, 1.0));
    renderer.draw(
        mesh,
        ground,
        InstanceDesc::new(Transform {
            position: Vec3::new(0.0, -0.5, 0.0),
            scale: Vec3::new(12.0, 0.5, 12.0),
            ..Transform::IDENTITY
        }),
    );
    // 悬浮的遮挡物：影子落在地板上，并且**偏出去一块**（太阳斜射），
    // 不能被它自己挡住 —— 否则"阴影强度"测不到东西。
    let box_mat = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.9, 0.6, 0.3, 1.0));
    renderer.draw(
        mesh,
        box_mat,
        InstanceDesc::new(Transform {
            position: Vec3::new(0.0, 0.8, 0.0),
            scale: Vec3::new(2.0, 1.6, 2.0),
            ..Transform::IDENTITY
        }),
    );

    // 斜射的太阳：影子往 +x/+z 偏 ≈ 1.0 米，露在遮挡物旁边
    let mut lighting = dark_lighting();
    lighting.sun = Some(DirectionalLight {
        direction: Vec3::new(0.6, -1.0, 0.35).normalize_or_zero(),
        color: Vec3::ONE,
        intensity: 3.0,
        cast_shadows: true,
        shadow_radius: 16.0,
        shadow_map_size: 1024,
    });
    lighting.shadow_strength = 0.0;

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 5.0, 7.0);
    camera.look_at(Vec3::new(0.0, 0.0, 0.0));

    let no_shadow = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染");

    lighting.shadow_strength = 1.0;
    let full_shadow = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染");

    let (changed, delta) = diff(&no_shadow, &full_shadow);
    assert!(
        changed > 150,
        "阴影强度 0 → 1 只有 {changed} 个像素变了：影子没画上，或者 shadow_strength 没生效"
    );
    assert!(
        delta < 0,
        "阴影强度拉满之后整幅图应当变暗，实测亮度变化 {delta}"
    );

    // 强度是**单调**的：0.5 的暗化程度必须落在 0 与 1 之间（不是"要么全无要么全黑"）。
    lighting.shadow_strength = 0.5;
    let half_shadow = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染");
    let (_, delta_half) = diff(&no_shadow, &half_shadow);
    assert!(
        delta_half < 0 && delta_half > delta,
        "强度 0.5 的暗化（{delta_half}）应当落在 0（0）与 {delta}（1.0）之间"
    );
}
