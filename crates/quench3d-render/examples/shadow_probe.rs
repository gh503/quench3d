//! 渲染器阴影诊断：验证「自阴影」与「阴影方向」两个曾经出错的点。
//!
//! - 自阴影：被拉伸成长方体的地面（`cube` 缩放）在开启阴影时不应整体变暗。
//! - 阴影方向：悬空方块的投影落点必须与太阳方向推导出的解析位置一致（不能镜像）。
//!
//! ```text
//! cargo run -p quench3d-render --example shadow_probe
//! ```

use quench3d_math::*;
use quench3d_render::*;

const W: u32 = 320;
const H: u32 = 240;

fn luminance(px: [u8; 3]) -> f32 {
    0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32
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
    let mut renderer = Renderer::new(&ctx, RendererOptions::default());

    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let ground_mat = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(0.72, 0.74, 0.78, 1.0).pbr(0.0, 0.95),
    );
    let box_mat = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(0.85, 0.30, 0.25, 1.0).pbr(0.0, 0.6),
    );

    // 地面：单位立方体拉伸 40x1x40，顶面正好位于 y = 0
    let ground = Transform::new(
        Vec3::new(0.0, -0.5, 0.0),
        Quat::IDENTITY,
        Vec3::new(40.0, 1.0, 40.0),
    );
    // 悬空方块：底面离地 2.5，位于原点正上方
    let hover = Transform::new(Vec3::new(0.0, 3.0, 0.0), Quat::IDENTITY, Vec3::ONE);

    let mut lighting = Lighting::default();
    lighting.sky.enabled = false;
    lighting.sky.show_grid = false;
    lighting.fog.density = 0.0;
    lighting.exposure = 1.0;
    lighting.ambient_intensity = 0.30;

    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 16.0, 7.0);
    camera.look_at(Vec3::new(0.0, 0.0, 1.0));
    camera.fov_y = 60f32.to_radians();
    camera.near = 0.05;
    camera.far = 500.0;

    let mut render = |lighting: &Lighting, shadows: bool| -> ImageData {
        let mut lighting = lighting.clone();
        if let Some(sun) = lighting.sun.as_mut() {
            sun.cast_shadows = shadows;
        }
        renderer.begin_frame();
        renderer.draw(cube, ground_mat, InstanceDesc::new(ground));
        renderer.draw(cube, box_mat, InstanceDesc::new(hover));
        renderer
            .capture(&mut ctx, W, H, &camera, &lighting)
            .expect("capture")
    };

    let with = render(&lighting, true);
    let without = render(&lighting, false);
    let px = |img: &ImageData, x: u32, y: u32| -> [u8; 3] {
        let i = ((y * img.width + x) * 4) as usize;
        [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]]
    };

    // ---- 1) 自阴影检查：地面顶面在开启阴影后不应变暗 ----
    let mut lit_delta = 0.0f32;
    let mut samples = 0.0f32;
    let mut shadowed = 0u32;
    let mut total = 0u32;
    for y in 0..H {
        for x in 0..W {
            let a = luminance(px(&with, x, y));
            let b = luminance(px(&without, x, y));
            total += 1;
            if b > 30.0 && (b - a).abs() > 4.0 {
                shadowed += 1;
                lit_delta += b - a;
                samples += 1.0;
            }
        }
    }
    println!(
        "受阴影影响像素 {:.2}%（{} / {}）",
        shadowed as f32 / total as f32 * 100.0,
        shadowed,
        total
    );
    if shadowed > 0 {
        println!(
            "受影响像素平均变暗 {:.1} 灰阶",
            lit_delta / samples.max(1.0)
        );
    }

    // ---- 2) 阴影方向：阴影质心 vs 解析落点 ----
    // 太阳方向 (0.42,-0.78,0.46)，单位化后水平/垂直分量之比决定投影偏移。
    let dir = Vec3::new(0.42, -0.78, 0.46).normalize_or_zero();
    let box_bottom = 3.0; // 悬空方块中心高度（阴影质心对应整个轮廓的中心）
    let horizontal = Vec3::new(dir.x, 0.0, dir.z) * (box_bottom / -dir.y);
    let expect_world = Vec3::new(horizontal.x, 0.0, horizontal.z); // 落在地面上（y = 0）
    let view_proj = camera.view_projection();
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
    let base_px = project(Vec3::new(0.0, 0.0, 0.0));
    // 把屏幕像素反投影到 y = 0 地面，得到该像素对应的世界坐标
    let inv_vp = view_proj.inverse();
    let unproject_ground = |x: f32, y: f32| -> Vec3 {
        let ndc = Vec3::new(x / W as f32 * 2.0 - 1.0, 1.0 - y / H as f32 * 2.0, 1.0);
        let far = inv_vp * Vec4::new(ndc.x, ndc.y, ndc.z, 1.0);
        let far = Vec3::new(far.x / far.w, far.y / far.w, far.z / far.w);
        let d = (far - camera.position).normalize_or_zero();
        let t = -camera.position.y / d.y;
        camera.position + d * t
    };

    // 阴影像素 = 开启阴影后显著变暗且非方块自身
    let mut cx = 0.0f64;
    let mut cy = 0.0f64;
    let mut n = 0.0f64;
    for y in 0..H {
        for x in 0..W {
            let a = px(&with, x, y);
            let b = px(&without, x, y);
            // 红蓝差很小 => 属于地面而非红色方块
            let ground_like = (b[0] as i32 - b[2] as i32).abs() < 25;
            if ground_like && luminance(b) - luminance(a) > 6.0 {
                cx += x as f64;
                cy += y as f64;
                n += 1.0;
            }
        }
    }
    match (expect_px, base_px) {
        (Some((ex, ey)), Some((bx, by))) if n > 4.0 => {
            let (mx, my) = ((cx / n) as f32, (cy / n) as f32);
            println!(
                "阴影质心 ({mx:.1},{my:.1})  解析落点 ({ex:.1},{ey:.1})  偏移误差 ({:.1},{:.1}) px  [方块正下方 ({bx:.1},{by:.1})]",
                mx - ex,
                my - ey
            );
            let err = ((mx - ex).powi(2) + (my - ey).powi(2)).sqrt();
            let hit = unproject_ground(mx, my);
            println!(
                "阴影质心落点世界坐标 ({:.2}, 0, {:.2})  解析落点 ({:.2}, 0, {:.2})  误差 ({:.2}, {:.2}) 世界单位",
                hit.x, hit.z, expect_world.x, expect_world.z,
                hit.x - expect_world.x, hit.z - expect_world.z
            );
            println!(
                "阴影方向{}: 质心到解析落点距离 {err:.1}px（阴影像素 {n:.0} 个）",
                if err < 6.0 { "正确" } else { "异常" }
            );
        }
        _ => println!("未检测到阴影像素（n = {n:.0}）"),
    }
}
