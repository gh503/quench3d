//! 离屏渲染示例：不需要窗口，直接生成一张 PNG。
//!
//! ```bash
//! cargo run -p quench3d-render --example offline_render
//! ```
//!
//! 用于验证完整渲染管线（PBR / 阴影 / 天空 / 网格 / MSAA / 像素回读），
//! 也适合放进 CI 做渲染回归测试。

use quench3d_math::{vec3, Quat, Transform, Vec3, Vec4};
use quench3d_render::prelude::*;
use quench3d_render::{color, MeshData, RendererOptions};

fn env_flag(name: &str) -> bool {
    std::env::var(name).map(|v| v != "0").unwrap_or(false)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let width = 960u32;
    let height = 640u32;
    let no_sky = env_flag("F3D_NO_SKY");
    let no_grid = env_flag("F3D_NO_GRID");
    let no_shadow = env_flag("F3D_NO_SHADOW");
    let no_ground = env_flag("F3D_NO_GROUND");
    let suffix = std::env::var("F3D_SUFFIX").unwrap_or_default();

    let mut ctx = pollster::block_on(GpuContext::headless(
        width,
        height,
        GpuOptions {
            msaa_samples: 4,
            ..Default::default()
        },
    ))?;
    println!(
        "适配器: {} | 后端: {:?} | 软件光栅: {}",
        ctx.adapter_info.name,
        ctx.adapter_info.backend,
        ctx.is_software()
    );

    let mut renderer = Renderer::new(
        &ctx,
        RendererOptions {
            grid: !no_grid,
            shadows: !no_shadow,
            sky: !no_sky,
            ..Default::default()
        },
    );

    // ---- 网格 ----
    let cube = renderer.add_mesh_named(&ctx, &MeshData::cube(1.0), "cube");
    let sphere = renderer.add_mesh_named(&ctx, &MeshData::sphere(0.5, 40, 24), "sphere");
    let cylinder = renderer.add_mesh_named(&ctx, &MeshData::cylinder(0.5, 0.5, 36), "cylinder");
    let cone = renderer.add_mesh_named(&ctx, &MeshData::cone(0.5, 1.0, 36), "cone");
    let capsule = renderer.add_mesh_named(&ctx, &MeshData::capsule(0.4, 0.35, 32, 20), "capsule");
    let torus = renderer.add_mesh_named(&ctx, &MeshData::torus(0.6, 0.22, 48, 24), "torus");
    let ground = renderer.add_mesh_named(&ctx, &MeshData::plane(1.0, 1), "ground");

    // ---- 材质 ----
    let red = renderer.add_material_named(
        &ctx,
        MaterialDesc::from_srgb(0.86, 0.22, 0.18, 1.0).pbr(0.0, 0.35),
        "red-plastic",
    );
    let gold = renderer.add_material_named(
        &ctx,
        MaterialDesc::from_srgb(1.0, 0.78, 0.34, 1.0).pbr(1.0, 0.18),
        "gold",
    );
    let steel = renderer.add_material_named(
        &ctx,
        MaterialDesc::from_srgb(0.78, 0.80, 0.84, 1.0).pbr(1.0, 0.28),
        "steel",
    );
    let blue = renderer.add_material_named(
        &ctx,
        MaterialDesc::from_srgb(0.22, 0.42, 0.9, 1.0).pbr(0.05, 0.4),
        "blue",
    );
    let emerald = renderer.add_material_named(
        &ctx,
        MaterialDesc::from_srgb(0.12, 0.72, 0.45, 1.0).pbr(0.15, 0.12),
        "emerald",
    );
    let glow = renderer.add_material_named(
        &ctx,
        MaterialDesc::from_srgb(0.2, 0.55, 1.0, 1.0)
            .pbr(0.0, 0.4)
            .emissive(Vec3::new(0.25, 0.6, 1.6), 3.0),
        "glow",
    );
    let checker_tex = renderer.add_texture(
        &ctx,
        &ImageData::checker(128, 8, [186, 190, 198, 255], [58, 60, 68, 255]),
        true,
    );
    let checker = renderer.add_material_named(
        &ctx,
        MaterialDesc::from_srgb(1.0, 1.0, 1.0, 1.0)
            .pbr(0.0, 0.5)
            .with_base_color_texture(checker_tex)
            .with_uv_scale(6.0),
        "checker",
    );

    // ---- 场景 ----
    renderer.begin_frame();

    // 地面（放大 40 倍；F3D_NO_GROUND 直接跳过地面，F3D_NO_GRID 只关网格）
    if !no_ground {
        let g = 40.0f32;
        renderer.draw(
            ground,
            checker,
            InstanceDesc::new(Transform::new(Vec3::ZERO, Quat::IDENTITY, Vec3::splat(g))),
        );
    }

    // 一排金属度递变的球
    for i in 0..5 {
        let t = i as f32 / 4.0;
        renderer.draw(
            sphere,
            steel,
            InstanceDesc::from_position(vec3(-4.2 + i as f32 * 1.1, 0.5, 3.2), 1.0)
                .with_color(Vec4::new(
                    1.0 - t * 0.6,
                    0.75 + t * 0.25,
                    0.35 + t * 0.6,
                    1.0,
                ))
                .with_pbr(t, 0.45 - t * 0.32),
        );
    }

    // 中央的立方体塔
    for i in 0..3 {
        let s = 1.0 - i as f32 * 0.22;
        renderer.draw(
            cube,
            if i == 1 { red } else { blue },
            InstanceDesc::new(Transform::new(
                vec3(0.0, 0.5 + i as f32 * 0.98, 0.0),
                Quat::from_euler_yxz(0.35 * i as f32, 0.5 * i as f32, 0.0),
                Vec3::splat(s),
            )),
        );
    }

    // 圆柱 / 圆锥 / 胶囊 / 圆环
    renderer.draw(
        cylinder,
        emerald,
        InstanceDesc::from_position(vec3(2.6, 0.5, 0.0), 1.0),
    );
    renderer.draw(
        cone,
        red,
        InstanceDesc::from_position(vec3(-2.6, 0.5, 0.0), 1.2),
    );
    renderer.draw(
        capsule,
        glow,
        InstanceDesc::from_position(vec3(2.4, 0.75, -2.4), 1.0).with_emissive(2.0),
    );
    renderer.draw(
        torus,
        gold,
        InstanceDesc::new(Transform::new(
            vec3(-2.4, 0.8, -2.6),
            Quat::from_euler_yxz(0.0, 0.6, 0.5),
            Vec3::splat(1.0),
        )),
    );

    // 阴影接收/投射测试：几个悬空的方块
    for i in 0..4 {
        renderer.draw(
            cube,
            steel,
            InstanceDesc::from_position(
                vec3(-3.5 + i as f32 * 2.4, 2.2 + (i % 2) as f32 * 0.7, -4.5),
                0.55,
            ),
        );
    }

    // ---- 调试线段 ----
    renderer.lines.world_axes(2.0, 0.4);
    renderer.lines.aabb(
        &quench3d_math::Aabb::new(vec3(-0.6, 0.0, -0.6), vec3(0.6, 2.2, 0.6)),
        Vec4::new(0.2, 1.0, 0.4, 0.8),
    );

    // ---- 相机 ----
    let mut camera = Camera::perspective(width as f32 / height as f32);
    camera.position = vec3(8.0, 5.4, 9.6);
    camera.fov_y = 48f32.to_radians();
    camera.look_at(vec3(0.0, 1.1, 0.0));

    let mut lighting = Lighting::studio();
    lighting.sky.show_grid = !no_grid;
    lighting.sky.enabled = !no_sky;
    lighting.fog.density = 0.006;
    lighting.exposure = 0.9;

    let image = renderer.capture(&mut ctx, width, height, &camera, &lighting)?;

    let out_dir = std::path::Path::new("target/screenshots");
    std::fs::create_dir_all(out_dir)?;
    let path = out_dir.join(format!("offline_render{suffix}.png"));
    let buffer = image::RgbaImage::from_raw(image.width, image.height, image.rgba.clone())
        .ok_or("图像缓冲区尺寸不匹配")?;
    buffer.save(&path)?;

    if env_flag("F3D_DUMP_SHADOW") {
        let (sw, sh, depths) = renderer.read_shadow_map(&ctx)?;
        println!("阴影贴图 {sw}x{sh}（'#'=近处有遮挡物，'.'=空）");
        let cols = 72usize;
        let rows = 26usize;
        for r in 0..rows {
            let mut line = String::new();
            for c in 0..cols {
                let x = c * sw as usize / cols;
                let y = r * sh as usize / rows;
                let d = depths[y * sw as usize + x];
                line.push(if d > 0.9999 {
                    '.'
                } else if d > 0.5 {
                    ':'
                } else if d > 0.05 {
                    '*'
                } else {
                    '#'
                });
            }
            println!("{line}");
        }
    }

    let stats = renderer.stats();
    println!(
        "已保存 {} ({}x{})",
        path.display(),
        image.width,
        image.height
    );
    println!(
        "统计: 绘制调用 {} | 实例 {} | 三角形 {} | 剔除 {} | 阴影实例 {} | 线段 {}",
        stats.draw_calls,
        stats.instances,
        stats.triangles,
        stats.culled,
        stats.shadow_instances,
        stats.lines
    );
    // ---- 自检：逐项关闭特性，统计受影响的像素比例 ----
    // 这样「天空 / 网格 / 阴影 / 地面是否真的生效」就不需要靠肉眼判断。
    let baseline = image.clone();
    let diff_percent = |other: &ImageData| -> f32 {
        let changed = baseline
            .rgba
            .chunks_exact(4)
            .zip(other.rgba.chunks_exact(4))
            .filter(|(a, b)| (0..3).any(|i| (a[i] as i32 - b[i] as i32).abs() > 8))
            .count();
        changed as f32 / (baseline.width * baseline.height).max(1) as f32 * 100.0
    };

    if !env_flag("F3D_NO_SELF_CHECK") {
        let mut off = lighting.clone();
        off.sky.enabled = false;
        let img = renderer.capture(&mut ctx, width, height, &camera, &off)?;
        println!("关闭天空: {:.2}% 像素变化", diff_percent(&img));

        let mut off = lighting.clone();
        off.sky.show_grid = false;
        let img = renderer.capture(&mut ctx, width, height, &camera, &off)?;
        println!("关闭网格: {:.2}% 像素变化", diff_percent(&img));

        let mut off = lighting.clone();
        if let Some(sun) = off.sun.as_mut() {
            sun.cast_shadows = false;
        }
        let img = renderer.capture(&mut ctx, width, height, &camera, &off)?;
        println!("关闭阴影: {:.2}% 像素变化", diff_percent(&img));
    }

    let _ = color::hex(0x336699);
    Ok(())
}
