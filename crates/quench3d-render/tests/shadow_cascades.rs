//! 级联阴影（gap #24）的**能力**验证：单级照不到的远处投射体，多级要照得到。
//!
//! 这条测试是 `examples/shadow_probe` 那套测量的回归版。为什么不比"哪个更清晰"：
//! 实测下来（见 `docs/production_readiness.md` §12.2）在"近处小场景 + far=500"的位形下，
//! 单级的 28 米手调正交盒**比任何级联都清晰**——级联必须覆盖到 500 米，
//! 每级还只分到半张贴图。级联真正换来的是**覆盖**：
//!
//! * 单级（半径 10 米）：60 米外的投射体根本不在阴影图里 → 地上一个影子像素都没有；
//! * 4 级：60 米外的影子出现，且**落点与太阳方向推导的解析位置一致**（误差 < 2 米）。
//!
//! 判据不写死像素值，用"有影子的像素数"（地面像素变暗 > 6 灰阶）——
//! 实测单级 0 个、4 级 5 个以上。数量小是因为那个投影在屏幕上本来就小（0.03% 像素），
//! 这也解释了为什么第 42 轮用"整幅图统计"完全看不出差别。
//!
//! 没有可用 GPU 时直接跳过（CI 机器通常没有软件光栅驱动）。

use quench3d_math::*;
use quench3d_render::prelude::*;
use quench3d_render::RendererOptions;

const W: u32 = 320;
const H: u32 = 240;

struct Scene {
    ctx: GpuContext,
}

impl Scene {
    fn new() -> Option<Self> {
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
        Some(Self { ctx })
    }

    /// 渲染一帧：深景（60 米外的投射体 + 单级半径只有 10 米）。
    fn render(&mut self, cascades: u32, shadows: bool) -> ImageData {
        let mut renderer = Renderer::new(
            &self.ctx,
            RendererOptions {
                shadow_cascades: cascades,
                grid: false,
                sky: false,
                ..Default::default()
            },
        );
        let cube = renderer.add_mesh(&self.ctx, &MeshData::cube(1.0));
        let ground_mat = renderer.add_material(
            &self.ctx,
            MaterialDesc::from_srgb(0.72, 0.74, 0.78, 1.0).pbr(0.0, 0.95),
        );
        let box_mat = renderer.add_material(
            &self.ctx,
            MaterialDesc::from_srgb(0.85, 0.30, 0.25, 1.0).pbr(0.0, 0.6),
        );

        let mut lighting = Lighting::default();
        lighting.sky.enabled = false;
        lighting.sky.show_grid = false;
        lighting.fog.density = 0.0;
        lighting.exposure = 1.0;
        lighting.ambient_intensity = 0.30;
        if let Some(sun) = lighting.sun.as_mut() {
            sun.shadow_radius = 10.0; // 单级只罩住相机前十几米
            sun.cast_shadows = shadows;
        }

        let mut camera = Camera::perspective(W as f32 / H as f32);
        camera.position = Vec3::new(0.0, 6.0, 0.0);
        camera.look_at(Vec3::new(0.0, 0.0, -60.0));
        camera.fov_y = 50f32.to_radians();
        camera.near = 0.05;
        camera.far = 200.0;

        let ground = Transform::new(
            Vec3::new(0.0, -0.5, -60.0),
            Quat::IDENTITY,
            Vec3::new(400.0, 1.0, 400.0),
        );
        let hover = Transform::new(Vec3::new(0.0, 8.0, -60.0), Quat::IDENTITY, Vec3::ONE);

        renderer.begin_frame();
        renderer.draw(cube, ground_mat, InstanceDesc::new(ground));
        renderer.draw(cube, box_mat, InstanceDesc::new(hover));
        renderer
            .capture(&mut self.ctx, W, H, &camera, &lighting)
            .expect("capture")
    }
}

fn luminance(p: [u8; 3]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

/// 地面像素里"变暗超过 6 灰阶"的个数 + 这些像素的屏幕质心。
fn shadow_pixels(with: &ImageData, without: &ImageData) -> (u32, (f64, f64)) {
    let (mut n, mut cx, mut cy) = (0u32, 0.0f64, 0.0f64);
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let a = [with.rgba[i], with.rgba[i + 1], with.rgba[i + 2]];
            let b = [without.rgba[i], without.rgba[i + 1], without.rgba[i + 2]];
            // "像地面"：红蓝差小（排除红色方块自身）；且本来是被照亮的
            let ground_like = (b[0] as i32 - b[2] as i32).abs() < 25;
            if ground_like && luminance(b) > 30.0 && luminance(b) - luminance(a) > 6.0 {
                n += 1;
                cx += x as f64;
                cy += y as f64;
            }
        }
    }
    let c = if n > 0 {
        (cx / n as f64, cy / n as f64)
    } else {
        (f64::NAN, f64::NAN)
    };
    (n, c)
}

#[test]
fn a_far_caster_only_gets_a_shadow_when_cascades_cover_it() {
    let Some(mut scene) = Scene::new() else {
        return;
    };
    let without = scene.render(1, false);
    let single = scene.render(1, true);
    let quad = scene.render(4, true);

    let (n_single, _) = shadow_pixels(&single, &without);
    let (n_quad, centroid) = shadow_pixels(&quad, &without);

    assert!(
        n_single <= 1,
        "单级半径 10 米时，60 米外的投射体不该有任何影子，实际有 {n_single} 个像素变暗"
    );
    assert!(
        n_quad >= 4,
        "4 级应当照到 60 米外的投射体（阴影图覆盖视锥），实际只有 {n_quad} 个像素变暗"
    );
    assert!(
        n_quad > n_single,
        "级联的意义就是覆盖更远：单级 {n_single} 个 vs 4 级 {n_quad} 个"
    );

    // 落点必须与太阳方向推导的解析位置一致（方向不能镜像、不能错级）
    let dir = Vec3::new(0.42, -0.78, 0.46).normalize_or_zero();
    let mut camera = Camera::perspective(W as f32 / H as f32);
    camera.position = Vec3::new(0.0, 6.0, 0.0);
    camera.look_at(Vec3::new(0.0, 0.0, -60.0));
    camera.fov_y = 50f32.to_radians();
    camera.near = 0.05;
    camera.far = 200.0;
    let inv_vp = camera.view_projection().inverse();
    let (mx, my) = centroid; // 质心 = (x, y)
    let ndc = Vec3::new(
        mx as f32 / W as f32 * 2.0 - 1.0,
        1.0 - my as f32 / H as f32 * 2.0,
        1.0,
    );
    let far = inv_vp * Vec4::new(ndc.x, ndc.y, ndc.z, 1.0);
    let far = Vec3::new(far.x / far.w, far.y / far.w, far.z / far.w);
    let d = (far - camera.position).normalize_or_zero();
    let hit = camera.position + d * (-camera.position.y / d.y);

    let horizontal = Vec3::new(dir.x, 0.0, dir.z) * (8.0 / -dir.y);
    let expect = Vec3::new(0.0 + horizontal.x, 0.0, -60.0 + horizontal.z);
    let err = ((hit.x - expect.x).powi(2) + (hit.z - expect.z).powi(2)).sqrt();
    assert!(
        err < 2.0,
        "级联阴影的落点 ({:.2}, {:.2}) 与解析位置 ({:.2}, {:.2}) 差 {err:.2} 米",
        hit.x,
        hit.z,
        expect.x,
        expect.z
    );
}
