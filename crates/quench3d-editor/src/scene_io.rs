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
    WaterStyleDoc, FORMAT, VERSION,
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

/// 导出用的**底稿**：有 `include` 时先合并成一份自包含文档，并把 `include` 清掉。
///
/// 为什么必须在导出前做这件事：编辑器编辑的是**合并后**的世界（引擎里就是合并后的物体），
/// 存盘时 `objects` 会按引擎重建。于是两种错法都不可接受 ——
///
/// * 底稿**留着** `include`：下次载入把同一批物体再并一遍（静默的数据繁殖）；
/// * 只清 `include` **不合并**：片段里的 `models` / `joints` / `actions` / 材质
///   会在存盘时被悄悄丢掉（它们不在根文件的这些段里）。
///
/// 所以"合并 + 清 `include`"是唯一正确的导出方式。代价写在这里、也说给用户听：
/// 存盘会把片段**内联**进根文件（片段文件本身不动），`notes` 里会说明这一次内联了什么。
///
/// 纯 CPU：不碰 GPU，也不需要真窗口 —— 编辑器侧的回归测试因此能在 CI 上真跑。
pub fn export_base_doc(base: &SceneDoc, notes: &mut Vec<String>) -> Result<SceneDoc, String> {
    if base.include.is_empty() {
        return Ok(base.clone());
    }
    let includes = base.include.clone();
    let resolved = base.resolved();
    let Some(merged) = resolved.doc else {
        return Err(format!(
            "include 解析失败，拒绝存盘（硬存会把片段内容丢掉）：{}",
            resolved.error_message()
        ));
    };
    notes.push(format!(
        "已把 include 的片段内联进这份文件（编辑器编辑的是合并后的场景，片段文件本身没动）：{}",
        includes.join("、")
    ));
    Ok(merged)
}

/// glTF 导出结果（单文件 `.gltf`，几何以 base64 内嵌）。
pub struct SavedGltf {
    /// `.gltf` JSON 文本。
    pub json: String,
    /// 写出的物体数。
    pub objects: usize,
    /// 三角形数。
    pub triangles: usize,
    /// 导出的动画条数。
    pub animations: usize,
    /// 没法带进 glTF 的说明（缺贴图、导入模型、凸包…）。
    pub skipped: Vec<String>,
}

/// GLB 导出结果（单文件二进制，JSON + BIN chunk）。
pub struct SavedGlb {
    /// 二进制 GLB 字节。
    pub bytes: Vec<u8>,
    /// 写出的物体数。
    pub objects: usize,
    /// 三角形数。
    pub triangles: usize,
    /// 导出的动画条数。
    pub animations: usize,
    /// 没法带进 GLB 的说明。
    pub skipped: Vec<String>,
}

/// 把引擎当前场景导出成二进制 GLB。
///
/// 复用 [`scene_to_gltf`] 的路径（场景 JSON → CLI 导出器 → 合法 `.gltf` JSON），
/// 再走 `glb_from_json` 把 base64 buffer 换成 BIN chunk；因此编辑器的 `.gltf` 与
/// `.glb` 两条出口在场景语义上完全一致。
pub fn scene_to_glb(engine: &Engine, doc: &Doc) -> Result<SavedGlb, String> {
    let saved = scene_to_gltf(engine, doc)?;
    let bytes = quench3d_cli::gltf_export::glb_from_json(&saved.json)?;
    Ok(SavedGlb {
        bytes,
        objects: saved.objects,
        triangles: saved.triangles,
        animations: saved.animations,
        skipped: saved.skipped,
    })
}

/// 把引擎当前场景导出成单文件 glTF 2.0。
///
/// 先走 [`scene_to_json`]（它负责把编辑器懂的部分写回底稿、把场景格式表达不了的物体
/// 记进 `skipped`），再把这份 JSON 解析回 `SceneDoc` 交给 CLI 的 glTF 导出器。这样
/// 编辑器/CLI 两条导出路径看到的是同一份场景语义，不会各写一套。
pub fn scene_to_gltf(engine: &Engine, doc: &Doc) -> Result<SavedGltf, String> {
    let saved = scene_to_json(engine, doc)?;
    let scene = SceneDoc::from_json(&saved.json)
        .map_err(|e| format!("导出的场景 JSON 不能重新解析：{e:?}"))?;
    // 贴图相对场景文件解析；没保存过的新场景以当前目录为基准。
    let base = std::path::Path::new(&doc.file_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let out = quench3d_cli::gltf_export::export_gltf_with_base(&scene, Some(base))?;
    let mut skipped = saved.skipped;
    skipped.extend(out.skipped);
    Ok(SavedGltf {
        json: out.json,
        objects: out.objects,
        triangles: out.triangles,
        animations: out.animations,
        skipped,
    })
}

/// 把引擎当前场景导出成 `quench3d.scene` JSON。
pub fn scene_to_json(engine: &Engine, doc: &Doc) -> Result<SavedScene, String> {
    // 没法用场景格式表达的物体（导入的模型、凸包…）与"这次存盘做了什么"都记在这里。
    let mut skipped = Vec::new();
    // **以最近载入的那份文档为底稿**：编辑器只"懂"物体、地面、材质标量与相机，
    // 而 `lighting` / `simulate` / `render` / `probes` / `models` 这些段它既不改也不该丢。
    // 曾经这里是 `SceneDoc::default()` 起头 —— 后果是「CLI 生成（带光照与渲染设置）
    // → 编辑器改一改 → 存盘」，光照、渲染设置、模型引用被**静默清零**。
    // `include` 的合并也在这一步（见 [`export_base_doc`]）。
    let loaded = doc.base_doc.clone().unwrap_or_default();
    let base = export_base_doc(&loaded, &mut skipped)?;
    let mut out = base.clone();
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
        near: Some(engine.orbit.near),
        far: Some(engine.orbit.far),
        orthographic_height: engine.orbit.orthographic_height,
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
    // 编辑器能改的雾 / 阴影质量 / 天空颜色也从引擎写回；不能改的（点光、bloom…）沿用底稿。
    lighting.fog = Some(quench3d_cli::scene_doc::FogDoc {
        color: Some([light.fog.color.x, light.fog.color.y, light.fog.color.z]),
        density: Some(light.fog.density),
    });
    // 级联数与切分系数现在由编辑器自己管（UI 直接改 `engine.renderer.options`，载入时
    // 也把场景里的值应用进去），所以从**引擎**写回而不是"沿用底稿"。
    // 关键不变式：载入 `cascades: 4` → 随手存一下仍然是 4（往返测试在 CLI 的 runner 里：
    // `shadow_quality_survives_the_round_trip_through_renderer_options`）。
    lighting.shadow_quality = Some(quench3d_cli::scene_doc::ShadowQualityDoc {
        strength: Some(light.shadow_strength),
        bias: light.sun.as_ref().map(|s| s.shadow_bias),
        radius: light.sun.as_ref().map(|s| s.shadow_radius),
        cascades: Some(engine.renderer.options.shadow_cascades),
        split_lambda: Some(engine.renderer.options.shadow_split_lambda),
    });
    // 局部光（点光源 / 聚光灯）：编辑器现在能增删改（见 ui 的 local_lights_section），
    // 所以从**引擎**写回。空数组写成"不写这一节"：对点光源来说"没写"（保留预设补光）
    // 与"写了 []"（显式清空）是两种意思，UI 里删光几盏灯不该顺手把预设补光也清掉。
    lighting.point_lights = point_light_docs(&light.point_lights);
    lighting.spot_lights = spot_light_docs(&light.spot_lights);
    // 自动 LOD 开关同上：写回 `render.auto_lod`，否则重开场景会退回缺省 false。
    out.render.auto_lod = Some(engine.renderer.options.auto_lod);
    // 后处理：编辑器现在能改（UI 直接改 `engine.renderer.options`），所以从引擎写回。
    // 以前这三项是"沿用底稿"——在编辑器里勾掉 FXAA，存盘却还是开着的。
    let opts = &engine.renderer.options;
    lighting.bloom = Some(quench3d_cli::scene_doc::BloomDoc {
        enabled: Some(opts.bloom.enabled),
        intensity: Some(opts.bloom.intensity),
        threshold: Some(opts.bloom.threshold),
    });
    lighting.fxaa = Some(quench3d_cli::scene_doc::FxaaDoc {
        enabled: Some(opts.fxaa.enabled),
        edge_threshold: Some(opts.fxaa.edge_threshold),
        edge_threshold_min: Some(opts.fxaa.edge_threshold_min),
    });
    lighting.taa = Some(quench3d_cli::scene_doc::TaaDoc {
        enabled: Some(opts.taa.enabled),
        feedback: Some(opts.taa.feedback),
    });
    lighting.ssao = Some(quench3d_cli::scene_doc::SsaoDoc {
        enabled: Some(opts.ssao.enabled),
        radius: Some(opts.ssao.radius),
        bias: Some(opts.ssao.bias),
        strength: Some(opts.ssao.strength),
        samples: Some(opts.ssao.samples),
        denoise: Some(opts.ssao.denoise),
    });
    lighting.dof = Some(quench3d_cli::scene_doc::DofDoc {
        enabled: Some(opts.dof.enabled),
        focus: Some(opts.dof.focus),
        aperture: Some(opts.dof.aperture),
        max_blur: Some(opts.dof.max_blur),
    });
    lighting.sky_colors = Some(quench3d_cli::scene_doc::SkyColorsDoc {
        zenith: Some([light.sky.zenith.x, light.sky.zenith.y, light.sky.zenith.z]),
        horizon: Some([
            light.sky.horizon.x,
            light.sky.horizon.y,
            light.sky.horizon.z,
        ]),
        ground: Some([light.sky.ground.x, light.sky.ground.y, light.sky.ground.z]),
    });
    out.lighting = lighting;

    // 屏幕空间水：编辑器没有独立的水域数据，但**风格**是引擎渲染选项的一部分，
    // 所以从 `renderer.options.water` 写回。这样在编辑器里拖过颜色/波浪后存盘，
    // `quench3d render` 拿到的就是同一份参数（否则改了只在当前预览有效）。
    //
    // 没有水、也没改过风格时不写这一节：否则"打开一个白模 → 存盘"会凭空多出
    // 五项默认值（虽然不改变画面，但属于无意义的数据繁殖）。
    let water = &engine.renderer.options.water;
    let defaults = WaterOptions::default();
    let style_changed = water.color != defaults.color
        || water.clarity != defaults.clarity
        || water.strength != defaults.strength
        || water.foam != defaults.foam
        || water.refraction != defaults.refraction;
    if out.water.is_some() || !out.waters.is_empty() || !out.fluids.is_empty() || style_changed {
        out.water = Some(WaterStyleDoc {
            color: Some(water.color),
            clarity: Some(water.clarity),
            wave_strength: Some(water.strength),
            foam: Some(water.foam),
            refraction: Some(water.refraction),
        });
    }

    // 场景物体下标 → 导出后 `objects` 里的下标（父子关系要靠它对上）。
    let mut exported: Vec<(usize, usize)> = Vec::new();
    for (index, object) in engine.scene.objects.iter().enumerate() {
        // 复合形状的附加部件是"绘制用"的伪物体：它们归主物体，不单独导出
        // （导出去会让场景每存一次就多出几个物体——静默的数据繁殖）。
        if object.owner_part.is_some() {
            continue;
        }
        // 粒子/水面是渲染代理，不是场景内容：存回去会变成几千个球体物体。
        if object.fluid_particle.is_some()
            || object.fluid_surface.is_some()
            || engine.scene.is_particle_object(index)
        {
            continue;
        }
        let name = if object.name.trim().is_empty() {
            format!("物体 {}", index + 1)
        } else {
            object.name.clone()
        };
        let body = object.body.and_then(|h| engine.world.body(h));
        // 形状：有刚体就用物理形状（权威），没有就从预设网格反推。
        let mut parts_doc: Option<Vec<quench3d_cli::scene_doc::ShapePartDoc>> = None;
        let shape = match body {
            Some(b) if !b.extra_parts().is_empty() => {
                // 复合形状：逐部件导出（局部位姿原样写回去）
                let mut list = Vec::new();
                let mut bad = false;
                for (part_shape, local) in b.parts() {
                    match shape_doc_from_shape(part_shape, object.transform.scale) {
                        Some(doc) => list.push(quench3d_cli::scene_doc::ShapePartDoc {
                            shape: doc,
                            position: Some([local.position.x, local.position.y, local.position.z]),
                            rotation_deg: Some(to_euler_xyz_deg(local.rotation)),
                        }),
                        None => {
                            skipped.push(format!("{name}（三角网部件）"));
                            bad = true;
                            break;
                        }
                    }
                }
                if bad {
                    continue;
                }
                parts_doc = Some(list);
                match shape_doc_from_shape(&b.shape, object.transform.scale) {
                    Some(shape) => shape,
                    None => {
                        skipped.push(format!("{name}（三角网碰撞体）"));
                        continue;
                    }
                }
            }
            Some(b) => match shape_doc_from_shape(&b.shape, object.transform.scale)
                .or_else(|| base_shape_for(&base, &name))
            {
                Some(shape) => shape,
                None => {
                    skipped.push(format!("{name}（三角网碰撞体）"));
                    continue;
                }
            },
            None => {
                match shape_doc_from_mesh(engine, object).or_else(|| base_shape_for(&base, &name)) {
                    Some(shape) => shape,
                    None => {
                        skipped.push(format!("{name}（不是预设网格）"));
                        continue;
                    }
                }
            }
        };
        // 地面：平面 + 静态 → 写成 `ground`（场景格式里地面是专门字段）。
        //
        // 高度取**地面表面**而不是物体原点：物理上的半空间就在原点平面上，
        // 而视觉上多半放了一块"薄板"（把单位立方体压扁、中心压在 -半厚处）。
        // 直接写原点的话，存一次再读一次地面会掉下去半个板厚。
        if parts_doc.is_none() && matches!(shape.kind, ShapeKind::Plane) {
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
        //
        // 查的是**合并后的底稿**（`base`）：片段里的物体/材质也要能被继承到，
        // 否则「载入带 include + 贴图的场景 → 随手存一次」会把片段物体的贴图清零。
        let inherited = base
            .objects
            .iter()
            .find(|o| o.name.as_deref() == Some(name.as_str()))
            .and_then(|source| base.materials.get(source.material.as_deref()?).cloned());
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
        let (shape_field, shapes_field) = match parts_doc {
            Some(list) => (None, Some(list)),
            None => (Some(shape), None),
        };
        out.objects.push(ObjectDoc {
            name: Some(name),
            // 编辑器不做模板/实例的界面：装载时已经展开成具体物体，导出就是具体物体
            // （行为一致，只是文件变长——测试 `templates_and_instances_are_flattened_on_save` 钉住）
            template: None,
            instances: None,
            shape: shape_field,
            shapes: shapes_field,
            parent: None, // 循环之后再补：那时才知道父物体有没有被导出
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
            static_friction: body.map(|b| b.static_friction),
            // 合成规则：编辑器能改（见光照面板下面的物理段），导出时从刚体读回来，
            // 这样"载入带规则的场景 → 存盘"不会把规则丢掉。
            friction_combine: body
                .and_then(|b| b.friction_combine)
                .map(quench3d_cli::scene_doc::CombineRuleDoc::from),
            restitution_combine: body
                .and_then(|b| b.restitution_combine)
                .map(quench3d_cli::scene_doc::CombineRuleDoc::from),
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
            // 碰撞过滤 / 传感器 / CCD：编辑器目前**没有编辑界面**，但导出时从刚体上读回来，
            // 这样"载入场景 → 保存"不会把这些字段悄悄丢掉。
            collision_layer: body.and_then(|b| {
                // 格式里一层就是一个位；取最低位（编辑器里不会出现多位成员的情况）。
                // 第 0 层是缺省值，省略不写，免得给每个物体都加一个 "collision_layer": 0。
                let m = b.groups.memberships;
                let layer = if m == 0 { 0 } else { m.trailing_zeros() as u8 };
                if layer == 0 {
                    None
                } else {
                    Some(layer)
                }
            }),
            collides_with: body.and_then(|b| {
                let f = b.groups.filter;
                if f == quench3d::physics::InteractionGroups::ALL {
                    None
                } else {
                    Some((0..32u8).filter(|i| f & (1u32 << i) != 0).collect())
                }
            }),
            sensor: body.map(|b| b.sensor).filter(|s| *s),
            ccd: body.map(|b| b.ccd).filter(|c| *c),
        });
        exported.push((index, out.objects.len() - 1));
    }

    // 父子关系：**循环之后**再补，因为"父物体有没有被导出"要看形状反推的结果 ——
    // 地面写进了 `ground`、模型子网格根本不写成物体，这时父物体名在文件里不存在，
    // 硬写出去就是一个悬空引用（整个文件会读不回来）。所以：丢掉这条关系，并上报。
    let scene_to_exported: std::collections::BTreeMap<usize, usize> =
        exported.iter().copied().collect();
    let mut dropped_parents = 0usize;
    for (scene_index, out_index) in &exported {
        let Some(parent) = engine.scene.objects[*scene_index].parent else {
            continue;
        };
        match scene_to_exported.get(&parent) {
            Some(&pi) => {
                let parent_name = out.objects[pi].name.clone();
                out.objects[*out_index].parent = parent_name;
            }
            None => dropped_parents += 1,
        }
    }
    if dropped_parents > 0 {
        skipped.push(format!(
            "{dropped_parents} 条父子关系（父物体没被写成物体：地面或模型展开出的网格）"
        ));
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

#[cfg(test)]
mod local_light_doc_tests {
    use super::*;

    #[test]
    fn local_lights_round_trip_through_the_scene_doc() {
        // 编辑器写回走这两个纯函数（不需要 GPU，所以 CI 也跑）。
        // 钉住三件事：空 = 不写这一节；字段一个不丢；角度"角度→余弦→角度"能回到原值。
        assert!(point_light_docs(&[]).is_none());
        assert!(spot_light_docs(&[]).is_none());

        let point = quench3d::render::PointLight {
            position: Vec3::new(1.0, 2.0, 3.0),
            color: Vec3::new(0.9, 0.8, 0.7),
            intensity: 12.5,
            radius: 20.0,
            cast_shadows: false,
        };
        let docs = point_light_docs(std::slice::from_ref(&point)).expect("有点光就该写");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].position, [1.0, 2.0, 3.0]);
        assert_eq!(docs[0].color, [0.9, 0.8, 0.7]);
        assert_eq!(docs[0].intensity, 12.5);
        assert_eq!(docs[0].radius, 20.0);
        assert_eq!(
            docs[0].cast_shadows, None,
            "没勾投影不许写这个字段（老场景存盘后要逐字节相同）"
        );

        let spot = quench3d::render::SpotLight::new(
            Vec3::new(0.0, 9.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.5),
            30.0,
            40.0,
            8.0,
            12.0,
        );
        let docs = spot_light_docs(std::slice::from_ref(&spot)).expect("有聚光就该写");
        assert_eq!(docs.len(), 1);
        let d = &docs[0];
        assert_eq!(d.position, [0.0, 9.0, 0.0]);
        assert_eq!(d.direction, [0.0, -1.0, 0.0]);
        assert_eq!(d.color, [1.0, 1.0, 0.5]);
        assert_eq!(d.intensity, 30.0);
        assert_eq!(d.radius, 40.0);
        // 引擎只存余弦，所以这里比的是"转回角度"的值（1e-4 度以内足够）
        assert!((d.inner_deg - 8.0).abs() < 1e-4, "内锥角 {}", d.inner_deg);
        assert!((d.outer_deg - 12.0).abs() < 1e-4, "外锥角 {}", d.outer_deg);
        // 方向会被归一化（作者写 (0,-2,0) 这种也认）
        let long = quench3d::render::SpotLight::new(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, -2.0, 0.0),
            Vec3::new(1.0, 1.0, 1.0),
            1.0,
            1.0,
            10.0,
            20.0,
        );
        let docs = spot_light_docs(std::slice::from_ref(&long)).unwrap();
        assert!((docs[0].direction[1] + 1.0).abs() < 1e-6);
        assert_eq!(
            docs[0].cast_shadows, None,
            "没勾投影不许写这个字段（老场景存盘后要逐字节相同）"
        );
    }

    /// 投影开关必须能原样往返：它是"预览 == 渲染"的一部分，
    /// 丢了它编辑器里勾的框存盘就没了（而 CLI 渲出来当然也没有影子）。
    #[test]
    fn spot_shadow_switch_survives_the_round_trip() {
        let base = quench3d::render::SpotLight::new(
            Vec3::new(0.0, 9.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 1.0),
            30.0,
            40.0,
            15.0,
            25.0,
        );
        let caster = base.with_cast_shadows(true);
        let docs = spot_light_docs(&[base, caster]).expect("有聚光就该写");
        assert_eq!(docs[0].cast_shadows, None);
        assert_eq!(docs[1].cast_shadows, Some(true));

        // 写进场景 JSON → 读回来，两步都不能丢（走 CLI 的同一份序列化，不是自己拼字符串）
        let mut doc = quench3d_cli::scene_doc::SceneDoc::default();
        doc.lighting.spot_lights = Some(docs);
        let text = doc.to_json_pretty();
        assert!(
            text.contains("\"cast_shadows\": true"),
            "勾了投影的场景存盘文本里必须有它: {text}"
        );
        assert_eq!(
            text.matches("cast_shadows").count(),
            1,
            "没勾的那盏不许写出这个字段: {text}"
        );
        let back = quench3d_cli::scene_doc::SceneDoc::from_json(&text).unwrap();
        let spots = back.lighting.spot_lights.expect("聚光还在");
        assert_eq!(spots[0].cast_shadows, None);
        assert_eq!(spots[1].cast_shadows, Some(true));
    }

    /// 点光源的投影开关同样必须能原样往返（它决定了 CLI 渲出来的画面有没有影子）。
    #[test]
    fn point_shadow_switch_survives_the_round_trip() {
        let base = quench3d::render::PointLight::new(
            Vec3::new(0.0, 3.0, 0.0),
            Vec3::new(1.0, 1.0, 1.0),
            60.0,
            30.0,
        );
        let caster = base.with_cast_shadows(true);
        let docs = point_light_docs(&[base, caster]).expect("有点光就该写");
        assert_eq!(docs[0].cast_shadows, None);
        assert_eq!(docs[1].cast_shadows, Some(true));

        // 写进场景 JSON → 读回来，两步都不能丢（走 CLI 的同一份序列化）
        let mut doc = quench3d_cli::scene_doc::SceneDoc::default();
        doc.lighting.point_lights = Some(docs);
        let text = doc.to_json_pretty();
        assert!(
            text.contains("\"cast_shadows\": true"),
            "勾了投影的场景存盘文本里必须有它: {text}"
        );
        assert_eq!(
            text.matches("cast_shadows").count(),
            1,
            "没勾的那盏不许写出这个字段: {text}"
        );
        let back = quench3d_cli::scene_doc::SceneDoc::from_json(&text).unwrap();
        let points = back.lighting.point_lights.expect("点光还在");
        assert_eq!(points[0].cast_shadows, None);
        assert_eq!(points[1].cast_shadows, Some(true));
    }
}

/// 引擎里的点光源 → 场景文档。空 = `None`（不写这一节）。
///
/// `cast_shadows` 同样只在**为真**时写进文档：不写就是 `false`（老行为），
/// 这样"编辑器里没勾过投影"的场景存盘后与打开前逐字节相同。
pub(crate) fn point_light_docs(
    lights: &[quench3d::render::PointLight],
) -> Option<Vec<quench3d_cli::scene_doc::PointLightDoc>> {
    if lights.is_empty() {
        return None;
    }
    Some(
        lights
            .iter()
            .map(|p| quench3d_cli::scene_doc::PointLightDoc {
                position: p.position.to_array(),
                color: p.color.to_array(),
                intensity: p.intensity,
                radius: p.radius,
                cast_shadows: if p.cast_shadows { Some(true) } else { None },
            })
            .collect(),
    )
}

/// 引擎里的聚光灯 → 场景文档。空 = `None`（不写这一节）。
///
/// 角度是**从余弦反解**出来的（引擎只存余弦），所以存回去的是等价角度，
/// 与作者写下的原始小数可能有 1e-5 量级的差别 —— 语义完全一样。
///
/// `cast_shadows` 只在**为真**时写进文档：不写就是 `false`（老行为），
/// 这样"编辑器里没勾过投影"的场景存盘后与打开前逐字节相同。
pub(crate) fn spot_light_docs(
    lights: &[quench3d::render::SpotLight],
) -> Option<Vec<quench3d_cli::scene_doc::SpotLightDoc>> {
    if lights.is_empty() {
        return None;
    }
    Some(
        lights
            .iter()
            .map(|s| quench3d_cli::scene_doc::SpotLightDoc {
                position: s.position.to_array(),
                direction: s.dir().to_array(),
                color: s.color.to_array(),
                intensity: s.intensity,
                radius: s.radius,
                inner_deg: s.inner_cos.acos().to_degrees(),
                outer_deg: s.outer_cos.acos().to_degrees(),
                cast_shadows: if s.cast_shadows { Some(true) } else { None },
            })
            .collect(),
    )
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
    doc.selected_camera = None;
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
    doc.selected_camera = None;
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
    // 旧场景的网格/材质/贴图不能跟着句柄空间一起留着：否则每次"打开"都会把上一份
    // 场景的显存再占一遍，而新场景的句柄从 0 开始，旧批次还可能引用同号资源。
    // 先清渲染器资源，再用空的 AssetCache 重建内置图元。
    engine.renderer.clear_scene_assets();
    // 自动 LOD 必须在上传模型**之前**定下来：LOD 是 `add_mesh*` 时生成的。
    engine.renderer.options.auto_lod = doc.render.auto_lod.unwrap_or(false);
    engine.assets = quench3d::AssetCache::new(&mut engine.renderer, &engine.gpu);
    engine.world = build.world;
    let instances =
        runner::setup_visuals(engine, &doc, &build.handles).map_err(|e| e.to_string())?;
    apply_camera(engine, &doc.camera);
    // 阴影级联：用 CLI 的同一份映射函数，保证编辑器预览与 `quench3d render` 是同一种阴影。
    // （以前这里什么也没设：载入一个 `cascades: 4` 的场景，编辑器预览仍是引擎默认的单级。）
    engine.renderer.options.shadow_cascades = runner::shadow_cascades(&doc);
    engine.renderer.options.shadow_split_lambda = runner::shadow_split_lambda(&doc);
    // 后处理三项也走同一份映射：以前编辑器**完全不应用**它们——载入一个开了 bloom 与 SSAO
    // 的场景，预览里这两样都不在，而 CLI 渲出来是有的。
    engine.renderer.options.bloom = runner::bloom_options(&doc);
    engine.renderer.options.fxaa = runner::fxaa_options(&doc);
    engine.renderer.options.taa = runner::taa_options(&doc);
    engine.renderer.options.ssao = runner::ssao_options(&doc);
    engine.renderer.options.dof = runner::dof_options(&doc);
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
pub(crate) fn apply_camera(engine: &mut Engine, camera: &CameraDoc) {
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

/// 底稿里同名物体的形状 —— 引擎里的形状**反推不出来**时的兜底。
///
/// 目前只有一种情况：**静态三角网**（高度场展开成的）。编辑器没有形状编辑面板
/// （只能加图元、摆位姿、删物体、改材质），所以"这个物体的形状没被改过"成立；
/// 没有这条兜底，`载入带地形的场景 → 随手存一次` 就会把整块地形丢掉。
///
/// 找不到同名物体（或它当时也没写 `shape`）就返回 `None`，调用方照旧报 skipped。
/// **部件级**的三角网不在兜底范围（那是 `shapes[]` 里的一个部件，按名字整体套用会串形状）。
pub fn base_shape_for(base: &SceneDoc, name: &str) -> Option<ShapeDoc> {
    base.objects
        .iter()
        .find(|o| o.name.as_deref() == Some(name))
        .and_then(|o| o.shape.clone())
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
        Shape::Cone {
            half_height,
            radius,
        } => ShapeDoc {
            kind: ShapeKind::Cone,
            half_height: Some(*half_height),
            radius: Some(*radius),
            ..Default::default()
        },
        Shape::RoundedBox {
            half_extents,
            radius,
        } => ShapeDoc {
            kind: ShapeKind::RoundedBox,
            size: Some([
                half_extents.x * 2.0,
                half_extents.y * 2.0,
                half_extents.z * 2.0,
            ]),
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
        || object.mesh == assets.unit_capsule
        || object.mesh == assets.unit_cone;
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
    } else if object.mesh == assets.unit_cone {
        Some(ShapeDoc {
            kind: ShapeKind::Cone,
            half_height: Some(0.5 * scale.y.abs()),
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
        // 万向锁（y = ±90°）：绕 X 与绕 Z 的自由度塌缩成一个，约定 Z = 0、角度全给 X。
        //
        // 这里必须用矩阵**第 2 行**（m21 / m20）来解：`cy = 0` 时 m12、m22、m01、m00 全成了 0，
        // 拿它们做 atan2 只会得到 ±90° 的符号，反解出来的旋转与原来**不是同一个**。
        // 曾经的 bug：父物体绕 Y 转 90°，存盘重开后变成绕 X 转 -90°，
        // 它自己看不出来（都是"转 90°"），但子物体会立刻跑到别的地方去。
        (m.cols[1].z.atan2(-m.cols[0].z * sy), 0.0)
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

/// Web 端：把字节当文件下载（GLB 等二进制）。
#[cfg(target_arch = "wasm32")]
pub fn download_bytes(file_name: &str, bytes: &[u8]) -> Result<(), String> {
    use wasm_bindgen::JsCast;
    let window = web_sys::window().ok_or("没有 window")?;
    let document = window.document().ok_or("没有 document")?;
    let array = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&array.buffer());
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).map_err(|e| format!("{e:?}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 引擎形状 → 场景文档形状：每种图元都要能**原样反推**（存盘不丢形状）。
    ///
    /// 这条只覆盖"反推得出"的那些（三角网走底稿兜底，见 `include_export.rs`）。
    /// 判据是往返：写进去的形状 → 引擎形状 → 反推回来还是同一份（`size` 是边长）。
    #[test]
    fn shape_export_round_trips_every_reversible_kind() {
        fn check(shape: &Shape, kind: ShapeKind, size: Option<[f32; 3]>, radius: Option<f32>) {
            let doc = shape_doc_from_shape(shape, Vec3::ONE).expect("该能反推");
            assert_eq!(doc.kind, kind, "{shape:?}");
            if let Some(size) = size {
                let got = doc.size.expect("该写 size");
                for (a, b) in got.iter().zip(size.iter()) {
                    assert!((a - b).abs() < 1e-6, "{shape:?}: {got:?} vs {size:?}");
                }
            }
            if let Some(radius) = radius {
                assert!(
                    (doc.radius.expect("该写 radius") - radius).abs() < 1e-6,
                    "{shape:?}"
                );
            }
        }
        check(
            &Shape::Box {
                half_extents: Vec3::new(0.5, 0.25, 0.75),
            },
            ShapeKind::Box,
            Some([1.0, 0.5, 1.5]),
            None,
        );
        check(
            &Shape::Sphere { radius: 0.4 },
            ShapeKind::Sphere,
            None,
            Some(0.4),
        );
        check(
            &Shape::Capsule {
                half_height: 0.6,
                radius: 0.2,
            },
            ShapeKind::Capsule,
            None,
            Some(0.2),
        );
        // 圆角盒：`size` 是边长（= 半长 × 2），`radius` 原样保留
        check(
            &Shape::RoundedBox {
                half_extents: Vec3::new(0.5, 0.3, 0.7),
                radius: 0.12,
            },
            ShapeKind::RoundedBox,
            Some([1.0, 0.6, 1.4]),
            Some(0.12),
        );
    }

    /// 欧拉角提取必须是**真的**反解：抽出来再塞回去要得到同一个旋转。
    ///
    /// 重点是万向锁（y = ±90°）：那里 X/Z 两个自由度塌缩成一个，用错矩阵行就会
    /// 反解成"另一个 90° 旋转"—— 单看这个物体(都是转 90°)看不出来，但它的子物体
    /// 会立刻跑到别处（见下面的 `hierarchy` 集成测试）。这类"只在特定角度出现"
    /// 的坑，只能靠穷举角度来钉。
    #[test]
    fn euler_extraction_round_trips_every_axis() {
        let mut cases: Vec<Quat> = Vec::new();
        for deg in [
            -90.0f32, -89.999, -60.0, -45.0, 0.0, 30.0, 45.0, 89.999, 90.0, 135.0,
        ] {
            let r = deg.to_radians();
            cases.push(Quat::from_rotation_x(r));
            cases.push(Quat::from_rotation_y(r));
            cases.push(Quat::from_rotation_z(r));
        }
        // 复合旋转（含 y 落在万向锁附近的那几组）
        cases.push(Quat::from_euler_xyz(0.4, std::f32::consts::FRAC_PI_2, 0.9));
        cases.push(Quat::from_euler_xyz(
            -1.2,
            -std::f32::consts::FRAC_PI_2,
            0.3,
        ));
        cases.push(Quat::from_euler_xyz(0.2, 0.5, -0.7));
        cases.push(Quat::from_axis_angle(
            Vec3::new(1.0, 1.0, -1.0).normalize(),
            2.0,
        ));

        for q in cases {
            let deg = to_euler_xyz_deg(q);
            let r = std::f32::consts::PI / 180.0;
            let back = Quat::from_euler_xyz(deg[0] * r, deg[1] * r, deg[2] * r);
            // 四元数 q 与 -q 表示同一个旋转。
            let dot = (q.x * back.x + q.y * back.y + q.z * back.z + q.w * back.w).abs();
            assert!(dot > 0.9999, "{q:?} → {deg:?} → {back:?}（dot={dot}）");
        }
    }
}
