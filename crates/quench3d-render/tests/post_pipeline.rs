//! 后处理链的**像素级**验证：HDR 中间目标 → tonemap（+ bloom）。
//!
//! 为什么不是"看代码路径"：这条路改的是**数值发生在哪个空间**，
//! 只有把像素与手算的闭式解对上，才能证明"tonemap 恰好做了一次"。
//!
//! 关键背景（改造前是错的）：
//!   场景原先直接往 sRGB 目标写 `aces(线性值)`，于是半透明/网格线的混合发生在
//!   tonemap **之后**——硬件把 tonemapped 值当线性值去混合，再编码一次。
//!   现在是：物体写线性 HDR → `rgba16float` 中间目标（在这里混合）→ 统一 tonemap。
//!
//! 没有可用 GPU 时直接跳过（CI 机器通常没有软件光栅驱动）。

use quench3d_render::prelude::*;
use quench3d_render::{BloomOptions, RendererOptions};

const W: u32 = 64;
const H: u32 = 64;

fn setup(bloom: BloomOptions) -> Option<(GpuContext, Renderer)> {
    let ctx = match pollster::block_on(GpuContext::headless(
        W,
        H,
        GpuOptions {
            msaa_samples: 1,
            ..Default::default()
        },
    )) {
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
            bloom,
            ..Default::default()
        },
    );
    Some((ctx, renderer))
}

/// 手算 ACES（与 `post.wgsl` 完全同式，用 f64 避免测试自己的舍入误差）。
fn aces(x: [f64; 3]) -> [f64; 3] {
    let (a, b, c, d, e) = (2.51, 0.03, 2.43, 0.59, 0.14);
    let mut out = [0.0; 3];
    for i in 0..3 {
        let v = x[i];
        out[i] = ((v * (a * v + b)) / (v * (c * v + d) + e)).clamp(0.0, 1.0);
    }
    out
}

/// 线性 → sRGB 编码（与硬件写 sRGB 目标做的事一致）。
fn encode(x: [f64; 3]) -> [f64; 3] {
    x.map(|v| quench3d_render::color::linear_to_srgb(v.clamp(0.0, 1.0) as f32) as f64)
}

fn srgb8(x: [f64; 3]) -> [i32; 3] {
    encode(x).map(|v| (v * 255.0).round() as i32)
}

fn pixel(image: &ImageData, x: u32, y: u32) -> [i32; 3] {
    let i = ((y * image.width + x) * 4) as usize;
    [
        image.rgba[i] as i32,
        image.rgba[i + 1] as i32,
        image.rgba[i + 2] as i32,
    ]
}

/// 线性颜色（与 `MaterialDesc::from_srgb` 同一条转换）。
fn lin(rgb: [f32; 3]) -> [f64; 3] {
    rgb.map(|v| quench3d_render::color::srgb_to_linear(v) as f64)
}

fn near(a: [i32; 3], b: [i32; 3], tol: i32) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= tol)
}

/// 一块铺满视野的无光照色块：像素必须等于 `encode(aces(线性色))`，
/// 也就是"tonemap 恰好一次"。多了或少了都会偏出几十个色阶。
#[test]
fn hdr_post_pass_tonemaps_exactly_once() {
    let Some((mut ctx, mut renderer)) = setup(BloomOptions::default()) else {
        return;
    };
    let mat = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.5, 0.6, 0.7, 1.0).unlit());
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    // 大立方体铺满整个视野（含四角）：相机 z=60、半边长 40 → 正面在 z=40，
    // 距离 20 处连对角半高只要 20·tan30°·√2 ≈ 16.3 < 40，四角也被盖住。
    renderer.draw(
        cube,
        mat,
        InstanceDesc::from_position(vec3(0.0, 0.0, 0.0), 80.0),
    );

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 60.0);
    camera.look_at(vec3(0.0, 0.0, 0.0));

    let image = renderer
        .capture(
            &mut ctx,
            W,
            H,
            &camera,
            &Lighting {
                exposure: 1.0,
                ..Default::default()
            },
        )
        .expect("离屏渲染");

    let expected = srgb8(aces(lin([0.5, 0.6, 0.7])));
    let got = pixel(&image, W / 2, H / 2);
    assert!(
        near(got, expected, 1),
        "tonemap 不是恰好一次：手算 {expected:?}，实际 {got:?}（f16 中间目标最多差 1）"
    );
    // 整幅图应当都是同一个值（无光照、无天空、无网格、msaa=1）
    for y in 0..H {
        for x in 0..W {
            let p = pixel(&image, x, y);
            assert!(
                near(p, expected, 1),
                "({x},{y}) = {p:?} 与手算 {expected:?} 不符"
            );
        }
    }
}

/// **本轮的核心理由**：混合必须发生在 tonemap 之前。
///
/// 50% 的亮色半透明块盖在暗色底板上：
///   正确 = encode(aces(α·亮 + (1-α)·暗))
///   改造前 = encode(α·aces(亮) + (1-α)·aces(暗))   ← 差 20+ 个色阶
/// 这个断言在旧路径上必然失败，所以它证明的是"修好了"，而不是"改动了"。
#[test]
fn alpha_blending_happens_before_tonemap() {
    let Some((mut ctx, mut renderer)) = setup(BloomOptions::default()) else {
        return;
    };
    let back = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.2, 0.2, 0.2, 1.0).unlit());
    let front = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(0.9, 0.9, 0.9, 0.5)
            .unlit()
            .alpha_blend(),
    );
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    // 底板 cube80 放在 z=-20（正面 z=20 → 距相机 40，铺满视野）；
    // 半透明层 cube100（正面 z=50 → 距相机 10）在它**前面**，且大到盖住整幅图。
    // 注意不能把两层都放在原点：小的那个会整个躲在大的里面，看不见。
    renderer.draw(
        cube,
        back,
        InstanceDesc::from_position(vec3(0.0, 0.0, -20.0), 80.0),
    );
    renderer.draw(
        cube,
        front,
        InstanceDesc::from_position(vec3(0.0, 0.0, 0.0), 100.0),
    );

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 60.0);
    camera.look_at(vec3(0.0, 0.0, 0.0));

    let image = renderer
        .capture(
            &mut ctx,
            W,
            H,
            &camera,
            &Lighting {
                exposure: 1.0,
                ..Default::default()
            },
        )
        .expect("离屏渲染");

    let bg = lin([0.2, 0.2, 0.2]);
    let fg = lin([0.9, 0.9, 0.9]);
    let alpha = 0.5;
    let correct = srgb8(aces([
        alpha * fg[0] + (1.0 - alpha) * bg[0],
        alpha * fg[1] + (1.0 - alpha) * bg[1],
        alpha * fg[2] + (1.0 - alpha) * bg[2],
    ]));
    let before_fix = srgb8([
        alpha * aces(fg)[0] + (1.0 - alpha) * aces(bg)[0],
        alpha * aces(fg)[1] + (1.0 - alpha) * aces(bg)[1],
        alpha * aces(fg)[2] + (1.0 - alpha) * aces(bg)[2],
    ]);
    let got = pixel(&image, W / 2, H / 2);

    assert!(
        (correct[0] - before_fix[0]).abs() > 20,
        "测试没有区分度：正确值 {correct:?} 与旧值 {before_fix:?} 太接近"
    );
    assert!(
        near(got, correct, 1),
        "混合没有发生在 tonemap 之前：期望 {correct:?}（旧路径会给 {before_fix:?}），实际 {got:?}"
    );
}

/// 缺省（bloom 关闭）时，后处理链的输出与"只有 tonemap"完全一致——
/// 也就是说 bloom 的三趟 pass 一行都没执行。
#[test]
fn bloom_off_ignores_the_bloom_parameters() {
    let Some((mut ctx_a, mut a)) = setup(BloomOptions::default()) else {
        return;
    };
    let ctx2 = match pollster::block_on(GpuContext::headless(
        W,
        H,
        GpuOptions {
            msaa_samples: 1,
            ..Default::default()
        },
    )) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            return;
        }
    };
    let mut ctx_b = ctx2;
    // 故意给一组很激进的参数，但开关是关的：结果必须与缺省逐像素一致
    let mut b = Renderer::new(
        &ctx_b,
        RendererOptions {
            grid: false,
            shadows: false,
            sky: false,
            bloom: BloomOptions {
                enabled: false,
                intensity: 5.0,
                threshold: 0.05,
            },
            ..Default::default()
        },
    );

    let build = |renderer: &mut Renderer, ctx: &mut GpuContext| {
        let mat = renderer.add_material(ctx, MaterialDesc::from_srgb(0.3, 0.8, 0.4, 1.0).unlit());
        let cube = renderer.add_mesh(ctx, &MeshData::cube(1.0));
        renderer.draw(
            cube,
            mat,
            InstanceDesc::from_position(vec3(0.0, 0.0, 0.0), 40.0),
        );
    };
    build(&mut a, &mut ctx_a);
    build(&mut b, &mut ctx_b);

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 60.0);
    camera.look_at(vec3(0.0, 0.0, 0.0));
    let lighting = Lighting {
        exposure: 1.0,
        ..Default::default()
    };

    let ia = a.capture(&mut ctx_a, W, H, &camera, &lighting).expect("A");
    let ib = b.capture(&mut ctx_b, W, H, &camera, &lighting).expect("B");
    assert_eq!(ia.rgba, ib.rgba, "bloom 关闭时参数不该影响任何像素");
    assert_eq!(a.stats().post_passes, 1, "关闭 bloom 时只该有 1 趟后处理");
}

/// bloom 真的让亮区**外溢**：亮块外围变亮，远处基本不变。
#[test]
fn bloom_makes_bright_areas_bleed() {
    let Some((mut ctx_off, mut off)) = setup(BloomOptions::default()) else {
        return;
    };
    let ctx_on = match pollster::block_on(GpuContext::headless(
        W,
        H,
        GpuOptions {
            msaa_samples: 1,
            ..Default::default()
        },
    )) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            return;
        }
    };
    let mut ctx_on = ctx_on;
    let mut on = Renderer::new(
        &ctx_on,
        RendererOptions {
            grid: false,
            shadows: false,
            sky: false,
            bloom: BloomOptions {
                enabled: true,
                intensity: 2.0,
                threshold: 0.8,
            },
            ..Default::default()
        },
    );

    // 中间一块很强的自发光（线性 8.0，远高于阈值 0.8），背景是清屏色（很暗）
    let bright = |renderer: &mut Renderer, ctx: &mut GpuContext| {
        let mat = renderer.add_material(
            ctx,
            MaterialDesc::from_srgb(0.0, 0.0, 0.0, 1.0)
                .unlit()
                .emissive(vec3(1.0, 1.0, 1.0), 8.0),
        );
        let cube = renderer.add_mesh(ctx, &MeshData::cube(1.0));
        // 半边长 10，相机 z=60（半屏世界高度 tan30°·60 ≈ 34.6）→ 屏上约占 29%，
        // 两侧各留出十几像素的暗背景，用来观察晕圈。
        renderer.draw(
            cube,
            mat,
            InstanceDesc::from_position(vec3(0.0, 0.0, 0.0), 20.0),
        );
    };
    bright(&mut off, &mut ctx_off);
    bright(&mut on, &mut ctx_on);

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 60.0);
    camera.look_at(vec3(0.0, 0.0, 0.0));
    let lighting = Lighting {
        exposure: 1.0,
        ..Default::default()
    };

    let i_off = off
        .capture(&mut ctx_off, W, H, &camera, &lighting)
        .expect("off");
    let i_on = on
        .capture(&mut ctx_on, W, H, &camera, &lighting)
        .expect("on");

    // 亮块正中心：都已经被 tonemap 压到接近 1，差别可能很小
    let center_off = pixel(&i_off, W / 2, H / 2);
    let center_on = pixel(&i_on, W / 2, H / 2);
    assert!(center_on[0] >= center_off[0], "中心不该变暗");

    // 亮块边缘在 x≈21 与 x≈42（实测），往外应当是一条渐弱的晕圈
    let ring_y = H / 2;
    let ring_off = pixel(&i_off, W / 2 + 20, ring_y);
    let ring_on = pixel(&i_on, W / 2 + 20, ring_y);
    assert!(
        ring_on[0] > ring_off[0] + 20,
        "亮区没有外溢：紧贴边缘处 旧={ring_off:?} 新={ring_on:?}"
    );

    // 晕圈必须是**单调衰减**的，而不是一块硬边
    let mut last = ring_on[0];
    for step in [24u32, 28, 31] {
        let v = pixel(&i_on, W / 2 + step, ring_y)[0];
        assert!(
            v <= last,
            "晕圈不是单调衰减：x={} 处 {v} > 上一次 {last}",
            W / 2 + step
        );
        last = v;
    }

    // 远角（离亮块 30+ 像素，半分辨率 9 抽头高斯够不到）：必须与关闭时一致
    let corner_off = pixel(&i_off, 1, 1);
    let corner_on = pixel(&i_on, 1, 1);
    assert!(
        near(corner_on, corner_off, 1),
        "远处不该被 bloom 影响：角落 旧={corner_off:?} 新={corner_on:?}"
    );

    // 背景本身没被整体抬亮（bloom 只加在亮区周围）
    let bg_off = pixel(&i_off, 2, H / 2);
    let bg_on = pixel(&i_on, 2, H / 2);
    assert!(
        near(bg_on, bg_off, 3),
        "背景被整体抬亮了：x=2 处 旧={bg_off:?} 新={bg_on:?}"
    );
}
