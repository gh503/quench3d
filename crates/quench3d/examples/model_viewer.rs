//! 模型查看器：加载 glTF / GLB / OBJ（含 MTL 材质），并可选地把模型变成静态碰撞体。
//!
//! ```text
//! cargo run --release --example model_viewer                  # 用内置生成的小房子演示
//! cargo run --release --example model_viewer -- path/to.glb   # 看自己的模型
//! cargo run --release --example model_viewer -- path/to.obj --collide   # 再生成三角网格碰撞体
//! ```
//!
//! 操作：`空格` 在模型上方丢一个球（验证碰撞体确实存在）、`R` 重新对准模型、
//! `Tab` 切换相机、`F6` 显示刚体包围盒、`F3` 网格、`F4` 阴影。
//!
//! 无窗口自检（CI 可用）：
//! ```text
//! F3D_MODEL_HEADLESS=1 cargo run -p quench3d --example model_viewer
//! ```

use quench3d::prelude::*;
use std::path::{Path, PathBuf};

/// 演示模型写在哪里（工作区 `target/models/`）。
fn demo_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("target")
        .join("models");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 生成一个带两个材质（墙 / 屋顶）的小房子 OBJ + MTL，用来演示 OBJ 与 MTL 解析。
fn write_demo_house(dir: &Path) -> std::io::Result<PathBuf> {
    let obj_path = dir.join("demo_house.obj");
    let mtl_path = dir.join("demo_house.mtl");

    std::fs::write(
        &mtl_path,
        "# Quench3D 生成的示例材质\n\
         newmtl walls\n\
         Kd 0.86 0.80 0.70\n\
         Ks 0.05 0.05 0.05\n\
         Ns 24\n\
         \n\
         newmtl roof\n\
         Kd 0.42 0.22 0.18\n\
         Ks 0.30 0.30 0.30\n\
         Ns 180\n",
    )?;

    let mut obj =
        String::from("# Quench3D 生成的示例模型（墙 + 平屋顶 + 烟囱）\nmtllib demo_house.mtl\n");
    let mut next = 1usize;

    // 墙体：2.0 x 1.4 x 2.0，底面贴地（y = 0）
    obj.push_str("o walls\nusemtl walls\n");
    write_box(
        &mut obj,
        &mut next,
        Vec3::new(0.0, 0.7, 0.0),
        Vec3::new(1.0, 0.7, 1.0),
    );
    // 屋顶平台：2.4 x 0.15 x 2.4，压住墙顶，给小球一个落脚面
    obj.push_str("o roof\nusemtl roof\n");
    write_box(
        &mut obj,
        &mut next,
        Vec3::new(0.0, 1.475, 0.0),
        Vec3::new(1.2, 0.075, 1.2),
    );
    // 烟囱
    obj.push_str("o chimney\nusemtl roof\n");
    write_box(
        &mut obj,
        &mut next,
        Vec3::new(-0.7, 1.9, -0.7),
        Vec3::new(0.18, 0.35, 0.18),
    );

    std::fs::write(&obj_path, obj)?;
    println!("已生成演示模型 {}（含 MTL）", obj_path.display());
    Ok(obj_path)
}

/// 追加一个长方体（6 个四边形面，逆时针朝外），OBJ 顶点索引从 `next` 开始。
fn write_box(obj: &mut String, next: &mut usize, center: Vec3, half: Vec3) {
    let base = *next;
    // 与 MeshData::box_sized 相同的面表：u × v = n
    let faces = [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::Y, Vec3::X),
    ];
    let mut quads: Vec<[usize; 4]> = Vec::new();
    let mut index = base;
    for (n, u, v) in faces {
        let su = extent(u, half);
        let sv = extent(v, half);
        // 面中心 = 体中心沿法线偏移该轴的半长（漏掉这一步会得到自交的退化网格）
        let fc = center + n * extent(n, half);
        let mut quad = [0usize; 4];
        for (k, (du, dv)) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
            .iter()
            .enumerate()
        {
            let p = fc + u * (du * su) + v * (dv * sv);
            obj.push_str(&format!("v {:.4} {:.4} {:.4}\n", p.x, p.y, p.z));
            quad[k] = index;
            index += 1;
        }
        quads.push(quad);
    }
    for q in quads {
        obj.push_str(&format!("f {} {} {} {}\n", q[0], q[1], q[2], q[3]));
    }
    *next = index;
}

fn extent(axis: Vec3, half: Vec3) -> f32 {
    if axis.x.abs() > 0.5 {
        half.x
    } else if axis.y.abs() > 0.5 {
        half.y
    } else {
        half.z
    }
}

struct ModelViewer {
    /// 模型路径。
    path: PathBuf,
    /// 是否同时生成静态三角网格碰撞体。
    collide: bool,
    /// 加载到的场景物体。
    objects: Vec<usize>,
    /// 模型包围盒（用来丢球与对准相机）。
    bounds: Aabb,
    /// 丢球计数。
    spawned: u32,
}

impl ModelViewer {
    fn load(&mut self, engine: &mut Engine) -> Result<(), Box<dyn std::error::Error>> {
        // 清掉上一次加载的物体（保留地面）
        let old: Vec<usize> = self.objects.drain(..).collect();
        for i in old.into_iter().rev() {
            if i < engine.scene.objects.len() {
                engine.scene.objects.remove(i);
            }
        }

        if self.collide {
            let (ids, _body) =
                engine.load_model_as_static(&self.path, Transform::IDENTITY, 0.85)?;
            self.objects = ids;
        } else {
            self.objects = engine.load_model(&self.path, Transform::IDENTITY)?;
        }

        self.bounds = engine
            .scene
            .bounds_of(&engine.renderer, &self.objects)
            .ok_or("模型没有可渲染的包围盒")?;

        println!(
            "加载 {} -> {} 个物体，包围盒 min {:?} max {:?}",
            self.path.display(),
            self.objects.len(),
            self.bounds.min,
            self.bounds.max
        );
        Ok(())
    }

    /// 在模型正上方丢一个球，用来验证碰撞体。
    fn drop_ball(&mut self, engine: &mut Engine) {
        // 落点不要太高：屋顶是 0.15 厚的薄板，而引擎没有 CCD（见 README「已知限制」）
        let top = self.bounds.max.y + 1.0;
        // 撒在模型水平范围内，保证会落在模型上
        let c = self.bounds.center();
        let h = self.bounds.half_extents();
        let jitter = 0.30 * h.x.min(h.z).max(0.2);
        let angle = self.spawned as f32 * 2.399;
        let pos = Vec3::new(c.x + angle.cos() * jitter, top, c.z + angle.sin() * jitter);
        let radius = 0.28;
        let body = engine.world.add_builder(
            BodyBuilder::dynamic(Shape::Sphere { radius })
                .position(pos)
                .restitution(0.35)
                .friction(0.6),
        );
        let material = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.95, 0.62, 0.25, 1.0),
            0.25,
            0.35,
        );
        let index = engine.scene.add_with_body(
            engine.assets.unit_sphere,
            material,
            body,
            &engine.world,
            "球",
        );
        engine.scene.objects[index].transform.scale = Vec3::splat(radius * 2.0);
        self.spawned += 1;
    }
}

impl Game for ModelViewer {
    fn init(&mut self, engine: &mut Engine) {
        // 地面（静态平面）
        let ground = engine
            .world
            .add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }));
        let ground_mat = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.52, 0.55, 0.58, 1.0),
            0.0,
            0.9,
        );
        let index = engine.scene.add_with_body(
            engine.assets.unit_plane,
            ground_mat,
            ground,
            &engine.world,
            "地面",
        );
        engine.scene.objects[index].transform.scale = Vec3::splat(40.0);

        if let Err(e) = self.load(engine) {
            eprintln!("加载模型失败: {e}");
        }

        // 相机对准模型
        engine.camera_mode = CameraMode::Orbit;
        engine.orbit.focus_on(&self.bounds);
        engine.orbit.distance *= 1.05;
        engine.orbit.pitch = 0.30;
        engine.orbit.yaw = 0.72;

        // 丢 3 个球，启动就能看到球落在屋顶上
        for _ in 0..3 {
            self.drop_ball(engine);
        }
    }

    fn update(&mut self, engine: &mut Engine, _dt: f32) {
        if engine.input.key_pressed(KeyCode::Space) {
            self.drop_ball(engine);
        }
        if engine.input.key_pressed(KeyCode::KeyL) {
            if let Err(e) = self.load(engine) {
                eprintln!("重新加载失败: {e}");
            }
        }
    }
}

/// 无窗口自检：生成模型 → 加载 → 生成碰撞体 → 丢球 → 步进 → 断言球停在模型上。
#[cfg(not(target_arch = "wasm32"))]
fn headless_check() -> Result<(), Box<dyn std::error::Error>> {
    let dir = demo_dir();
    let obj = write_demo_house(&dir)?;

    let config = EngineConfig {
        width: 900,
        height: 560,
        ..Default::default()
    };

    let mut viewer = ModelViewer {
        path: obj,
        collide: true,
        objects: Vec::new(),
        bounds: Aabb::EMPTY,
        spawned: 0,
    };

    let image = quench3d::render_offscreen(900, 560, &config, |engine| {
        viewer.init(engine);
        println!(
            "模型包围盒: min ({:.2},{:.2},{:.2}) max ({:.2},{:.2},{:.2})",
            viewer.bounds.min.x,
            viewer.bounds.min.y,
            viewer.bounds.min.z,
            viewer.bounds.max.x,
            viewer.bounds.max.y,
            viewer.bounds.max.z
        );

        if std::env::var("F3D_MODEL_DUMP").is_ok() {
            for (_, b) in engine.world.bodies() {
                if let Shape::TriMesh { vertices, indices } = &b.shape {
                    println!(
                        "  静态三角网格: {} 顶点 {} 三角形",
                        vertices.len(),
                        indices.len() / 3
                    );
                    for t in indices.chunks_exact(3).take(14) {
                        let p: Vec<Vec3> = t.iter().map(|i| vertices[*i as usize]).collect();
                        println!(
                            "    ({:.2},{:.2},{:.2}) ({:.2},{:.2},{:.2}) ({:.2},{:.2},{:.2})",
                            p[0].x, p[0].y, p[0].z, p[1].x, p[1].y, p[1].z, p[2].x, p[2].y, p[2].z
                        );
                    }
                }
            }
        }
        for step in 0..300 {
            engine.world.step(1.0 / 60.0);
            if std::env::var("F3D_MODEL_TRACE").is_ok() && step % 60 == 0 {
                let mut line = format!("  第 {step:3} 步:");
                for b in engine
                    .world
                    .bodies()
                    .filter(|(_, b)| b.is_dynamic())
                    .map(|(_, b)| b.position())
                {
                    line.push_str(&format!(" ({:.2},{:.2},{:.2})", b.x, b.y, b.z));
                }
                println!("{line}");
            }
        }
        engine.scene.sync_from_physics(&engine.world);

        // 统计所有球：应该停在屋顶（y ≈ 1.55）而不是穿过模型掉到地面（y ≈ 0.22）
        let mut resting = 0;
        let mut lowest = f32::MAX;
        let mut nan = 0;
        for obj in &engine.scene.objects {
            if obj.name != "球" {
                continue;
            }
            let p = obj.transform.position;
            if !p.is_finite() {
                nan += 1;
                continue;
            }
            lowest = lowest.min(p.y);
            if p.y > 1.2 {
                resting += 1;
            }
        }
        println!(
            "刚体 {} 接触 {} | 球 {} 个，停在屋顶 {} 个，最低 y = {:.3}，NaN = {}",
            engine.world.body_count(),
            engine.world.contacts().len(),
            viewer.spawned,
            resting,
            lowest,
            nan
        );
        assert_eq!(nan, 0, "出现 NaN");
        assert_eq!(
            resting, viewer.spawned as i32,
            "有球穿过了三角网格碰撞体（最低 y = {lowest:.3}）"
        );
    })?;

    let out = Path::new("target/screenshots");
    std::fs::create_dir_all(out)?;
    quench3d::write_png(&image, out.join("model_viewer.png"))?;
    println!("已输出 target/screenshots/model_viewer.png");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var("F3D_MODEL_HEADLESS").is_ok() {
        return headless_check();
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let collide = args.iter().any(|a| a == "--collide");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from);

    let path = match path {
        Some(p) => p,
        None => write_demo_house(&demo_dir())?,
    };

    let title = format!("Quench3D — 模型查看器（{}）", path.display());
    quench3d::run(
        EngineConfig {
            title,
            width: 1280,
            height: 720,
            ..Default::default()
        },
        ModelViewer {
            path,
            collide,
            objects: Vec::new(),
            bounds: Aabb::EMPTY,
            spawned: 0,
        },
    )
}
