//! 渲染器阴影诊断：验证「自阴影」「阴影方向」与**级联到底值不值**。
//!
//! - 自阴影：被拉伸成长方体的地面（`cube` 缩放）在开启阴影时不应整体变暗。
//! - 阴影方向：悬空方块的投影落点必须与太阳方向推导出的解析位置一致（不能镜像）。
//! - 级联（gap #24）：`cascades = 1/2/3/4` 各测一遍，并给出**半影比例**
//!   （边缘像素 / 核心像素）与**推导的纹素足迹**（世界单位/纹素）。
//!
//! ```text
//! cargo run -p quench3d-render --example shadow_probe
//! ```
//!
//! 半影比例是这么定义与使用的：
//!
//! ```text
//! dark(x) = 亮度(无阴影) - 亮度(有阴影)      // 只统计"像地面"的像素
//! 核心   = dark > 0.75 * max(dark)
//! 边缘   = 0.15 * max(dark) < dark <= 0.75 * max(dark)
//! 半影比例 = 边缘 / 核心
//! ```
//!
//! 同一套相机与几何下，影子轮廓的周长与面积都不变，所以**边缘像素数正比于半影宽度**；
//! 半影比例越小 = 边缘越陡 = 该处阴影贴图的有效分辨率越高。PCF 是固定 3x3，
//! 所以"边缘变窄"只可能来自纹素在世界上变小 —— 这正是级联想换到的东西。
//!
//! 注意：这是**打印数值的诊断程序**（要真显卡），不进 CI；读数写进
//! `docs/production_readiness.md` 的 §12.2。

use quench3d_math::*;
use quench3d_render::*;

const W: u32 = 320;
const H: u32 = 240;

fn luminance(px: [u8; 3]) -> f32 {
    0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32
}

/// 一个相机位形（级联的近处收益要在**近景**上看，远景是看覆盖）。
struct Case {
    label: &'static str,
    camera: Camera,
    /// 投射体中心（世界坐标）。
    caster: Vec3,
    /// 地面中心与边长（单位立方体拉伸）。
    ground_center: Vec3,
    ground_size: f32,
    /// `None` = 用引擎缺省半径（28 米）。深景那一组故意给 10 米：
    /// 单级阴影只罩住相机前十几米，远处的投射体根本不在阴影图里。
    shadow_radius: Option<f32>,
}

/// 一次测量的结果。
#[derive(Default)]
struct Report {
    /// 开启阴影后显著变暗的像素占比（自阴影失控时这个数会暴涨）。
    affected_pct: f32,
    /// 阴影质心与解析落点的像素误差（方向对不对）。
    centroid_err_px: f32,
    /// 地面上的核心 / 边缘像素数。
    core: u32,
    partial: u32,
    /// 推导的纹素足迹：该级视锥切段的世界半径 / 该级纹素数（越小越清晰）。
    texel_world_max: f32,
}

impl Report {
    fn penumbra(&self) -> f32 {
        if self.core == 0 {
            f32::NAN
        } else {
            self.partial as f32 / self.core as f32
        }
    }
}

fn cases() -> [Case; 3] {
    // 远景：原来那套（相机在 16 米高、看原点附近），主要看"覆盖与方向"。
    let mut wide = Camera::perspective(W as f32 / H as f32);
    wide.position = Vec3::new(0.0, 16.0, 7.0);
    wide.look_at(Vec3::new(0.0, 0.0, 1.0));
    wide.fov_y = 60f32.to_radians();
    wide.near = 0.05;
    wide.far = 500.0;

    // 近景：相机凑到方块跟前，视锥切段的近处几级只覆盖几米 —— CSM 该在这里赢。
    let mut near = Camera::perspective(W as f32 / H as f32);
    near.position = Vec3::new(0.0, 3.4, 5.2);
    near.look_at(Vec3::new(0.0, 0.0, 0.0));
    near.fov_y = 55f32.to_radians();
    near.near = 0.05;
    near.far = 500.0;

    // 深景：投射体在 60 米外。这是**级联真正要解决的问题**——
    // 单级要么把半径调大（贴图分辨率摊薄、近处糊），要么就完全照不到远处。
    // 这里把单级半径钉在 10 米，让"照不到"这件事显式暴露出来。
    let mut deep = Camera::perspective(W as f32 / H as f32);
    deep.position = Vec3::new(0.0, 6.0, 0.0);
    deep.look_at(Vec3::new(0.0, 0.0, -60.0));
    deep.fov_y = 50f32.to_radians();
    deep.near = 0.05;
    deep.far = 200.0;

    [
        Case {
            label: "远景",
            camera: wide,
            caster: Vec3::new(0.0, 3.0, 0.0),
            ground_center: Vec3::new(0.0, -0.5, 0.0),
            ground_size: 40.0,
            shadow_radius: None,
        },
        Case {
            label: "近景",
            camera: near,
            caster: Vec3::new(0.0, 3.0, 0.0),
            ground_center: Vec3::new(0.0, -0.5, 0.0),
            ground_size: 40.0,
            shadow_radius: None,
        },
        Case {
            label: "深景(60米外投射体, 单级半径10米)",
            camera: deep,
            caster: Vec3::new(0.0, 8.0, -60.0),
            ground_center: Vec3::new(0.0, -0.5, -60.0),
            ground_size: 400.0,
            shadow_radius: Some(10.0),
        },
    ]
}

/// 推导"每一级的纹素在世界上有多宽"（用 `cascade` 模块自己算，纯 CPU）。
///
/// `cascades = 1` 时贴图全给这一级；多级时每级只分到 2x2 图集里的一块（半张）。
fn texel_footprint(
    camera: &Camera,
    cascades: u32,
    map_size: u32,
    single_radius: f32,
) -> (Vec<f32>, f32) {
    let map = map_size.clamp(512, 4096);
    if cascades <= 1 {
        // 单级路径**不是**按视锥切段，而是引擎那套"围绕相机前方 radius 米的手调正交盒"
        // （`shadow_cascades_for` 的 n <= 1 分支）。所以这里必须用那个半径，
        // 否则会拿"整个视锥的纹素足迹"去跟多级比，把单级说得比实际糊 20 倍。
        let w = 2.0 * single_radius / map as f32;
        return (vec![w], w);
    }
    let tile_texels = (map / 2) as f32;
    let splits = cascade::splits(camera.near, camera.far, cascades, 0.5);
    let inv_vp = camera.view_projection().inverse();
    let corners = cascade::frustum_corners(&inv_vp);
    let mut prev = camera.near;
    let mut per_level = Vec::with_capacity(splits.len());
    for s in &splits {
        let slice = cascade::slice_corners(&corners, camera.near, camera.far, prev, *s);
        let (_, radius) = cascade::bounding_sphere(&slice);
        per_level.push(2.0 * radius / tile_texels);
        prev = *s;
    }
    let max = per_level.iter().cloned().fold(0.0f32, f32::max);
    (per_level, max)
}

fn measure(ctx: &mut GpuContext, cascades: u32, case: &Case) -> Option<Report> {
    let options = RendererOptions {
        shadow_cascades: cascades,
        ..Default::default()
    };
    let mut base_lighting = Lighting::default();
    if let (Some(r), Some(sun)) = (case.shadow_radius, base_lighting.sun.as_mut()) {
        sun.shadow_radius = r;
    }
    let single_radius = base_lighting.sun.map(|s| s.shadow_radius).unwrap_or(28.0);
    let (per_level, texel_world_max) = texel_footprint(
        &case.camera,
        cascades,
        options.shadow_map_size,
        single_radius,
    );
    println!(
        "  cascades={cascades} 推导纹素足迹（世界单位/纹素）: {}  [最差 {texel_world_max:.4}]",
        per_level
            .iter()
            .map(|v| format!("{v:.4}"))
            .collect::<Vec<_>>()
            .join(" / ")
    );

    let mut renderer = Renderer::new(ctx, options);
    let cube = renderer.add_mesh(ctx, &MeshData::cube(1.0));
    let ground_mat = renderer.add_material(
        ctx,
        MaterialDesc::from_srgb(0.72, 0.74, 0.78, 1.0).pbr(0.0, 0.95),
    );
    let box_mat = renderer.add_material(
        ctx,
        MaterialDesc::from_srgb(0.85, 0.30, 0.25, 1.0).pbr(0.0, 0.6),
    );

    // 地面：单位立方体拉伸成一块板，顶面正好位于 y = 0
    let ground = Transform::new(
        case.ground_center,
        Quat::IDENTITY,
        Vec3::new(case.ground_size, 1.0, case.ground_size),
    );
    // 悬空方块
    let hover = Transform::new(case.caster, Quat::IDENTITY, Vec3::ONE);

    let mut lighting = base_lighting;
    lighting.sky.enabled = false;
    lighting.sky.show_grid = false;
    lighting.fog.density = 0.0;
    lighting.exposure = 1.0;
    lighting.ambient_intensity = 0.30;

    let mut render = |renderer: &mut Renderer, lighting: &Lighting, shadows: bool| -> ImageData {
        let mut lighting = lighting.clone();
        if let Some(sun) = lighting.sun.as_mut() {
            sun.cast_shadows = shadows;
        }
        renderer.begin_frame();
        renderer.draw(cube, ground_mat, InstanceDesc::new(ground));
        renderer.draw(cube, box_mat, InstanceDesc::new(hover));
        renderer
            .capture(ctx, W, H, &case.camera, &lighting)
            .expect("capture")
    };

    let with = render(&mut renderer, &lighting, true);
    let without = render(&mut renderer, &lighting, false);
    let px = |img: &ImageData, x: u32, y: u32| -> [u8; 3] {
        let i = ((y * img.width + x) * 4) as usize;
        [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]]
    };

    // ---- 1) 自阴影检查：地面顶面在开启阴影后不应整体变暗 ----
    let mut dark: Vec<f32> = Vec::with_capacity((W * H) as usize);
    let mut affected = 0u32;
    let mut total = 0u32;
    for y in 0..H {
        for x in 0..W {
            let a = luminance(px(&with, x, y));
            let b = luminance(px(&without, x, y));
            total += 1;
            let d = b - a;
            // 只统计"像地面"的像素：红蓝差很小，排除红色方块自身
            let ground_like =
                (px(&without, x, y)[0] as i32 - px(&without, x, y)[2] as i32).abs() < 25;
            dark.push(if ground_like && b > 30.0 {
                d.max(0.0)
            } else {
                0.0
            });
            if b > 30.0 && d.abs() > 4.0 {
                affected += 1;
            }
        }
    }
    let affected_pct = affected as f32 / total as f32 * 100.0;
    let max_dark = dark.iter().cloned().fold(0.0f32, f32::max);
    let mut core = 0u32;
    let mut partial = 0u32;
    if max_dark > 6.0 {
        for d in &dark {
            if *d > 0.75 * max_dark {
                core += 1;
            } else if *d > 0.15 * max_dark {
                partial += 1;
            }
        }
    }

    // ---- 2) 阴影方向：阴影质心 vs 解析落点 ----
    // 太阳方向 (0.42,-0.78,0.46)，单位化后水平/垂直分量之比决定投影偏移。
    let dir = Vec3::new(0.42, -0.78, 0.46).normalize_or_zero();
    // 阴影质心对应整个轮廓的中心，所以拿投射体中心的高度推落点（落在地面 y = 0 上）
    let horizontal = Vec3::new(dir.x, 0.0, dir.z) * (case.caster.y / -dir.y);
    let expect_world = Vec3::new(
        case.caster.x + horizontal.x,
        0.0,
        case.caster.z + horizontal.z,
    );
    let view_proj = case.camera.view_projection();
    let project = |p: Vec3| -> Option<(f32, f32)> {
        let clip = view_proj * Vec4::new(p.x, p.y, p.z, 1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let ndc = Vec3::new(clip.x / clip.w, clip.y / clip.w, clip.z / clip.w);
        Some((
            (ndc.x * 0.5 + 0.5) * W as f32,
            (0.5 - ndc.y * 0.5) * H as f32,
        ))
    };
    let expect_px = project(expect_world);
    let inv_vp = view_proj.inverse();
    let unproject_ground = |x: f32, y: f32| -> Vec3 {
        let ndc = Vec3::new(x / W as f32 * 2.0 - 1.0, 1.0 - y / H as f32 * 2.0, 1.0);
        let far = inv_vp * Vec4::new(ndc.x, ndc.y, ndc.z, 1.0);
        let far = Vec3::new(far.x / far.w, far.y / far.w, far.z / far.w);
        let d = (far - case.camera.position).normalize_or_zero();
        let t = -case.camera.position.y / d.y;
        case.camera.position + d * t
    };

    // 阴影像素 = 开启阴影后显著变暗（`dark` 已经把方块自身排除掉了）
    let mut cx = 0.0f64;
    let mut cy = 0.0f64;
    let mut n = 0.0f64;
    for y in 0..H {
        for x in 0..W {
            if dark[(y * W + x) as usize] > 6.0 {
                cx += x as f64;
                cy += y as f64;
                n += 1.0;
            }
        }
    }
    let mut centroid_err_px = f32::NAN;
    match expect_px {
        Some((ex, ey)) if n > 4.0 => {
            let (mx, my) = ((cx / n) as f32, (cy / n) as f32);
            centroid_err_px = ((mx - ex).powi(2) + (my - ey).powi(2)).sqrt();
            let hit = unproject_ground(mx, my);
            println!(
                "  阴影质心 ({mx:.1},{my:.1})  解析落点 ({ex:.1},{ey:.1})  误差 {centroid_err_px:.1}px"
            );
            println!(
                "  质心落点世界坐标 ({:.2}, 0, {:.2})  解析 ({:.2}, 0, {:.2})  误差 ({:.2}, {:.2}) 世界单位",
                hit.x,
                hit.z,
                expect_world.x,
                expect_world.z,
                hit.x - expect_world.x,
                hit.z - expect_world.z
            );
        }
        _ => println!("  未检测到阴影像素（n = {n:.0}）"),
    }

    Some(Report {
        affected_pct,
        centroid_err_px,
        core,
        partial,
        texel_world_max,
    })
}

fn main() {
    let mut ctx = pollster::block_on(GpuContext::headless(
        W,
        H,
        GpuOptions {
            msaa_samples: 1,
            ..Default::default()
        },
    ))
    .expect("headless GPU");

    for case in cases() {
        println!(
            "== {} ==  相机 ({:.1},{:.1},{:.1}) → 看 ({:.1},{:.1},{:.1})，far={:.0}",
            case.label,
            case.camera.position.x,
            case.camera.position.y,
            case.camera.position.z,
            case.camera.forward().x,
            case.camera.forward().y,
            case.camera.forward().z,
            case.camera.far
        );
        for cascades in 1..=4u32 {
            let Some(r) = measure(&mut ctx, cascades, &case) else {
                continue;
            };
            println!(
                "  -> 受影响像素 {:.2}%  核心/边缘 {} / {}  半影比例 {:.4}  \
                 方向误差 {:.1}px  最差纹素足迹 {:.4} 世界单位",
                r.affected_pct,
                r.core,
                r.partial,
                r.penumbra(),
                r.centroid_err_px,
                r.texel_world_max
            );
        }
    }
}
