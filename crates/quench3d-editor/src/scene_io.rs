//! 场景文件读写：直接复用 CLI 那套 `quench3d.scene` 格式。
//!
//! 为什么不自己定一套格式：CLI、MCP 助手、编辑器必须认同一份 `scene.json`，
//! 否则「CLI 生成 → 编辑器手工改 → CLI 渲染成视频」这条链在中间就断了。
//! 这里只做两件事：把引擎里现在的场景**导出**成 `SceneDoc`，
//! 以及把 `SceneDoc` **装回**引擎（都借 CLI 的库代码，不重写一遍物理/材质装配）。

use crate::doc::{Doc, TexturePaths};
use quench3d::prelude::*;
use quench3d_cli::runner;
use quench3d_cli::scene_doc::{
    BodyKind as DocBodyKind, CameraDoc, MaterialDoc, ObjectDoc, SceneDoc, ShapeDoc, ShapeKind,
    FORMAT, VERSION,
};

/// 导出结果。
pub struct SavedScene {
    /// 场景 JSON 文本。
    pub json: String,
    /// 没法用场景格式表达的物体（导入的模型、凸包…），只报告，不静默丢。
    pub skipped: Vec<String>,
}

/// 一次场景装载的结果。
pub struct Installed {
    /// 装进引擎的物体数。
    pub objects: usize,
    /// 场景文件名（`名字.json`）。
    pub name: String,
    /// 模型实例映射（`models[i]` → 它展开出来的物体下标）。
    pub instances: Vec<runner::ModelInstance>,
}

/// 把引擎当前场景导出成 `quench3d.scene` JSON。
pub fn scene_to_json(engine: &Engine, doc: &Doc) -> Result<SavedScene, String> {
    // **以最近载入的那份文档为底稿**：编辑器只"懂"物体、地面、材质标量与相机，
    // 而 `lighting` / `simulate` / `render` / `probes` / `models` 这些段它既不改也不该丢。
    // 曾经这里是 `SceneDoc::default()` 起头 —— 后果是「CLI 生成（带光照与渲染设置）
    // → 编辑器改一改 → 存盘」，光照、渲染设置、模型引用被**静默清零**。
    let mut out = doc.base_doc.clone().unwrap_or_default();
    out.format = FORMAT.to_string();
    out.version = VERSION;
    let inherited_models = out.models.len();
    // 这三段由当前引擎状态重建，先清空（`models` 反过来要留着：模型实例被展开成
    // 普通物体后，非预设网格那部分本来就导不出来，清了等于把它们整个丢掉）。
    out.objects.clear();
    out.materials.clear();
    out.ground = None;
    out.name = Some(
        std::path::Path::new(&doc.file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("scene")
            .to_string(),
    );
    let g = engine.world.gravity();
    out.gravity = Some([g.x, g.y, g.z]);
    out.camera = CameraDoc {
        position: Some([
            engine.orbit.focus.x + 0.0,
            engine.orbit.focus.y,
            engine.orbit.focus.z,
        ]),
        look_at: Some([
            engine.orbit.focus.x,
            engine.orbit.focus.y,
            engine.orbit.focus.z,
        ]),
        distance: Some(engine.orbit.distance),
        yaw_deg: Some(engine.orbit.yaw.to_degrees()),
        pitch_deg: Some(engine.orbit.pitch.to_degrees()),
        fov_deg: Some(engine.orbit.fov_y.to_degrees()),
        ..Default::default()
    };
    // 相机位置只在"用 position/look_at 描述"时才写，否则让 yaw/pitch/distance 说话。
    out.camera.position = None;

    // 光照：编辑器能改的字段从**引擎**写回（所以拖了太阳方向、改了曝光，存盘会生效），
    // 编辑器不管的（点光、bloom…）沿用底稿。`preset` 必须清掉：预设会在载入时
    // 再覆盖一遍显式字段，留着它等于"存了但不算数"。
    let light = &engine.scene.lighting;
    let mut lighting = std::mem::take(&mut out.lighting);
    lighting.preset = None;
    if let Some(sun) = &light.sun {
        lighting.sun_direction = Some([sun.direction.x, sun.direction.y, sun.direction.z]);
        lighting.sun_intensity = Some(sun.intensity);
        lighting.shadows = Some(sun.cast_shadows);
    }
    lighting.ambient = Some(light.ambient_intensity);
    lighting.exposure = Some(light.exposure);
    lighting.sky = Some(light.sky.enabled);
    lighting.grid = Some(light.sky.show_grid);
    out.lighting = lighting;

    let mut skipped = Vec::new();
    for (index, object) in engine.scene.objects.iter().enumerate() {
        let name = if object.name.trim().is_empty() {
            format!("物体 {}", index + 1)
        } else {
            object.name.clone()
        };
        let body = object.body.and_then(|h| engine.world.body(h));
        // 形状：有刚体就用物理形状（权威），没有就从预设网格反推。
        let shape = match body {
            Some(b) => match shape_doc_from_shape(&b.shape, object.transform.scale) {
                Some(shape) => shape,
                None => {
                    skipped.push(format!("{name}（三角网碰撞体）"));
                    continue;
                }
            },
            None => match shape_doc_from_mesh(engine, object) {
                Some(shape) => shape,
                None => {
                    skipped.push(format!("{name}（不是预设网格）"));
                    continue;
                }
            },
        };
        // 地面：平面 + 静态 → 写成 `ground`（场景格式里地面是专门字段）。
        //
        // 高度取**地面表面**而不是物体原点：物理上的半空间就在原点平面上，
        // 而视觉上多半放了一块"薄板"（把单位立方体压扁、中心压在 -半厚处）。
        // 直接写原点的话，存一次再读一次地面会掉下去半个板厚。
        if matches!(shape.kind, ShapeKind::Plane) {
            if let Some(b) = body {
                if b.is_static() {
                    out.ground = Some(quench3d_cli::scene_doc::GroundDoc {
                        size: object.transform.scale.x.max(1.0),
                        y: object.transform.position.y + surface_offset(engine, object),
                        friction: b.friction,
                        restitution: b.restitution,
                        material: None,
                    });
                    continue;
                }
            }
        }

        let material_name = format!("mat{index}");
        // 同名物体在原文档里用过的材质：连贴图、UV、开关一起继承。
        // 编辑器没有贴图界面 —— 但"没有界面"不等于"应该丢"。
        let inherited = doc.base_doc.as_ref().and_then(|base| {
            let source = base
                .objects
                .iter()
                .find(|o| o.name.as_deref() == Some(name.as_str()))?;
            base.materials.get(source.material.as_deref()?).cloned()
        });
        let base = engine
            .renderer
            .material(object.material)
            .map(|m| m.base_color)
            .unwrap_or(Vec4::ONE);
        let linear = Vec4::new(
            base.x * object.color.x,
            base.y * object.color.y,
            base.z * object.color.z,
            base.w * object.color.w,
        );
        let source = engine.renderer.material(object.material);
        let mut material = inherited.unwrap_or(MaterialDoc {
            color: [0.0, 0.0, 0.0],
            metallic: 0.0,
            roughness: 0.5,
            emissive: 0.0,
            alpha: None,
            // 以下几项只有从原文档继承时才可能非空。
            emissive_color: None,
            uv_scale: None,
            base_color_texture: None,
            normal_texture: None,
            metallic_roughness_texture: None,
            emissive_texture: None,
            normal_scale: None,
            alpha_cutoff: None,
            alpha_blend: None,
            unlit: None,
            double_sided: None,
        });
        // 编辑器改得动的只有这五个标量，其余（贴图、UV、开关）保持原样。
        material.color = [
            quench3d::render::color::linear_to_srgb(linear.x.clamp(0.0, 1.0)),
            quench3d::render::color::linear_to_srgb(linear.y.clamp(0.0, 1.0)),
            quench3d::render::color::linear_to_srgb(linear.z.clamp(0.0, 1.0)),
        ];
        material.metallic = object
            .metallic
            .unwrap_or_else(|| source.map(|m| m.metallic).unwrap_or(0.0));
        material.roughness = object
            .roughness
            .unwrap_or_else(|| source.map(|m| m.roughness).unwrap_or(0.5));
        material.emissive = object.emissive_strength;
        material.alpha = if linear.w < 1.0 {
            Some(linear.w.clamp(0.0, 1.0))
        } else {
            None
        };
        // 用户在编辑器里显式指定过的贴图/开关优先（按物体名匹配；没有就用继承来的）。
        if let Some(explicit) = doc.textures.get(&name) {
            apply_texture_overrides(&mut material, explicit);
        }
        out.materials.insert(material_name.clone(), material);

        let (rotation_deg, position) = match body {
            Some(b) => (to_euler_xyz_deg(b.rotation()), b.position()),
            None => (
                to_euler_xyz_deg(object.transform.rotation),
                object.transform.position,
            ),
        };
        let body_kind = match body {
            None => DocBodyKind::None,
            Some(b) if b.is_dynamic() => DocBodyKind::Dynamic,
            Some(b) if b.is_static() => DocBodyKind::Static,
            Some(_) => DocBodyKind::Kinematic,
        };
        out.objects.push(ObjectDoc {
            name: Some(name),
            shape,
            position: Some([position.x, position.y, position.z]),
            rotation_deg: Some(rotation_deg),
            scale: Some([
                object.transform.scale.x,
                object.transform.scale.y,
                object.transform.scale.z,
            ]),
            material: Some(material_name),
            body: body_kind,
            density: None,
            restitution: body.map(|b| b.restitution),
            friction: body.map(|b| b.friction),
            velocity: body
                .map(|b| b.linear_velocity())
                .filter(|v| v.length_squared() > 0.0)
                .map(|v| [v.x, v.y, v.z]),
            angular_velocity: body
                .map(|b| b.angular_velocity())
                .filter(|v| v.length_squared() > 0.0)
                .map(|v| [v.x, v.y, v.z]),
            linear_damping: body.map(|b| b.linear_damping),
            angular_damping: body.map(|b| b.angular_damping),
            gravity_scale: body.map(|b| b.gravity_scale),
            mass: body.filter(|b| b.is_dynamic()).map(|b| b.mass()),
            allow_sleep: body.map(|b| b.can_sleep()),
            visible: object.visible,
        });
    }

    // 关节与动作按**物体名**引用：编辑器删掉的物体不能留下悬空引用
    // （那会让存出来的文件自己都读不回来）。
    let names: std::collections::HashSet<String> =
        out.objects.iter().filter_map(|o| o.name.clone()).collect();
    let before = out.joints.len();
    out.joints
        .retain(|j| names.contains(&j.a) && names.contains(&j.b));
    if before > out.joints.len() {
        skipped.push(format!(
            "{} 个关节（引用的物体已不存在）",
            before - out.joints.len()
        ));
    }
    let before = out.actions.len();
    // `$ground` / `$model[i]` 这类内置目标不按物体名解析，保留。
    out.actions
        .retain(|a| a.target.starts_with('$') || names.contains(&a.target));
    if before > out.actions.len() {
        skipped.push(format!(
            "{} 个动作（目标物体已不存在）",
            before - out.actions.len()
        ));
    }
    if inherited_models > 0 && !skipped.is_empty() {
        skipped.push(format!(
            "注意：原文档有 {inherited_models} 个 models 引用，已原样保留；\
             模型展开出来的物体不写成 objects（它们的变换改动已写回 models 引用）"
        ));
    }

    Ok(SavedScene {
        json: out.to_json_pretty(),
        skipped,
    })
}

/// 把编辑器里显式设置的贴图/开关盖到材质上（只覆盖非 `None` 的项）。
fn apply_texture_overrides(material: &mut MaterialDoc, explicit: &TexturePaths) {
    if explicit.base_color.is_some() {
        material.base_color_texture = explicit.base_color.clone();
    }
    if explicit.normal.is_some() {
        material.normal_texture = explicit.normal.clone();
    }
    if explicit.metallic_roughness.is_some() {
        material.metallic_roughness_texture = explicit.metallic_roughness.clone();
    }
    if explicit.emissive.is_some() {
        material.emissive_texture = explicit.emissive.clone();
    }
    if explicit.uv_scale.is_some() {
        material.uv_scale = explicit.uv_scale;
    }
    if explicit.normal_scale.is_some() {
        material.normal_scale = explicit.normal_scale;
    }
    if explicit.alpha_cutoff.is_some() {
        material.alpha_cutoff = explicit.alpha_cutoff;
    }
    if explicit.alpha_blend.is_some() {
        material.alpha_blend = explicit.alpha_blend;
    }
    if explicit.unlit.is_some() {
        material.unlit = explicit.unlit;
    }
    if explicit.double_sided.is_some() {
        material.double_sided = explicit.double_sided;
    }
}

/// 把场景 JSON 装进引擎。
///
/// 返回 `(物体数, 场景名)`。会顺带把相机、光照、关节一起装好 ——
/// 因为装配代码就是 CLI 渲染用的那一份，编辑器里看到的和 CLI 渲出来的是同一个场景。
pub fn load_into_engine(engine: &mut Engine, text: &str) -> Result<(usize, String), String> {
    let installed = install_text(engine, text)?;
    Ok((installed.objects, installed.name))
}

/// 装进引擎并**返回模型实例映射**（撤销/重做要重新展开模型，靠它刷新编辑器里的映射）。
pub(crate) fn install_text(engine: &mut Engine, text: &str) -> Result<Installed, String> {
    let doc = SceneDoc::from_json(text)
        .map_err(|e| e.to_string())?
        .validated()
        .map_err(|e| e.to_string())?;
    install_doc(engine, doc)
}

/// 把场景 JSON 装进引擎，**并把这份文档记成底稿**（编辑器导出时以它为底）。
pub fn load_text_into_doc(
    engine: &mut Engine,
    doc: &mut Doc,
    text: &str,
) -> Result<(usize, String), String> {
    let parsed = SceneDoc::from_json(text)
        .map_err(|e| e.to_string())?
        .validated()
        .map_err(|e| e.to_string())?;
    let installed = install_doc(engine, parsed.clone())?;
    doc.base_doc = Some(parsed);
    doc.set_model_instances(installed.instances);
    Ok((installed.objects, installed.name))
}

/// 从文件装载，并记成底稿（桌面端：贴图相对路径以文件所在目录为基准）。
#[cfg(not(target_arch = "wasm32"))]
pub fn load_file_into_doc(
    engine: &mut Engine,
    doc: &mut Doc,
    path: &std::path::Path,
) -> Result<(usize, String), String> {
    let parsed = SceneDoc::from_file(path)
        .map_err(|e| e.to_string())?
        .validated()
        .map_err(|e| e.to_string())?;
    let installed = install_doc(engine, parsed.clone())?;
    doc.base_doc = Some(parsed);
    doc.set_model_instances(installed.instances);
    Ok((installed.objects, installed.name))
}

/// 从文件装载（桌面端：贴图相对路径以文件所在目录为基准）。
#[cfg(not(target_arch = "wasm32"))]
pub fn load_file_into_engine(
    engine: &mut Engine,
    path: &std::path::Path,
) -> Result<(usize, String), String> {
    let doc = SceneDoc::from_file(path)
        .map_err(|e| e.to_string())?
        .validated()
        .map_err(|e| e.to_string())?;
    let installed = install_doc(engine, doc)?;
    Ok((installed.objects, installed.name))
}

/// 把文档装配进引擎。映射与文档一起出来，避免调用方再猜一遍。
fn install_doc(engine: &mut Engine, doc: SceneDoc) -> Result<Installed, String> {
    // 物理世界与可视场景都换掉：先清干净，再按文档重建。
    let build = runner::build_world(&doc).map_err(|e| e.to_string())?;
    engine.scene.clear();
    engine.world = build.world;
    let instances =
        runner::setup_visuals(engine, &doc, &build.handles).map_err(|e| e.to_string())?;
    apply_camera(engine, &doc.camera);
    let name = doc.name.clone().unwrap_or_else(|| "scene".to_string());
    Ok(Installed {
        objects: engine.scene.len(),
        name: format!("{name}.json"),
        instances,
    })
}

/// 把文档里的相机装进轨道相机。
///
/// 只认两种写法：给了 `position` + `look_at` 就反解出距离与角度；
/// 只给了 `yaw/pitch/distance` 就直接用。这样 CLI 写的场景打开后视角一致。
fn apply_camera(engine: &mut Engine, camera: &CameraDoc) {
    if let Some(target) = camera.look_at {
        engine.orbit.focus = Vec3::new(target[0], target[1], target[2]);
    }
    if let Some(position) = camera.position {
        let eye = Vec3::new(position[0], position[1], position[2]);
        let offset = eye - engine.orbit.focus;
        let distance = offset.length();
        if distance > 1e-4 {
            let dir = offset / distance;
            // 与 `OrbitCamera::direction` 的约定一致：dir = (cp·sy, sp, cp·cy)
            engine.orbit.yaw = dir.x.atan2(dir.z);
            engine.orbit.pitch = dir.y.clamp(-1.0, 1.0).asin();
        }
        if distance > 1e-4 {
            engine.orbit.distance = distance;
        }
    } else {
        if let Some(yaw) = camera.yaw_deg {
            engine.orbit.yaw = yaw.to_radians();
        }
        if let Some(pitch) = camera.pitch_deg {
            engine.orbit.pitch = pitch.to_radians();
        }
        if let Some(distance) = camera.distance {
            engine.orbit.distance = distance.max(0.05);
        }
    }
    if let Some(fov) = camera.fov_deg {
        engine.orbit.fov_y = fov.to_radians();
    }
    if let Some(near) = camera.near {
        engine.orbit.near = near;
    }
    if let Some(far) = camera.far {
        engine.orbit.far = far;
    }
    engine.orbit.orthographic_height = camera.orthographic_height.filter(|h| *h > 0.0);
}

/// 物理形状 → 场景文档形状（`size` 是**边长**，不是半长）。
fn shape_doc_from_shape(shape: &Shape, _visual_scale: Vec3) -> Option<ShapeDoc> {
    Some(match shape {
        Shape::Box { half_extents } => ShapeDoc {
            kind: ShapeKind::Box,
            size: Some([
                half_extents.x * 2.0,
                half_extents.y * 2.0,
                half_extents.z * 2.0,
            ]),
            ..Default::default()
        },
        Shape::Sphere { radius } => ShapeDoc {
            kind: ShapeKind::Sphere,
            radius: Some(*radius),
            ..Default::default()
        },
        Shape::Capsule {
            half_height,
            radius,
        } => ShapeDoc {
            kind: ShapeKind::Capsule,
            half_height: Some(*half_height),
            radius: Some(*radius),
            ..Default::default()
        },
        Shape::Cylinder {
            half_height,
            radius,
        } => ShapeDoc {
            kind: ShapeKind::Cylinder,
            half_height: Some(*half_height),
            radius: Some(*radius),
            ..Default::default()
        },
        Shape::Plane { normal } => ShapeDoc {
            kind: ShapeKind::Plane,
            normal: Some([normal.x, normal.y, normal.z]),
            ..Default::default()
        },
        Shape::ConvexHull { points } => ShapeDoc {
            kind: ShapeKind::Convex,
            points: Some(points.iter().map(|p| [p.x, p.y, p.z]).collect()),
            ..Default::default()
        },
        // 三角网碰撞体在场景格式里没有对应写法（格式只表达图元与凸包）：
        // 返回 None，让调用方把物体名报给用户，而不是悄悄丢一个。
        Shape::TriMesh { .. } => return None,
    })
}

/// 视觉网格的**表面**相对物体原点的抬高量（沿 +Y）。
///
/// 单位立方体/球/圆柱/胶囊都是中心在原点，所以表面在原点上方半个高度处；
/// `unit_plane` 本身就是一张平面，表面就在原点。地面高度要用它，
/// 否则「薄板当地面」的场景存读一轮就会下沉半个板厚。
fn surface_offset(engine: &Engine, object: &SceneObject) -> f32 {
    let assets = &engine.assets;
    // 物体一旋转，"沿 +Y 抬高半高"就不是表面高度了；这时老实返回 0，
    // 不去猜（猜错比不猜更糟：地面会莫名其妙地漂）。
    let rotation = object.transform.rotation;
    let tilted = (rotation.x * rotation.x + rotation.z * rotation.z).sqrt() > 1e-3;
    if tilted {
        return 0.0;
    }
    let scale_y = object.transform.scale.y.abs();
    let centered = object.mesh == assets.unit_cube
        || object.mesh == assets.unit_sphere
        || object.mesh == assets.unit_cylinder
        || object.mesh == assets.unit_capsule;
    if centered {
        0.5 * scale_y
    } else {
        0.0
    }
}

/// 没有刚体的物体：按预设网格反推形状，推不出来就返回 `None`（导出时报告跳过）。
fn shape_doc_from_mesh(engine: &Engine, object: &SceneObject) -> Option<ShapeDoc> {
    let assets = &engine.assets;
    let scale = object.transform.scale;
    if object.mesh == assets.unit_cube {
        Some(ShapeDoc {
            kind: ShapeKind::Box,
            size: Some([scale.x.abs(), scale.y.abs(), scale.z.abs()]),
            ..Default::default()
        })
    } else if object.mesh == assets.unit_sphere {
        Some(ShapeDoc {
            kind: ShapeKind::Sphere,
            radius: Some(0.5 * scale.x.abs()),
            ..Default::default()
        })
    } else if object.mesh == assets.unit_cylinder {
        Some(ShapeDoc {
            kind: ShapeKind::Cylinder,
            half_height: Some(0.5 * scale.y.abs()),
            radius: Some(0.5 * scale.x.abs()),
            ..Default::default()
        })
    } else if object.mesh == assets.unit_capsule {
        Some(ShapeDoc {
            kind: ShapeKind::Capsule,
            half_height: Some((0.5 * scale.y.abs()) - 0.5 * scale.x.abs()),
            radius: Some(0.5 * scale.x.abs()),
            ..Default::default()
        })
    } else if object.mesh == assets.unit_plane {
        Some(ShapeDoc {
            kind: ShapeKind::Plane,
            normal: Some([0.0, 1.0, 0.0]),
            ..Default::default()
        })
    } else {
        None
    }
}

/// 四元数 → `Rx·Ry·Rz` 顺序的欧拉角（度），与场景格式的 `rotation_deg` 对齐。
///
/// 数学库里只有 `to_euler_yxz`（`Ry·Rx·Rz`），顺序不同不能互相顶替：
/// 单轴旋转两者一致，复合旋转会差出十几度。
pub(crate) fn to_euler_xyz_deg(q: Quat) -> [f32; 3] {
    let m = q.to_mat3();
    // 列主序：`cols[c]` 的第 r 个分量是矩阵第 r 行第 c 列。
    let m00 = m.cols[0].x;
    let m01 = m.cols[1].x;
    let m02 = m.cols[2].x;
    let m12 = m.cols[2].y;
    let m22 = m.cols[2].z;
    let sy = m02.clamp(-1.0, 1.0);
    let y = sy.asin();
    let (x, z) = if sy.abs() < 0.999_999 {
        ((-m12).atan2(m22), (-m01).atan2(m00))
    } else {
        // 万向锁：把自由度全给 X，Z 记 0。
        ((-m.cols[2].x).atan2(m00), 0.0)
    };
    [x.to_degrees(), y.to_degrees(), z.to_degrees()]
}

/// Web 端：请求浏览器打开文件选择框（真正的读取在 [`on_scene_file`]）。
#[cfg(target_arch = "wasm32")]
pub fn pick_file_and_load(suggested_name: String) {
    use wasm_bindgen::{JsCast, JsValue};
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(pick) =
        js_sys::Reflect::get(&window, &JsValue::from_str("__quench3d_pick_scene_file"))
            .ok()
            .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
    else {
        log::warn!("页面没有 __quench3d_pick_scene_file（index.html 需要提供它）");
        return;
    };
    let _ = pick.call1(&JsValue::NULL, &JsValue::from_str(&suggested_name));
}

/// Web 端：浏览器把读好的文件交回来（由页面脚本调用；入口 example 再转发一次）。
#[cfg(target_arch = "wasm32")]
pub fn on_scene_file(name: String, text: String) {
    PENDING.with(|slot| {
        *slot.borrow_mut() = Some((name, text));
    });
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static PENDING: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) };
}

/// 取走浏览器刚交回来的场景文件（每帧查一次）。
#[cfg(target_arch = "wasm32")]
pub fn take_pending_upload() -> Option<(String, String)> {
    PENDING.with(|slot| slot.borrow_mut().take())
}

/// Web 端：把文本当文件下载。
#[cfg(target_arch = "wasm32")]
pub fn download_text(file_name: &str, text: &str) -> Result<(), String> {
    use wasm_bindgen::JsCast;
    let window = web_sys::window().ok_or("没有 window")?;
    let document = window.document().ok_or("没有 document")?;
    let array = js_sys::Array::new();
    array.push(&wasm_bindgen::JsValue::from_str(text));
    let blob = web_sys::Blob::new_with_str_sequence(&array).map_err(|e| format!("{e:?}"))?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(|e| format!("{e:?}"))?;
    let anchor = document
        .create_element("a")
        .map_err(|e| format!("{e:?}"))?
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .map_err(|_| "创建 a 元素失败")?;
    anchor.set_href(&url);
    anchor.set_download(file_name);
    let body = document.body().ok_or("没有 body")?;
    body.append_child(&anchor).map_err(|e| format!("{e:?}"))?;
    anchor.click();
    body.remove_child(&anchor).map_err(|e| format!("{e:?}"))?;
    web_sys::Url::revoke_object_url(&url).map_err(|e| format!("{e:?}"))?;
    Ok(())
}

/// 给 Web 探针用：场景 JSON 在 wasm 里也能解析并建成物理世界。
///
/// 不带 GPU（`build_world` 是纯 CPU 路径），所以浏览器里可以随时调。
/// 导出方向（引擎 → JSON）需要 GPU 才能建引擎，放在原生测试里覆盖。
pub fn roundtrip_probe() -> String {
    const SRC: &str = r#"{
        "ground": {"size": 10},
        "objects": [
            {"name": "球", "shape": {"type": "sphere", "radius": 0.5},
             "position": [0, 4, 0], "body": "dynamic"}
        ]
    }"#;
    let doc = match SceneDoc::from_json(SRC) {
        Ok(doc) => doc,
        Err(e) => return format!("err 解析: {e}"),
    };
    if SceneDoc::has_errors(&doc.validate()) {
        return "err 校验未通过".to_string();
    }
    match runner::build_world(&doc) {
        Ok(build) => format!(
            "ok bodies={} objects={} steps=0",
            build.world.body_count(),
            doc.objects.len()
        ),
        Err(e) => format!("err 建世界: {e}"),
    }
}
