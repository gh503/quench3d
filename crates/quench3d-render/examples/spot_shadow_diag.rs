//! 聚光灯阴影图的**逐点对账**（本机 GPU 诊断，不进 CI 判据）。
//!
//! ```text
//! cargo run -p quench3d-render --example spot_shadow_diag
//! ```
//!
//! 它回答一个"看图看不出来"的问题：阴影图里的深度，和 CPU 侧用 `spot_shadow_camera`
//! 算出来的深度，是不是**同一个坐标系**。做法是拿地面上一批点：
//!
//!   (a) 用 `spot_shadow_camera` 投影，得到 uv 与归一化深度 ndc.z；
//!   (b) 按同一个 uv 在阴影图里读回深度。
//!
//! 判读方式：
//!   * 两者差 ~1e-6  → 映射一致，自阴影只可能来自 bias（`spot_shadow.rs` 的常量）；
//!   * 差一个常数或随位置漂移 → uv / 矩阵 / V 翻转有一处不对。
//!
//! 第 54 轮就是靠它发现"聚光灯在为空的阴影图上采样"：地图内容为空时这批点的差值
//! 恰好等于 `1.0 - ndc.z`（也就是"图上一个遮挡物都没有"）。
//! 场景刻意简单：一块地面 + 一盏投影聚光灯，没有别的光，便于对读。

use quench3d_math::*;
use quench3d_render::prelude::*;
use quench3d_render::spot_shadow::spot_shadow_camera;
use quench3d_render::{GpuContext, GpuOptions, Renderer, RendererOptions};

fn main() -> Result<(), String> {
    let (width, height) = (320u32, 320u32);
    let mut ctx = pollster::block_on(GpuContext::headless(
        width,
        height,
        GpuOptions {
            msaa_samples: 1,
            ..Default::default()
        },
    ))
    .map_err(|e| format!("没有可用的 GPU 后端: {e}"))?;
    let mut renderer = Renderer::new(
        &ctx,
        RendererOptions {
            grid: false,
            sky: false,
            shadows: true,
            ..Default::default()
        },
    );

    let ground = renderer.add_mesh(&ctx, &MeshData::plane(40.0, 1));
    let mat = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(0.62, 0.62, 0.62, 1.0).pbr(0.0, 0.95),
    );
    renderer.begin_frame();
    renderer.draw(ground, mat, InstanceDesc::from_position(Vec3::ZERO, 1.0));

    let mut camera = Camera::perspective(width as f32 / height as f32);
    camera.position = vec3(0.0, 14.0, 16.0);
    camera.fov_y = 45f32.to_radians();
    camera.look_at(Vec3::ZERO);

    // 没有太阳、没有环境光：画面里能亮的只有这盏聚光灯，便于把"采样错图"与"别处漏光"分开。
    let mut lighting = Lighting::none();
    lighting.sky.enabled = false;
    lighting.sky.show_grid = false;
    lighting.sky.zenith = Vec3::ZERO;
    lighting.sky.horizon = Vec3::ZERO;
    lighting.sky.ground = Vec3::ZERO;
    lighting.ambient_intensity = 0.0;
    let spot = SpotLight::new(
        vec3(-3.0, 9.0, 0.0),
        vec3(3.0, -9.0, 0.0),
        Vec3::ONE,
        4000.0,
        40.0,
        20.0,
        30.0,
    )
    .with_cast_shadows(true);
    lighting.spot_lights = vec![spot];

    let image = renderer.capture(&mut ctx, width, height, &camera, &lighting)?;
    println!(
        "渲染完成 {}x{}，聚光灯阴影绘制调用 {}，地图边长 {}",
        image.width,
        image.height,
        renderer.stats().spot_shadow_draw_calls,
        renderer.spot_shadow_map_size()
    );

    let cam = spot_shadow_camera(&spot);
    println!(
        "阴影相机：近 {} 远 {}，tan(半角) {:.4}，深度偏移 {:.1} 纹素 / 法线偏移 {:.1} 纹素",
        cam.near, cam.far, cam.tan_half_fov, cam.depth_bias_texels, cam.normal_offset_texels
    );
    let (sw, sh, depths) = renderer.read_spot_shadow_map(&ctx)?;
    println!("阴影图 {sw}x{sh}（'.' = 空，越靠 '#' 越近）");
    for r in 0..24 {
        let mut line = String::new();
        for c in 0..72 {
            let x = c * sw as usize / 72;
            let y = r * sh as usize / 24;
            let d = depths[y * sw as usize + x];
            line.push(match d {
                d if d > 0.9999 => '.',
                d if d > 0.5 => ':',
                d if d > 0.05 => '*',
                _ => '#',
            });
        }
        println!("{line}");
    }

    // 逐点对账：地面 -4..4 的 9x9 网格
    let mut worst = 0.0f32;
    let mut worst_uv = (0.0f32, 0.0f32);
    let mut n = 0usize;
    let mut sum = 0.0f64;
    for i in 0..9 {
        for j in 0..9 {
            let p = vec3(-4.0 + i as f32, 0.0, -4.0 + j as f32);
            let ndc = cam.view_proj.project_point3(p);
            let uv = vec2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
            if !(0.0..1.0).contains(&uv.x) || !(0.0..1.0).contains(&uv.y) {
                continue;
            }
            let x = ((uv.x * sw as f32) as usize).min(sw as usize - 1);
            let y = ((uv.y * sh as f32) as usize).min(sh as usize - 1);
            let d = depths[y * sw as usize + x] - ndc.z;
            sum += d as f64;
            n += 1;
            if d.abs() > worst {
                worst = d.abs();
                worst_uv = (uv.x, uv.y);
            }
        }
    }
    println!(
        "对账 {n} 个点：平均（图上 - CPU 投影）= {:.6}，最大 |差| = {worst:.6} @ uv ({:.3}, {:.3})",
        sum / n.max(1) as f64,
        worst_uv.0,
        worst_uv.1
    );
    println!("（图上全空时这批差值应恰好等于 1.0 - ndc.z ≈ 0.033 —— 那就是「采到了空的图」）");
    Ok(())
}
