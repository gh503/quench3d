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
//! **显式 opt-in**：这个二进制要真的创建 wgpu 上下文，所以只有设了 `QUENCH3D_GPU_TESTS=1`
//! 才执行（见 `with_gpu_tests`），否则整条测试体不跑并打印跳过原因 —— 在没有 GPU 的
//! Windows runner 上建上下文是**原生崩溃**（`0xc0000005`），`Result::Err` 接不住。

use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;
use quench3d_render::{BloomOptions, DofOptions, FxaaOptions, RendererOptions};

const W: u32 = 64;
const H: u32 = 64;

/// 建一个无窗上下文；没显式打开开关、或本机没有 GPU 时返回 `None`（测试跳过）。
///
/// **创建上下文那一步在闭包里面**：没设 `QUENCH3D_GPU_TESTS=1` 时它一次都不会被调用。
/// `what` 是跳过原因：同一个文件里建上下文的地方有好几处，只说"跳过"分不清是哪一条。
fn context(msaa_samples: u32, what: &str) -> Option<GpuContext> {
    let ctx = with_gpu_tests(what, || {
        pollster::block_on(GpuContext::headless(
            W,
            H,
            GpuOptions {
                msaa_samples,
                ..Default::default()
            },
        ))
    })?;
    match ctx {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            None
        }
    }
}

fn setup(bloom: BloomOptions) -> Option<(GpuContext, Renderer)> {
    let ctx = context(1, "后处理链的像素级验证（真的创建 wgpu 上下文）")?;
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

/// 解析式环境高光必须真的改变金属球：同一盏太阳、同一台相机，只切
/// `RendererOptions.environment`，金属球上的反射/环境项必须不同。
///
/// 这不是"检查着色器里有没有那行代码"：像素差异证明它进了最终画面。
#[test]
fn analytic_environment_reflection_changes_metal() {
    let Some(mut ctx) = context(1, "环境高光的像素级验证（真的创建 wgpu 上下文）")
    else {
        return;
    };
    let image = |ctx: &mut GpuContext, environment: bool| {
        let mut renderer = Renderer::new(
            ctx,
            RendererOptions {
                grid: false,
                shadows: false,
                sky: false,
                environment,
                ..Default::default()
            },
        );
        let material = renderer.add_material(
            ctx,
            MaterialDesc {
                base_color: Vec4::new(0.15, 0.18, 0.22, 1.0),
                metallic: 1.0,
                roughness: 0.08,
                ..Default::default()
            },
        );
        let sphere = renderer.add_mesh(ctx, &MeshData::sphere(0.5, 24, 16));
        renderer.draw(
            sphere,
            material,
            InstanceDesc::from_position(vec3(0.0, 0.0, 0.0), 1.0),
        );
        let mut camera = Camera::perspective(W as f32 / H as f32);
        camera.position = vec3(0.0, 0.0, 2.0);
        camera.look_at(vec3(0.0, 0.0, 0.0));
        renderer
            .capture(
                ctx,
                W,
                H,
                &camera,
                &Lighting {
                    exposure: 1.0,
                    ..Default::default()
                },
            )
            .expect("离屏渲染")
    };
    let on = image(&mut ctx, true);
    let off = image(&mut ctx, false);
    let mut changed = 0usize;
    let mut max_diff = 0i32;
    for y in 0..H {
        for x in 0..W {
            let a = pixel(&on, x, y);
            let b = pixel(&off, x, y);
            let d = (0..3).map(|i| (a[i] - b[i]).abs()).max().unwrap_or(0);
            if d > 3 {
                changed += 1;
            }
            max_diff = max_diff.max(d);
        }
    }
    assert!(
        max_diff > 12 && changed > (W * H) as usize / 20,
        "环境高光没有改变金属球：changed={changed}, max_diff={max_diff}"
    );
}

/// 两个半透明面重叠时，必须**全局由远到近**画，而不是按材质 id/提交顺序。
///
/// 这条测试故意先提交近的红色面、再提交远的蓝色面：旧实现按批次的材质 id
/// 排序，会把红色先画、蓝色后画，中心像素偏蓝；正确结果是近处红色占 50%，
/// 叠加在远处蓝色之上。只改一个像素就能区分两种顺序。
#[test]
fn transparent_objects_are_drawn_back_to_front() {
    let Some((mut ctx, mut renderer)) = setup(BloomOptions::default()) else {
        return;
    };
    let bg = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.2, 0.2, 0.2, 1.0).unlit());
    let near_mat = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(1.0, 0.0, 0.0, 0.5)
            .unlit()
            .alpha_blend(),
    );
    let far_mat = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(0.0, 0.0, 1.0, 0.5)
            .unlit()
            .alpha_blend(),
    );
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    // 背景 z=-40（正面 z=0）；远处蓝面 z=-20（正面 z=20）；近处红面 z=0（正面 z=50）。
    renderer.draw(
        cube,
        bg,
        InstanceDesc::from_position(vec3(0.0, 0.0, -40.0), 80.0),
    );
    // 故意先提交近的：旧代码会先画它。
    renderer.draw(
        cube,
        near_mat,
        InstanceDesc::from_position(vec3(0.0, 0.0, 0.0), 100.0),
    );
    renderer.draw(
        cube,
        far_mat,
        InstanceDesc::from_position(vec3(0.0, 0.0, -20.0), 80.0),
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

    // 线性空间：bg → 50% 蓝 → 50% 红（从后往前）。
    let bg_lin = lin([0.2, 0.2, 0.2]);
    let blue_lin = lin([0.0, 0.0, 1.0]);
    let red_lin = lin([1.0, 0.0, 0.0]);
    let mut correct_lin = [0.0; 3];
    let mut wrong_lin = [0.0; 3];
    for i in 0..3 {
        let after_blue = 0.5 * blue_lin[i] + 0.5 * bg_lin[i];
        correct_lin[i] = 0.5 * red_lin[i] + 0.5 * after_blue;
        // 旧顺序：先红后蓝。
        let after_red = 0.5 * red_lin[i] + 0.5 * bg_lin[i];
        wrong_lin[i] = 0.5 * blue_lin[i] + 0.5 * after_red;
    }
    let correct = srgb8(aces(correct_lin));
    let wrong = srgb8(aces(wrong_lin));
    let got = pixel(&image, W / 2, H / 2);
    assert!(
        (correct[0] - wrong[0]).abs() > 20 || (correct[2] - wrong[2]).abs() > 20,
        "测试没有区分度：正确 {correct:?} 与错误顺序 {wrong:?} 太接近"
    );
    assert!(
        near(got, correct, 2),
        "透明物体没有由远到近画：期望 {correct:?}（先画近处会给 {wrong:?}），实际 {got:?}"
    );
}

/// 缺省（bloom 关闭）时，后处理链的输出与"只有 tonemap"完全一致——
/// 也就是说 bloom 的三趟 pass 一行都没执行。
#[test]
fn bloom_off_ignores_the_bloom_parameters() {
    let Some((mut ctx_a, mut a)) = setup(BloomOptions::default()) else {
        return;
    };
    let Some(ctx2) = context(1, "bloom 关闭时的后处理链对比（真的创建 wgpu 上下文）")
    else {
        return;
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
    let Some(ctx_on) = context(1, "bloom 亮区外溢的对比（真的创建 wgpu 上下文）")
    else {
        return;
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

/// 画一个转过的立方体并读回像素（`fxaa` 决定开不开后处理抗锯齿）。
///
/// 用**转过角度**的立方体：轮廓上有浅斜边，FXAA 的定向模糊才有东西可做。
/// 正对镜头的立方体是纯水平/垂直边——那是理想边缘，FXAA 原版也（正确地）不动它。
fn fxaa_image(fxaa: FxaaOptions) -> Option<ImageData> {
    let ctx = context(1, "FXAA 的像素级验证（真的创建 wgpu 上下文）")?;
    let mut ctx = ctx;
    let mut renderer = Renderer::new(
        &ctx,
        RendererOptions {
            grid: false,
            shadows: false,
            sky: false,
            fxaa,
            ..Default::default()
        },
    );
    // 无光照材质：物体内部必须是**同一个值**，这样"平坦区域没被动过"才可断言。
    let mat = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.9, 0.2, 0.15, 1.0).unlit());
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    // 半边长 20，相机 z=60（半屏世界高度 tan30°·60 ≈ 34.6）→ 屏上约占 58%，
    // 四周留出十几像素背景；容器角 30°/20° 让轮廓出现浅斜边。
    let rotation = Quat::from_rotation_y(0.5) * Quat::from_rotation_x(0.35);
    renderer.draw(
        cube,
        mat,
        InstanceDesc::new(Transform::from_trs(
            vec3(0.0, 0.0, 0.0),
            rotation,
            vec3(40.0, 40.0, 40.0),
        )),
    );

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 60.0);
    camera.look_at(vec3(0.0, 0.0, 0.0));

    Some(
        renderer
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
            .expect("离屏渲染"),
    )
}

/// 既不像 `a` 也不像 `b` 的像素（即"被抗锯齿混出来的"中间值）。
fn mixed_pixels(image: &ImageData, a: [i32; 3], b: [i32; 3]) -> usize {
    let mut n = 0;
    for y in 0..image.height {
        for x in 0..image.width {
            let p = pixel(image, x, y);
            if !near(p, a, 2) && !near(p, b, 2) {
                n += 1;
            }
        }
    }
    n
}

/// FXAA 的像素级验证：**硬边被平滑，平坦区域逐位不变**。
///
/// 后半句比前半句重要：抗锯齿最常见的失败是"整幅画糊了一层"。
/// `msaa_samples: 1` + 无光照材质让这个断言可以做得非常硬：
///   * 关闭时轮廓是硬的（中间值必须为 0）；
///   * 打开后同一处必须出现中间值（否则 FXAA 什么都没干）；
///   * 立方体内部与四角背景必须**逐位不变**（FXAA 的提前退出与 ±8 texel 步长上限）。
#[test]
fn fxaa_smooths_edges_and_leaves_flat_areas_alone() {
    let off_opts = FxaaOptions {
        enabled: false,
        ..Default::default()
    };
    let Some(i_off) = fxaa_image(off_opts) else {
        return;
    };
    let Some(i_on) = fxaa_image(FxaaOptions {
        enabled: true,
        ..Default::default()
    }) else {
        return;
    };

    let cube = pixel(&i_off, W / 2, H / 2);
    let background = pixel(&i_off, 1, 1);
    // 自检：两种颜色必须差得足够远，否则"混合像素"根本区分不出来
    assert!(
        !near(cube, background, 24),
        "测试自身失效：物体 {cube:?} 与背景 {background:?} 太接近"
    );
    // 物体颜色等于手算值（无光照 = 材质色过一遍 ACES）
    let expected = srgb8(aces(lin([0.9, 0.2, 0.15])));
    assert!(
        near(cube, expected, 1),
        "物体颜色与手算不符：{cube:?} vs {expected:?}"
    );

    let mixed_off = mixed_pixels(&i_off, cube, background);
    let mixed_on = mixed_pixels(&i_on, cube, background);
    assert_eq!(
        mixed_off, 0,
        "关掉 FXAA 时不该有中间值（msaa=1、无光照），实际 {mixed_off} 个"
    );
    assert!(
        mixed_on >= 8,
        "开着 FXAA 却几乎没有平滑（中间值 {mixed_on} 个）：轮廓 {cube:?} / 背景 {background:?}"
    );

    // 平坦区域：物体内部 ±6 像素必须逐位不变（离轮廓至少 12 像素，超出 ±8 texel 步长）
    for y in (H / 2 - 6)..=(H / 2 + 6) {
        for x in (W / 2 - 6)..=(W / 2 + 6) {
            let p = pixel(&i_on, x, y);
            assert!(
                near(p, expected, 1),
                "物体内部被改了：({x},{y}) = {p:?}，手算 {expected:?}"
            );
        }
    }
    // 四角背景同理（立方体只占中间约 58%）
    for (x, y) in [(1, 1), (W - 2, 1), (1, H - 2), (W - 2, H - 2), (2, H / 2)] {
        let p = pixel(&i_on, x, y);
        assert!(
            near(p, background, 2),
            "背景被改了：({x},{y}) = {p:?}，关闭时 {background:?}"
        );
    }
}

/// FXAA 的阈值必须是**真的在用**：把相对阈值调到不可能触发（比任何对比度都大），
/// 画面就该退化成关闭时的样子。这条防的是"阈值读了但没进判断"这类接线错误。
#[test]
fn fxaa_threshold_actually_gates_the_filter() {
    let Some(i_off) = fxaa_image(FxaaOptions {
        enabled: false,
        ..Default::default()
    }) else {
        return;
    };
    let Some(i_huge) = fxaa_image(FxaaOptions {
        enabled: true,
        edge_threshold: 100.0,
        edge_threshold_min: 100.0,
    }) else {
        return;
    };
    let cube = pixel(&i_off, W / 2, H / 2);
    let background = pixel(&i_off, 1, 1);
    assert_eq!(
        mixed_pixels(&i_huge, cube, background),
        0,
        "阈值大到不该触发时仍然平滑了边缘：说明阈值没有参与判断"
    );
    for y in 0..H {
        for x in 0..W {
            let (a, b) = (pixel(&i_off, x, y), pixel(&i_huge, x, y));
            assert!(
                near(a, b, 1),
                "({x},{y}) 与关闭 FXAA 不一致：{b:?} vs {a:?}（阈值应当让它原样返回）"
            );
        }
    }
}

/// 画"地面 + 坐在上面的方块"，返回 `(开 SSAO, 关 SSAO)` 两张图 + 相机。
///
/// 用 **msaa = 4**：SSAO 要单采样深度，而 msaa > 1 时深度附件是多采样的——
/// 这条路径（`depth_resolve.wgsl` 那一趟）只有这里才会被真正执行。
/// 同一个 renderer 先关后开，顺带验证"运行时改 `options.ssao.enabled`"能生效
/// （编辑器里勾选框走的就是这条路：`ensure_targets` 必须察觉开关变了）。
fn ssao_images() -> Option<(ImageData, ImageData, Camera)> {
    let ctx = context(4, "SSAO 的像素级验证（真的创建 wgpu 上下文）")?;
    let mut ctx = ctx;
    let mut renderer = Renderer::new(
        &ctx,
        RendererOptions {
            grid: false,
            shadows: false,
            sky: false,
            ..Default::default()
        },
    );
    // GL/WebGL2 上读不了深度纹理（naga GLSL 限制）→ SSAO 整体关掉，
    // 见 `Renderer::supports_ssao`。这不是"失败被跳过"：这条断言在该后端上**没有意义**，
    // AO 根本不参与着色（`WGPU_BACKEND=gl` 跑这个测试会走到这里，并打印原因）。
    if !renderer.supports_ssao() {
        eprintln!("跳过：当前后端读不了深度纹理，SSAO 已按能力关闭（GL/WebGL2）");
        return None;
    }
    // 无光照中灰：AO 是乘上去的，平坦面上"暗了多少"可以逐像素直接读
    let mat = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.6, 0.6, 0.6, 1.0).unlit());
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    // 地面：12×0.4×12，顶面正好在 y = 0
    renderer.draw(
        cube,
        mat,
        InstanceDesc::from_position(vec3(0.0, -0.2, 0.0), 12.0),
    );
    let ground = InstanceDesc::new(Transform::from_trs(
        vec3(0.0, -0.2, 0.0),
        Quat::IDENTITY,
        vec3(12.0, 0.4, 12.0),
    ));
    renderer.draw(cube, mat, ground);
    // 方块：1.2 边长，底面正好在 y = 0（与地面接触 → 接缝处应当有明显 AO）
    let box_desc = InstanceDesc::new(Transform::from_trs(
        vec3(0.0, 0.6, 0.0),
        Quat::IDENTITY,
        vec3(1.2, 1.2, 1.2),
    ));
    renderer.draw(cube, mat, box_desc);
    // 外圈地面上的另一个方块，用来确认 AO 不是"整幅画都压暗"
    let far_box = InstanceDesc::new(Transform::from_trs(
        vec3(-4.0, 0.6, -4.0),
        Quat::IDENTITY,
        vec3(1.2, 1.2, 1.2),
    ));
    renderer.draw(cube, mat, far_box);

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(3.2, 2.2, 4.2);
    camera.look_at(vec3(0.0, 0.5, 0.0));

    let lighting = Lighting {
        exposure: 1.0,
        ..Default::default()
    };
    let off = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染（关 SSAO）");
    // 运行时打开：`ensure_targets` 必须重建（多采样深度要解析成单采样）
    renderer.options.ssao.enabled = true;
    let on = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染（开 SSAO）");
    Some((on, off, camera))
}

/// 一个屏幕窗口内的平均亮度（0~255）。
fn window_mean(image: &ImageData, center: (f32, f32), half: i32) -> f64 {
    let (cx, cy) = (center.0.round() as i32, center.1.round() as i32);
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in (cy - half)..=(cy + half) {
        for x in (cx - half)..=(cx + half) {
            if x < 0 || y < 0 || x >= image.width as i32 || y >= image.height as i32 {
                continue;
            }
            let p = pixel(image, x as u32, y as u32);
            sum += (p[0] + p[1] + p[2]) as f64 / 3.0;
            n += 1.0;
        }
    }
    if n == 0.0 {
        f64::NAN
    } else {
        sum / n
    }
}

/// SSAO 的像素级验证：**接触处变暗、远处与天空不动**，且运行时开关立刻生效。
#[test]
fn ssao_darkens_contacts_and_leaves_the_far_plane_alone() {
    let Some((on, off, camera)) = ssao_images() else {
        return;
    };
    let viewport = vec2(W as f32, H as f32);

    // ① 方块与地面的接缝：AO 应当把它压暗
    let contact = camera
        .world_to_screen(vec3(0.62, 0.02, 0.62), viewport)
        .expect("接缝在屏幕内");
    let contact_off = window_mean(&off, (contact.x, contact.y), 5);
    let contact_on = window_mean(&on, (contact.x, contact.y), 5);
    let contact_drop = contact_off - contact_on;
    eprintln!("接缝：关 {contact_off:.2} → 开 {contact_on:.2}（暗了 {contact_drop:.2}）");
    // 峰值：接缝上最暗的那一个像素暗了多少（均值会被窗口里没接触的像素摊薄）
    let (mut peak, mut peak_at) = (0.0f64, (0, 0));
    for y in 0..H {
        for x in 0..W {
            let a = pixel(&off, x, y);
            let b = pixel(&on, x, y);
            let drop = ((a[0] + a[1] + a[2]) - (b[0] + b[1] + b[2])) as f64 / 3.0;
            if drop > peak {
                peak = drop;
                peak_at = (x, y);
            }
        }
    }
    let peak_dist = (((peak_at.0 as f32 - contact.x).powi(2)
        + (peak_at.1 as f32 - contact.y).powi(2))
    .sqrt()) as f64;
    eprintln!("最暗像素在 {peak_at:?}，暗了 {peak:.2}，离接缝 {peak_dist:.1} 像素");
    assert!(
        peak >= 20.0,
        "整幅图没有任何一个像素被明显压暗（最暗只有 {peak:.2}），不像在做 AO"
    );
    assert!(
        peak_dist <= 12.0,
        "最暗的地方离接缝 {peak_dist:.1} 像素，AO 没有集中在接触处"
    );
    assert!(
        contact_drop >= 3.0,
        "接缝没有被环境光遮蔽压暗：{contact_off:.2} → {contact_on:.2}"
    );
    assert!(
        contact_drop <= 120.0,
        "接缝被压得太狠（{contact_drop:.1}），不像 AO 像黑影"
    );

    // ② 远处的地面（离最近的方块 3 米以上，远超 0.75 米半径）：必须几乎不动
    let far = camera
        .world_to_screen(vec3(-2.0, 0.0, 2.0), viewport)
        .expect("远地面在屏幕内");
    let far_off = window_mean(&off, (far.x, far.y), 5);
    let far_on = window_mean(&on, (far.x, far.y), 5);
    eprintln!(
        "远处地面：关 {far_off:.2} → 开 {far_on:.2}（差 {:.2}）",
        far_on - far_off
    );
    assert!(
        (far_on - far_off).abs() <= 2.0,
        "远离遮挡物的地面不该变暗：{far_off:.2} → {far_on:.2}"
    );

    // ③ 天空（没有几何，深度 = 1）：**逐位不变**——远平面提前退出
    for (x, y) in [(0, 0), (W - 1, 0), (W / 2, 0)] {
        let (a, b) = (pixel(&off, x, y), pixel(&on, x, y));
        assert_eq!(a, b, "({x},{y}) 是天空，SSAO 不该碰它");
    }

    // ④ 整幅图确实变了（否则上面那条"接缝变暗"可能只是巧合）
    let mut changed = 0;
    for y in 0..H {
        for x in 0..W {
            if pixel(&off, x, y) != pixel(&on, x, y) {
                changed += 1;
            }
        }
    }
    eprintln!("改动像素 {changed} / {}", W * H);
    assert!(changed > 100, "开 SSAO 后画面几乎没变：{changed} 个像素");
}

// ---------------------------------------------------------------------------
// 「裸奔守卫」：不需要显卡，每条 CI 腿都会跑
//
// 为什么需要它：第 58 轮只给两个测试二进制（`backend_fallback` / `model_instances`）
// 加了 `with_gpu_tests`，其余 8 个仍会**直接建 wgpu 上下文** —— 在没有 GPU 的 Windows
// runner 上那是**原生崩溃**（`0xc0000005`），`Result::Err` 接不住，红得无法诊断。
// 只把那 8 个补上只能解决"这一次"；这条守卫解决"下一次有人新写一个"。
// ---------------------------------------------------------------------------

/// 会**真的建 wgpu 上下文 / 实例**的调用（按子串匹配）。
///
/// * `GpuContext::from_candidates` 同时覆盖 `from_candidates_with`；
/// * `wgpu::Instance::new` 同时覆盖 `new_without_display_handle`。
///
/// 只看这几个**入口**：适配器请求、设备请求都在它们内部（自己拆开写的话，
/// 入口那一步已经被这里拦住了）。API 改名时这条守卫会以"一个调用点都没扫到"报红，
/// 提醒你更新这份清单 —— 而不是静默空转。
const GPU_CREATION_CALLS: &[&str] = &[
    "GpuContext::headless",
    "GpuContext::from_candidates",
    "GpuContext::create_instance_with",
    "wgpu::Instance::new",
];

fn blank(out: &mut [u8], i: usize) {
    if out[i] != b'\n' && out[i] != b'\r' {
        out[i] = b' ';
    }
}

/// 把注释与字符串字面量换成空格（保留换行），只留下**代码**。
///
/// 为什么必须先做这一步：`crates/quench3d-editor/tests/joint_frames.rs` 的**文档注释**里
/// 就写着「编辑器现有的往返测试都要 `GpuContext::headless`」—— 纯 `grep` 会把那句散文
/// 当成一处裸奔的建上下文。会乱叫的守卫最后一定会被人加白名单，那等于没有守卫。
///
/// 已知边界：不特殊处理字符字面量（`'"'` 会看岔，仓库里没有）；`r#标识符` 会按普通代码走。
fn code_only(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        // 行注释
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                blank(&mut out, i);
                i += 1;
            }
            continue;
        }
        // 块注释（Rust 允许嵌套）
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            let mut depth = 0usize;
            while i < b.len() {
                if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                    depth += 1;
                    blank(&mut out, i);
                    blank(&mut out, i + 1);
                    i += 2;
                } else if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                    blank(&mut out, i);
                    blank(&mut out, i + 1);
                    i += 2;
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                } else {
                    blank(&mut out, i);
                    i += 1;
                }
            }
            continue;
        }
        // 原始字符串：r"..." / r#"..."# / r##"..."##（先数 # 再找配对的收尾）
        if b[i] == b'r' {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < b.len() && b[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < b.len() && b[j] == b'"' {
                let mut k = j + 1;
                loop {
                    if k >= b.len() {
                        break;
                    }
                    if b[k] == b'"' {
                        let mut h = 0usize;
                        while k + 1 + h < b.len() && b[k + 1 + h] == b'#' {
                            h += 1;
                        }
                        if h >= hashes {
                            k += 1 + hashes;
                            break;
                        }
                    }
                    k += 1;
                }
                for x in i..k.min(b.len()) {
                    blank(&mut out, x);
                }
                i = k;
                continue;
            }
        }
        // 普通字符串（含 b"..."，转义用 \ ）
        if b[i] == b'"' {
            blank(&mut out, i);
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    blank(&mut out, i);
                    i += 1;
                    if i < b.len() {
                        blank(&mut out, i);
                        i += 1;
                    }
                    continue;
                }
                let done = b[i] == b'"';
                blank(&mut out, i);
                i += 1;
                if done {
                    break;
                }
            }
            continue;
        }
        i += 1;
    }
    String::from_utf8(out).expect("逐字节替换成空格不会破坏 UTF-8")
}

/// `with_gpu_tests(...)` 覆盖的字节区间（括号配对；注释与字符串已剥掉，可以直接数括号）。
fn gated_spans(code: &str) -> Vec<(usize, usize)> {
    let b = code.as_bytes();
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(off) = code[from..].find("with_gpu_tests") {
        let at = from + off;
        from = at + "with_gpu_tests".len();
        let mut i = from;
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b'(' {
            continue;
        }
        let mut depth = 0i32;
        let mut j = i;
        while j < b.len() {
            match b[j] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        j += 1;
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        spans.push((at, j));
    }
    spans
}

/// 仓库根：`crates/quench3d-render` 往上两级。
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("仓库根")
        .to_path_buf()
}

/// 所有 `crates/*/tests/*.rs`（仓库根相对路径 + 内容），按路径排序。
fn test_sources() -> Vec<(String, String)> {
    let root = repo_root();
    let mut crates: Vec<_> = std::fs::read_dir(root.join("crates"))
        .expect("读 crates/")
        .map(|e| e.expect("crates/ 目录项").path())
        .collect();
    crates.sort();
    let mut out = Vec::new();
    for krate in crates {
        let tests = krate.join("tests");
        if !tests.is_dir() {
            continue;
        }
        let mut files: Vec<_> = std::fs::read_dir(&tests)
            .expect("读 tests/")
            .map(|e| e.expect("tests/ 目录项").path())
            .collect();
        files.sort();
        for f in files {
            if f.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = f
                .strip_prefix(&root)
                .expect("在仓库根下")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((name, std::fs::read_to_string(&f).expect("读测试文件")));
        }
    }
    out
}

fn line_of(code: &str, at: usize) -> usize {
    1 + code[..at].matches('\n').count()
}

/// 扫描 `crates/*/tests/*.rs`：**凡是真的建 wgpu 上下文的地方，都必须经过 `with_gpu_tests`**。
///
/// 这条测试不需要显卡（只读文件），所以它在**每条** CI 腿上都跑 —— 包括没有 GPU 的
/// Windows 腿。它自动覆盖**将来新增**的测试文件：新写一个裸建上下文的测试，
/// 这里立刻红，不用等人想起"哦还有一条腿会崩"。
#[test]
fn every_real_gpu_context_creation_in_tests_goes_through_the_gate() {
    let sources = test_sources();
    assert!(
        sources.len() >= 5,
        "只扫到 {} 个测试文件，守卫退化了（crates/*/tests/*.rs 的布局变了？）",
        sources.len()
    );

    let mut gated = 0usize;
    let mut bare: Vec<String> = Vec::new();
    for (name, src) in &sources {
        let code = code_only(src);
        let spans = gated_spans(&code);
        for needle in GPU_CREATION_CALLS {
            let mut from = 0;
            while let Some(off) = code[from..].find(needle) {
                let at = from + off;
                from = at + needle.len();
                if spans.iter().any(|(s, e)| at >= *s && at < *e) {
                    gated += 1;
                } else {
                    bare.push(format!("{name}:{}  {needle}", line_of(&code, at)));
                }
            }
        }
    }

    assert!(
        gated > 0,
        "一个「建上下文且已过 with_gpu_tests」的调用点都没扫到：要么 wgpu 入口改名了\
         （更新 GPU_CREATION_CALLS），要么测试被搬走了 —— 守卫不能是空转的"
    );
    assert!(
        bare.is_empty(),
        "这些地方真的建了 wgpu 上下文却没走 `with_gpu_tests`（在没有 GPU 的 Windows runner 上\
         会**原生崩溃** `0xc0000005`，`Result::Err` 接不住）：\n  {}\n\
         修法：把 `GpuContext::headless(...)` 那一句放进 `with_gpu_tests(\"原因（真的创建 wgpu 上下文）\", || ...)` \
         的闭包里；断言一条都不用改。",
        bare.join("\n  ")
    );
    eprintln!(
        "裸奔守卫：扫了 {} 个测试文件，{} 处建上下文全部经过 with_gpu_tests",
        sources.len(),
        gated
    );
}

/// 屏幕空间水：水面材质只写 mask（颜色 alpha = 0），后处理必须自己把水画出来。
///
/// 这是"mask 附件 + 第二颜色目标 + 后处理读取"三件事的端到端守卫：
/// 任何一环接错，`on` 与 `off` 的中心像素都不会有差别。
#[test]
fn water_post_uses_the_mask_and_changes_pixels() {
    let Some(mut ctx) = context(1, "屏幕空间水的 mask/后处理（真的创建 wgpu 上下文）")
    else {
        return;
    };

    let image = |ctx: &mut GpuContext, water: WaterOptions, level: f32| {
        let mut renderer = Renderer::new(
            ctx,
            RendererOptions {
                grid: false,
                shadows: false,
                sky: false,
                water,
                ..Default::default()
            },
        );
        let bg = renderer.add_material(ctx, MaterialDesc::from_srgb(0.2, 0.2, 0.2, 1.0).unlit());
        let mut water_desc = MaterialDesc::from_srgb(1.0, 0.0, 0.0, 0.0).unlit();
        water_desc.alpha_blend = true;
        water_desc.water_mask = true;
        let water = renderer.add_material(ctx, water_desc);
        let cube = renderer.add_mesh(ctx, &MeshData::cube(1.0));
        renderer.draw(
            cube,
            bg,
            InstanceDesc::from_position(vec3(0.0, 0.0, -20.0), 80.0),
        );
        renderer.draw(
            cube,
            water,
            InstanceDesc::from_position(vec3(0.0, level, 0.0), 100.0),
        );
        let mut camera = Camera::perspective(W as f32 / H as f32);
        camera.position = vec3(0.0, 0.0, 60.0);
        camera.look_at(vec3(0.0, 0.0, 0.0));
        renderer
            .capture(
                ctx,
                W,
                H,
                &camera,
                &Lighting {
                    exposure: 1.0,
                    ..Default::default()
                },
            )
            .expect("离屏渲染")
    };

    let flat = WaterOptions {
        enabled: true,
        color: [1.0, 0.0, 0.0],
        strength: 0.0,
        clarity: 1.0,
        foam: 0.0,
        refraction: 0.0,
    };
    let off = image(
        &mut ctx,
        WaterOptions {
            enabled: false,
            ..flat
        },
        0.0,
    );
    let on = image(&mut ctx, flat, 0.0);
    let wavy = image(
        &mut ctx,
        WaterOptions {
            strength: 3.0,
            foam: 1.0,
            refraction: 1.0,
            ..flat
        },
        0.0,
    );
    let a = pixel(&off, W / 2, H / 2);
    let b = pixel(&on, W / 2, H / 2);
    assert!(
        b[0] > a[0] + 10,
        "水面后处理没有生效：on={b:?} off={a:?}（红通道应当明显升高）"
    );
    let corner_off = pixel(&off, 4, 4);
    let corner_on = pixel(&on, 4, 4);
    assert!(
        corner_on[0] > corner_off[0] + 5,
        "角落也没有水面效果：on={corner_on:?} off={corner_off:?}"
    );
    // 波面法线/折射/高光必须真的起作用：同一水面只改风格参数，像素应当不同。
    let flat_center = pixel(&on, W / 2, H / 2);
    let wavy_center = pixel(&wavy, W / 2, H / 2);
    assert_ne!(
        flat_center, wavy_center,
        "波浪/折射参数没有改变画面：flat={flat_center:?} wavy={wavy_center:?}"
    );
}

/// mask.g 存的是水面世界高度：把水面抬高后，屏幕射线与水面交点变化，
/// 波面法线/折射也必须变化。这个测试用**水平面**（真实水面几何），
/// 避免立方体竖直面把 `level` 变成逐像素变化的值。
#[test]
fn water_mask_height_reconstructs_the_screen_ray() {
    let Some(mut ctx) = context(1, "水面高度重建（真的创建 wgpu 上下文）") else {
        return;
    };
    let image = |ctx: &mut GpuContext, level: f32| {
        let mut renderer = Renderer::new(
            ctx,
            RendererOptions {
                grid: false,
                shadows: false,
                sky: false,
                water: WaterOptions {
                    enabled: true,
                    color: [0.1, 0.45, 0.7],
                    strength: 3.0,
                    clarity: 0.8,
                    foam: 1.0,
                    refraction: 1.0,
                },
                ..Default::default()
            },
        );
        let bg = renderer.add_material(ctx, MaterialDesc::from_srgb(0.2, 0.2, 0.2, 1.0).unlit());
        let mut water_desc = MaterialDesc::from_srgb(0.1, 0.4, 0.7, 0.0).unlit();
        water_desc.alpha_blend = true;
        water_desc.water_mask = true;
        let water = renderer.add_material(ctx, water_desc);
        let cube = renderer.add_mesh(ctx, &MeshData::cube(1.0));
        let plane = renderer.add_mesh(ctx, &MeshData::plane(1.0, 1));
        renderer.draw(
            cube,
            bg,
            InstanceDesc::from_position(vec3(0.0, 0.0, -20.0), 80.0),
        );
        renderer.draw(
            plane,
            water,
            InstanceDesc::from_position(vec3(0.0, level, 0.0), 100.0),
        );
        let mut camera = Camera::perspective(W as f32 / H as f32);
        camera.position = vec3(0.0, 30.0, 40.0);
        camera.look_at(vec3(0.0, 0.0, 0.0));
        renderer
            .capture(
                ctx,
                W,
                H,
                &camera,
                &Lighting {
                    exposure: 1.0,
                    ..Default::default()
                },
            )
            .expect("离屏渲染")
    };
    let low = image(&mut ctx, 0.0);
    let high = image(&mut ctx, 5.0);
    let low_px = pixel(&low, W / 2, H / 2 + 20);
    let high_px = pixel(&high, W / 2, H / 2 + 20);
    assert_ne!(
        low_px, high_px,
        "水面抬高后像素不变，mask.g 没参与射线-平面重建：low={low_px:?} high={high_px:?}"
    );
}

/// 网格写了 `frag_depth`，管线也必须真的允许写深度。
///
/// 失败表现：地面下的调试线段穿过地面画出来（网格看起来"不挡东西"）。
/// 这里用两条只有 y 不同的线段做对照：地面**下方**的必须被遮住，
/// 地面上方的必须可见。修之前下方那条会露出来。
#[test]
fn grid_writes_depth_and_hides_lines_below_the_floor() {
    let Some((mut ctx, _)) = setup(BloomOptions::default()) else {
        return;
    };
    // 重新建一个开网格的渲染器：`setup` 关掉了网格。
    let mut renderer = Renderer::new(
        &ctx,
        RendererOptions {
            grid: true,
            shadows: false,
            sky: false,
            ..Default::default()
        },
    );
    renderer.lines.enabled = true;

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 4.0, 8.0);
    camera.look_at(vec3(0.0, 0.0, 0.0));

    let lighting = Lighting {
        exposure: 1.0,
        ..Default::default()
    };
    let blank = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("空白地面");
    renderer.lines.line(
        vec3(-3.0, -0.6, 0.0),
        vec3(3.0, -0.6, 0.0),
        Vec4::new(0.0, 0.0, 1.0, 1.0),
    );
    let hidden = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("地面下方线段");
    renderer.lines.clear();
    renderer.lines.line(
        vec3(-1.0, 0.8, 0.0),
        vec3(1.0, 0.8, 0.0),
        Vec4::new(0.0, 0.0, 1.0, 1.0),
    );
    let visible = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("地面上方线段");

    assert_eq!(
        hidden.rgba, blank.rgba,
        "地面下方的线段被画出来了：网格深度没有写入"
    );
    assert_ne!(
        visible.rgba, blank.rgba,
        "地面上方的线段也没画出来：线段渲染本身坏了"
    );
}

/// 景深：同一对焦外的高对比边缘，打开 DoF 后必须真的糊开；把对焦拉回物体
/// 所在深度则应当与关闭时几乎一致（证明不是"只要是 DoF 就整体变糊"）。
#[test]
fn depth_of_field_blurs_out_of_focus_edges() {
    let Some((mut ctx, mut renderer)) = setup(BloomOptions::default()) else {
        return;
    };
    if matches!(ctx.adapter_info.backend, wgpu::Backend::Gl) {
        eprintln!("跳过：GL 后端没有深度纹理，DoF 着色器会整段剥离");
        return;
    }
    let red = renderer.add_material(&ctx, MaterialDesc::from_srgb(1.0, 0.0, 0.0, 1.0).unlit());
    let green = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.0, 1.0, 0.0, 1.0).unlit());
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    // 两个立方体在画面中心形成一条红/绿竖直边，深度都在 z=0（相机在 z=10）。
    renderer.draw(
        cube,
        red,
        InstanceDesc::from_position(vec3(-0.75, 0.0, 0.0), 1.5),
    );
    renderer.draw(
        cube,
        green,
        InstanceDesc::from_position(vec3(0.75, 0.0, 0.0), 1.5),
    );
    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = vec3(0.0, 0.0, 10.0);
    camera.look_at(vec3(0.0, 0.0, 0.0));
    let lighting = Lighting {
        exposure: 1.0,
        ..Default::default()
    };
    let sharp = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染（DoF 关闭）");
    renderer.options.dof = DofOptions {
        enabled: true,
        focus: 1.0,
        aperture: 1.0,
        max_blur: 12.0,
    };
    let blurred = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染（DoF 开启、焦点在 1m）");
    let changed = (0..W * H)
        .filter(|i| {
            let i = (*i * 4) as usize;
            (0..3).any(|c| (sharp.rgba[i + c] as i32 - blurred.rgba[i + c] as i32).abs() > 4)
        })
        .count();
    assert!(
        changed > 100,
        "焦外边缘应当被明显模糊，实际只有 {changed} 个像素变化"
    );

    // 光圈归零：同一套 DoF 参数、同一个深度图，画面必须回到与关闭时逐像素一致，
    // 证明上面的差异确实来自 DoF 的模糊半径，而不是别的随机/缓存差异。
    renderer.options.dof.aperture = 0.0;
    let focused = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染（DoF 开启、光圈 0）");
    let still_changed = (0..W * H)
        .filter(|i| {
            let i = (*i * 4) as usize;
            (0..3).any(|c| (sharp.rgba[i + c] as i32 - focused.rgba[i + c] as i32).abs() > 4)
        })
        .count();
    assert_eq!(
        still_changed, 0,
        "光圈为 0 时 DoF 不该改变任何像素，实际有 {still_changed} 个"
    );
}
