//! 点光源立方体阴影的**逐面对账**（本机 GPU 诊断，不进 CI 判据）。
//!
//! ```text
//! cargo run -p quench3d-render --example point_shadow_diag
//! ```
//!
//! 它回答三个"看图看不出来"的问题：
//!
//! 1. **面序有没有错**：六个面里，哪个面应该有内容？灯在 `(0, 3, 0)`，五个 1 米盒子
//!    分别在 `+x / -x / +y / -y / +z / -z` 里各偏 2 米（`-y` 那盏在正下方 1.5 米）：
//!    盒子是**封闭**的，而阴影管线是"正面剔除 + 深度偏移"（和太阳/聚光同一条管线），
//!    所以每个盒子的**背面**才会被写进图里；地面这种单面几何会被整面剔掉
//!    （这正是想要的：地面不遮挡自己）。于是判据是"五个面各有内容、`+Y` 面必须全空"，
//!    而且五面里最小的深度都应当≈离灯 1.5 米处的归一化深度。
//! 2. **深度是不是同一个坐标系**：拿地面上一批点，用 `cube_face_uv` 选面 + 相机矩阵
//!    投影得到 uv，再按同一个 uv 在对应的层里读回深度，与 `normalized_depth(主分量)`
//!    比。差 ~1e-6 = 映射一致；差一个常数或随位置漂移 = uv / 矩阵 / 层序有一处不对。
//! 3. **"零差异基线"**：同一场景连渲染两次必须**逐位相同**（否则后面测出来的"差异"
//!    分不清是阴影还是抖动）；然后打开 `cast_shadows` 再渲染，差异像素数必须 > 0。
//!
//! 第 55 轮就是靠第 1 条确认"六面都真的被渲染到了"，靠第 3 条确认"没有阴影时画面
//! 一个像素都不变"（老场景的兼容性底线）。

use quench3d_math::*;
use quench3d_render::point_shadow::{cube_face_uv, point_shadow_camera};
use quench3d_render::prelude::*;
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
    let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
    let mat = renderer.add_material(
        &ctx,
        MaterialDesc::from_srgb(0.62, 0.62, 0.62, 1.0).pbr(0.0, 0.95),
    );
    renderer.begin_frame();
    renderer.draw(ground, mat, InstanceDesc::from_position(Vec3::ZERO, 1.0));
    // 五个盒子：±x、±z 各偏 2 米，还有一个正在灯下方（顶面离灯正好 1.5 米）。
    // 每个盒子离灯的**主分量**都是 1.5 米 → 五面的最小深度应当彼此相同。
    let boxes = [
        vec3(2.0, 2.0, 0.0),  // +X 面
        vec3(-2.0, 2.0, 0.0), // -X 面
        vec3(0.0, 2.0, 2.0),  // +Z 面
        vec3(0.0, 2.0, -2.0), // -Z 面
        vec3(0.0, 1.0, 0.0),  // -Y 面（顶面 y = 1.5）
    ];
    for b in boxes {
        renderer.draw(cube, mat, InstanceDesc::from_position(b, 1.0));
    }

    let mut camera = Camera::perspective(width as f32 / height as f32);
    camera.position = vec3(0.0, 12.0, 14.0);
    camera.fov_y = 45f32.to_radians();
    camera.look_at(Vec3::ZERO);

    // 没有太阳、没有环境光：画面里能亮的只有这盏点光源
    let mut lighting = Lighting::none();
    lighting.sky.enabled = false;
    lighting.sky.show_grid = false;
    lighting.sky.zenith = Vec3::ZERO;
    lighting.sky.horizon = Vec3::ZERO;
    lighting.sky.ground = Vec3::ZERO;
    lighting.ambient_intensity = 0.0;
    let light = PointLight::new(vec3(0.0, 3.0, 0.0), Vec3::ONE, 60.0, 30.0);
    lighting.point_lights = vec![light];

    // ---- 第 3 条：零差异基线 ----
    let off_a = renderer.capture(&mut ctx, width, height, &camera, &lighting)?;
    let off_b = renderer.capture(&mut ctx, width, height, &camera, &lighting)?;
    let biggest = |a: &ImageData, b: &ImageData| -> i32 {
        a.rgba
            .iter()
            .zip(b.rgba.iter())
            .map(|(x, y)| (*x as i32 - *y as i32).abs())
            .max()
            .unwrap_or(0)
    };
    let changed = |a: &ImageData, b: &ImageData| -> usize {
        a.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.rgba.as_chunks::<4>().0)
            .filter(|(p, q)| (0..3).any(|i| (p[i] as i32 - q[i] as i32).abs() > 8))
            .count()
    };
    let baseline_max = biggest(&off_a, &off_b);
    println!(
        "[基线] 不投影连渲两次：逐位最大差 {baseline_max}（必须 0），差异像素 {}",
        changed(&off_a, &off_b)
    );

    lighting.point_lights = vec![light.with_cast_shadows(true)];
    let on = renderer.capture(&mut ctx, width, height, &camera, &lighting)?;
    let shadow_px = changed(&off_a, &on);
    println!(
        "[效果] 打开投影：差异像素 {shadow_px} / {}（{:.2}%），点光阴影绘制调用 {}，单面边长 {}",
        width * height,
        shadow_px as f32 / (width * height) as f32 * 100.0,
        renderer.stats().point_shadow_draw_calls,
        renderer.point_shadow_map_size()
    );

    let cam = point_shadow_camera(&light);
    println!(
        "阴影相机：近 {:.4} 远 {:.1}，tan(半面角) {:.4}，深度偏移 {:.1} 纹素 / 法线偏移 {:.1} 纹素",
        cam.near, cam.far, cam.tan_half_fov, cam.depth_bias_texels, cam.normal_offset_texels
    );

    // ---- 第 1 条：六个面各自的占空比 + 最小深度 ----
    let map = renderer.point_shadow_map_size() as usize;
    let names = ["+X", "-X", "+Y", "-Y", "+Z", "-Z"];
    let mut min_depth = [1.0f32; 6];
    let mut filled = [0usize; 6];
    let mut uv_sum = [(0.0f64, 0.0f64); 6];
    let mut per_face = Vec::new();
    for face in 0..6 {
        let (w, h, depths) = renderer.read_point_shadow_map(&ctx, face)?;
        assert_eq!((w as usize, h as usize), (map, map), "面尺寸不对");
        for (idx, d) in depths.iter().enumerate() {
            if *d < 0.9999 {
                filled[face] += 1;
                min_depth[face] = min_depth[face].min(*d);
                uv_sum[face].0 += (idx % map) as f64 / map as f64;
                uv_sum[face].1 += (idx / map) as f64 / map as f64;
            }
        }
        per_face.push(depths);
    }
    let mut centroid = [(0.5f32, 0.5f32); 6];
    for face in 0..6 {
        if filled[face] > 0 {
            centroid[face] = (
                (uv_sum[face].0 / filled[face] as f64) as f32,
                (uv_sum[face].1 / filled[face] as f64) as f32,
            );
        }
        let pct = filled[face] as f32 / (map * map) as f32 * 100.0;
        println!(
            "[面 {}] 有内容的纹素 {:>7} / {}（{:5.2}%），最小深度 {:.6}，重心 uv ({:.3}, {:.3})",
            names[face],
            filled[face],
            map * map,
            pct,
            min_depth[face],
            centroid[face].0,
            centroid[face].1
        );
    }
    // 判据（打印出来，供人/脚本核对）
    assert_eq!(filled[2], 0, "+Y 面必须完全是空的（灯上方什么都没有）");
    // 每个盒子离灯的主分量都是 1.5 米；阴影管线带 slope_scale = 2 的深度偏移，
    // 所以允许 0.01 的偏差（偏移量本身会被算出来打印，见下面的对账）。
    let want = cam.normalized_depth(1.5);
    for face in [0usize, 1, 3, 4, 5] {
        assert!(
            filled[face] > 500,
            "{} 面应该有那个盒子（只有 {} 个纹素）",
            names[face],
            filled[face]
        );
        assert!(
            (min_depth[face] - want).abs() < 0.01,
            "{} 面的最小深度应≈1.5 米处的归一化深度 {want}，实际 {}（差 {}）",
            names[face],
            min_depth[face],
            min_depth[face] - want
        );
    }

    // v 方向：五个盒子都在灯**下方**（y < 3），所以在 ±X/±Z 四个面里它们必须出现在
    // 贴图的**下半**（v > 0.5）。v 翻转的话重心会跑到上半去。
    for face in [0usize, 1, 4, 5] {
        assert!(
            centroid[face].1 > 0.5,
            "{} 面的内容应该在贴图下半（盒子在灯下方），实际重心 v = {}",
            names[face],
            centroid[face].1
        );
    }

    // ---- 第 2 条：对账（+X 盒子的 **z = +0.5 那一侧**）
    // 那一片是盒子的背面（管线剔正面，所以它一定被画进去），而且它上面每个点的**主分量
    // 随位置变化**（= |dx| = x，从 1.6 变到 2.4），于是"uv 映射对了没有"变成了一个
    // 有区分度的判据：u 一旦镜像/面序一旦错，读回来的深度会差 0.06 以上（或者读到空值 1.0）。
    let mut diffs = Vec::new();
    let mut worst = 0.0f32;
    let mut worst_at = (0usize, 0.0f32, 0.0f32);
    for i in 0..6 {
        for j in 0..6 {
            let p = vec3(1.6 + i as f32 * 0.16, 1.6 + j as f32 * 0.16, 0.5);
            let d = p - light.position;
            let (face, u, v) = cube_face_uv(d);
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            assert_eq!(
                face, 0,
                "z = 0.5 那一侧应当落在 +X 面（{p:?} → face {face}）"
            );
            assert_eq!(
                d.abs().max_element(),
                d.x.abs(),
                "这一片的主分量应当就是 |dx|（{p:?}）"
            );
            let expect = cam.normalized_depth(d.x.abs());
            let x = ((u * map as f32) as usize).min(map - 1);
            let y = ((v * map as f32) as usize).min(map - 1);
            let got = per_face[face][y * map + x];
            let diff = got - expect;
            if diff.abs() > worst {
                worst = diff.abs();
                worst_at = (face, u, v);
            }
            diffs.push(diff);
        }
    }
    let mean = diffs.iter().sum::<f32>() / diffs.len().max(1) as f32;
    let lo = diffs.iter().cloned().fold(f32::MAX, f32::min);
    let hi = diffs.iter().cloned().fold(f32::MIN, f32::max);
    println!(
        "[对账] +X 盒子背面 {} 个点（深度跨 {:.4}）：平均（图上 - CPU）= {mean:.6}，\
         极差 {:.6}，最大 |差| = {worst:.6} @ uv ({:.3}, {:.3})",
        diffs.len(),
        cam.normalized_depth(2.4) - cam.normalized_depth(1.6),
        hi - lo,
        worst_at.1,
        worst_at.2
    );
    println!(
        "（uv/面序/far-near 任何一处错，这批点都会差出一大截或者读到空值 1.0；\
         残余的常数偏移来自阴影管线的 slope 深度偏移）"
    );
    assert!(diffs.len() >= 20, "对账点太少: {}", diffs.len());
    // 残余偏移的**极差**来自阴影管线的 slope 深度偏移（同一片平面由多个三角形拼成，
    // 每个三角形各带一份偏移），量级 ~2e-3；而这一片平面的深度跨度是 0.063。
    // 一旦 u 镜像 / 面序错，读回来的深度会横跨那 0.063，极差是这里的 10 倍以上。
    assert!(
        (hi - lo) < 0.005,
        "同一片平面上的偏移必须是常数（极差 {} 应远小于深度跨度 0.063）",
        hi - lo
    );
    assert!(
        worst < 0.01,
        "最大偏差 {worst} 太大（图上那一点不是 CPU 投影的那个点）"
    );
    Ok(())
}
