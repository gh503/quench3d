//! 点光源的**像素级**验证。
//!
//! 这里盯的是一个真实存在过的 bug：`FrameUniforms` 曾经把点光源写成两个并列数组
//! （8 个位置一段 + 8 个颜色一段，SoA），而 `shaders/common.wgsl` 里的
//! `array<PointLight, 8>` 是**交错的结构体数组**（AoS：pos0, color0, pos1, …）。
//! 布局错位之后，着色器把"下一盏灯的位置"当成"这一盏灯的颜色和强度"在读：
//! 预设的两盏灯看着有光（颜色是错的），而场景里只给一盏灯时**完全不亮**。
//!
//! 所以这个测试刻意**只放一盏红灯**——它同时验证两件事：
//! 1. 灯真的亮（旧布局下单灯亮度为零，测试会失败）；
//! 2. 颜色取自 `PointLight::color` 而不是别处的字节（旧布局下会取到位置）。
//!
//! 没有可用 GPU 时直接跳过（CI 机器通常没有软件光栅驱动）。

use quench3d_render::prelude::*;
use quench3d_render::{RendererOptions, MAX_POINT_LIGHTS};

const W: u32 = 96;
const H: u32 = 64;

/// 建一个无窗上下文；没有 GPU 时返回 `None`（测试跳过）。
fn setup() -> Option<(GpuContext, Renderer)> {
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
            ..Default::default()
        },
    );
    Some((ctx, renderer))
}

/// 一盏灯都不开时的「黑」到底有多黑。
///
/// `Lighting::none()` 只是没有太阳、环境光强度为 0；着色器里的环境项还会混入天空色
/// （`mix(ambient, sky, 0.65)`），所以必须把天空色也清零，否则箱子会被"夜空"照亮成
/// 深蓝，测不出点光源的真实贡献。
fn no_ambient() -> Lighting {
    let mut lighting = Lighting::none();
    lighting.sky.enabled = false;
    lighting.sky.zenith = Vec3::ZERO;
    lighting.sky.horizon = Vec3::ZERO;
    lighting.sky.ground = Vec3::ZERO;
    lighting
}

/// 与取景无关的统计量：整幅图里「红主导」的像素数、最亮像素的红通道、整体最大亮度。
///
/// 用整幅图而不是中心小窗，是因为箱子的屏幕尺寸随画布与 FOV 变化，写死窗口很脆。
fn red_stats(image: &ImageData) -> (usize, u8, u8) {
    let mut red_dominant = 0;
    let mut max_red = 0u8;
    let mut max_lum = 0u8;
    for px in image.rgba.chunks_exact(4) {
        let (r, g, b) = (px[0], px[1], px[2]);
        if r > 60 && r > g.saturating_add(30) && r > b.saturating_add(30) {
            red_dominant += 1;
            max_red = max_red.max(r);
        }
        max_lum = max_lum.max(r.max(g).max(b));
    }
    (red_dominant, max_red, max_lum)
}

#[test]
fn single_point_light_lights_the_box_with_its_own_color() {
    let Some((mut ctx, mut renderer)) = setup() else {
        return;
    };

    let mesh = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let white = renderer.add_material(&ctx, MaterialDesc::from_srgb(1.0, 1.0, 1.0, 1.0));
    renderer.draw(mesh, white, InstanceDesc::from_position(Vec3::ZERO, 1.0));

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 0.0, 6.0);
    camera.look_at(Vec3::ZERO);

    // 参考帧：一盏灯都没有
    let dark = renderer
        .capture(&mut ctx, W, H, &camera, &no_ambient())
        .expect("离屏渲染");
    let dark = red_stats(&dark);

    // 实验帧：只有一盏**纯红**点光，位于相机与箱子之间
    let mut lighting = no_ambient();
    lighting.point_lights = vec![PointLight::new(
        Vec3::new(0.0, 0.0, 4.0),
        Vec3::new(1.0, 0.0, 0.0),
        200.0,
        60.0,
    )];
    let lit = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染");
    let lit = red_stats(&lit);

    assert_eq!(dark.0, 0, "没有点光源时不该有任何偏红像素，实测 {dark:?}");
    // 背景的清除色是淡蓝的，所以这里不看整体亮度，只看"有没有被照出红色"
    assert_eq!(dark.1, 0, "没有灯就不该有亮红像素，实测 {dark:?}");
    assert!(
        lit.0 > 50,
        "单盏点光源没照亮箱子（可能一盏都没生效 → uniform 布局又错位了？），实测 {lit:?}"
    );
    assert!(lit.1 > 150, "被点光源照到的箱子应当很亮，实测 {lit:?}");
}

#[test]
fn more_point_lights_than_the_shader_supports_do_not_panic() {
    let Some((mut ctx, mut renderer)) = setup() else {
        return;
    };

    let mesh = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let white = renderer.add_material(&ctx, MaterialDesc::from_srgb(1.0, 1.0, 1.0, 1.0));
    renderer.draw(mesh, white, InstanceDesc::from_position(Vec3::ZERO, 1.0));

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 0.0, 6.0);
    camera.look_at(Vec3::ZERO);

    // 上限之外的灯必须被安全忽略（不能越界、不能 panic）
    let mut lighting = no_ambient();
    lighting.point_lights = (0..MAX_POINT_LIGHTS + 5)
        .map(|i| {
            PointLight::new(
                Vec3::new(i as f32 * 0.1, 0.0, 4.0),
                Vec3::new(1.0, 1.0, 1.0),
                50.0,
                40.0,
            )
        })
        .collect();
    let image = renderer
        .capture(&mut ctx, W, H, &camera, &lighting)
        .expect("离屏渲染");
    let (red, _max_red, max_lum) = red_stats(&image);

    assert_eq!(red, 0, "灯是白的，不该出现红主导像素");
    assert!(max_lum > 60, "前 8 盏灯应当照亮相箱，实测最亮 {max_lum}");
}
