//! 把场景里的**图元物体**导出成 glTF 2.0（`.gltf`，二进制几何用 base64 内嵌）。
//!
//! 和 [`crate::obj_export`] 的边界一致：只导出 `objects`（图元 / 模板实例展开后的形状），
//! `models[]` 引用的外部 glTF/OBJ 资产不重复导出；凸包没有三角网格，跳过并报告。
//! 几何写进一个 `data:application/octet-stream;base64,...` 的 buffer，所以 `.gltf` 是
//! **单文件**，不依赖外部 `.bin`。
//!
//! **贴图**：`base_color_texture` / `normal_texture` / `metallic_roughness_texture` /
//! `emissive_texture` / `ao_texture` 会按场景文件所在目录解析并内嵌进同一个 buffer（`images` + `textures`），
//! 只接受 PNG/JPEG；路径缺失/读不到/格式不支持会在 `skipped` 里报告，导出继续。
//! Web 端没有文件系统，贴图统一报告为未嵌入，几何照常导出。
//!
//! **节点树**：父子关系写成 glTF 节点 `children`，节点只带局部 `translation` /
//! `rotation`；物体 `scale` 与部件局部位姿烘进顶点。这样别的工具里能继续按层级选中，
//! 世界位置也与引擎一致。父链成环时断开成根并在 `skipped` 里报告。
//!
//! **动画**：`animations[]` 的位置/欧拉旋转轨道烘成 glTF translation/rotation
//! sampler（时间轴按 `start`/`speed` 换算，旋转关键帧转成四元数）；`loop`/`weight`
//! 是引擎运行时语义，glTF 动画本身没有这两个字段，仍按单周期导出。
//!
//! **刻意不做的事**：`uv_scale` 的平铺不写（glTF 2.0 core 没有，不擅自加扩展）；
//! 目标是"能在别的工具里打开看到同样的几何/颜色/主要贴图/层级/动画"，不是无损编辑往返。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use crate::obj_export::mesh_for_shape;
use crate::scene_doc::SceneDoc;
use quench3d::prelude::{Mat4, Vec3};

use crate::runner;

/// 导出结果。
pub struct GltfExport {
    /// `.gltf` JSON 文本。
    pub json: String,
    /// 实际写出的物体数。
    pub objects: usize,
    /// 三角形数。
    pub triangles: usize,
    /// 顶点数。
    pub vertices: usize,
    /// 内嵌的贴图数。
    pub textures: usize,
    /// 导出的动画条数。
    pub animations: usize,
    /// 没法带过去的物体/部件说明。
    pub skipped: Vec<String>,
}

/// 导出 glTF 2.0；不解析外部贴图（相对路径没有基准目录），主要用于纯几何调用方。
pub fn export_gltf(doc: &SceneDoc) -> Result<GltfExport, String> {
    export_gltf_with_base(doc, None)
}

/// 导出 glTF 2.0，并以 `base_dir` 解析相对的贴图路径。
pub fn export_gltf_with_base(
    doc: &SceneDoc,
    base_dir: Option<&Path>,
) -> Result<GltfExport, String> {
    let resolved = doc.resolved();
    if resolved.has_errors() {
        return Err(resolved.error_message());
    }
    let doc = resolved.doc.ok_or("场景 include 解析失败")?.expanded();

    let names: BTreeMap<String, usize> = doc
        .objects
        .iter()
        .enumerate()
        .map(|(i, o)| (o.name.clone().unwrap_or_else(|| format!("objects[{i}]")), i))
        .collect();
    // 父链（下标）：只认能在 `names` 里找到的父物体；环在导出时断开成根。
    let mut parent_of: Vec<Option<usize>> = doc
        .objects
        .iter()
        .map(|o| o.parent.as_ref().and_then(|p| names.get(p)).copied())
        .collect();
    let mut cycle_notes = Vec::new();
    for i in 0..parent_of.len() {
        let mut seen = vec![false; parent_of.len()];
        let mut cur = Some(i);
        while let Some(j) = cur {
            if seen[j] {
                parent_of[i] = None;
                cycle_notes.push(format!("objects[{i}]: 父链成环，已按根物体导出"));
                break;
            }
            seen[j] = true;
            cur = parent_of[j];
        }
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); doc.objects.len()];
    for (i, p) in parent_of.iter().enumerate() {
        if let Some(p) = p {
            children[*p].push(i);
        }
    }
    let roots: Vec<usize> = (0..doc.objects.len())
        .filter(|i| parent_of[*i].is_none())
        .collect();

    let mut buffer: Vec<u8> = Vec::new();
    let mut buffer_views: Vec<Value> = Vec::new();
    let mut accessors: Vec<Value> = Vec::new();
    let mut meshes: Vec<Value> = Vec::new();
    let mut materials: Vec<Value> = Vec::new();
    let mut material_index: BTreeMap<String, usize> = BTreeMap::new();
    let mut images: Vec<Value> = Vec::new();
    let mut textures: Vec<Value> = Vec::new();
    let mut animations: Vec<Value> = Vec::new();
    let mut skipped = Vec::new();
    skipped.extend(cycle_notes);
    // glTF 节点树：节点只带局部平移/旋转；物体缩放与部件局部位姿烘进顶点。
    // 这样父子层级在别的工具里仍然可见，世界位置也不会变。
    let mut nodes: Vec<Value> = (0..doc.objects.len())
        .map(|i| {
            let object = &doc.objects[i];
            let pos = Vec3::from_array(object.position.unwrap_or([0.0, 0.0, 0.0]));
            let q = runner::euler_quat(object.rotation_deg);
            let name = object
                .name
                .clone()
                .unwrap_or_else(|| format!("objects[{i}]"));
            json!({
                "name": name,
                "translation": [pos.x, pos.y, pos.z],
                "rotation": [q.x, q.y, q.z, q.w],
            })
        })
        .collect();
    for (p, kids) in children.iter().enumerate() {
        if !kids.is_empty() {
            nodes[p]["children"] = json!(kids);
        }
    }
    let mut texture_cache: BTreeMap<String, Option<usize>> = BTreeMap::new();
    let mut unlit = false;
    // 先把用到的材质贴图全部装进 buffer，后面写 material 时只查索引。
    let used: BTreeSet<String> = doc
        .objects
        .iter()
        .filter_map(|o| o.material.clone())
        .collect();
    for name in &used {
        let Some(m) = doc.materials.get(name) else {
            continue;
        };
        for (kind, path) in [
            ("基础色", &m.base_color_texture),
            ("法线", &m.normal_texture),
            ("金属度-粗糙度", &m.metallic_roughness_texture),
            ("自发光", &m.emissive_texture),
            ("环境光遮蔽", &m.ao_texture),
        ] {
            if let Some(path) = path {
                load_texture(
                    base_dir,
                    path,
                    kind,
                    &mut buffer,
                    &mut buffer_views,
                    &mut images,
                    &mut textures,
                    &mut texture_cache,
                    &mut skipped,
                );
            }
        }
    }
    let mut objects = 0usize;
    let mut triangles = 0usize;
    let mut vertices = 0usize;

    for (i, object) in doc.objects.iter().enumerate() {
        let name = object
            .name
            .clone()
            .unwrap_or_else(|| format!("objects[{i}]"));
        let scale = Vec3::from_array(object.scale.unwrap_or([1.0, 1.0, 1.0]));
        // 顶点只到物体局部空间；世界位置由父节点链的 translation/rotation 提供。
        let object_mat = Mat4::from_scale(scale);
        let parts = object.parts();
        if parts.is_empty() {
            skipped.push(format!("{name}: 没有形状"));
            continue;
        }
        let material = object
            .material
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let mat_index = *material_index.entry(material.clone()).or_insert_with(|| {
            let index = materials.len();
            materials.push(material_json(
                &doc,
                &material,
                &texture_cache,
                &mut unlit,
                &mut skipped,
            ));
            index
        });
        let mut primitives = Vec::new();
        for (part_index, part) in parts.iter().enumerate() {
            let Some((mesh, mesh_scale)) = mesh_for_shape(&part.shape) else {
                skipped.push(format!("{name} 部件 #{part_index}: 凸包没有三角网格，跳过"));
                continue;
            };
            if mesh.is_empty() {
                continue;
            }
            let part_mat =
                Mat4::from_translation(Vec3::from_array(part.position.unwrap_or([0.0, 0.0, 0.0])))
                    * Mat4::from_quat(runner::euler_quat(part.rotation_deg));
            // 物体缩放 × 部件局部位姿；父链由 glTF 节点树表达。
            // `mesh_scale` 是图元网格自身的半径/半高缩放（例如球半径 0.5 → 1）。
            let transform = object_mat * Mat4::from_scale(mesh_scale) * part_mat;
            let normal_mat = transform.inverse().transpose();
            let positions = mesh
                .vertices
                .iter()
                .map(|v| transform.transform_point3(v.pos()))
                .collect::<Vec<_>>();
            let normals = mesh
                .vertices
                .iter()
                .map(|v| {
                    let n = normal_mat.transform_vector3(v.nrm());
                    if n.length_squared() > 1e-12 {
                        n.normalize()
                    } else {
                        Vec3::Y
                    }
                })
                .collect::<Vec<_>>();

            let pos_accessor =
                push_vec3(&mut buffer, &mut buffer_views, &mut accessors, &positions);
            let normal_accessor =
                push_vec3(&mut buffer, &mut buffer_views, &mut accessors, &normals);
            let uv_accessor = push_vec2(
                &mut buffer,
                &mut buffer_views,
                &mut accessors,
                &mesh.vertices,
            );
            let index_accessor = push_indices(
                &mut buffer,
                &mut buffer_views,
                &mut accessors,
                &mesh.indices,
            );
            primitives.push(json!({
                "attributes": {
                    "POSITION": pos_accessor,
                    "NORMAL": normal_accessor,
                    "TEXCOORD_0": uv_accessor,
                },
                "indices": index_accessor,
                "material": mat_index,
                "mode": 4,
            }));
            triangles += mesh.triangle_count();
            vertices += mesh.vertex_count();
        }
        if primitives.is_empty() {
            skipped.push(format!("{name}: 没有可导出的三角网格"));
            continue;
        }
        meshes.push(json!({
            "name": name,
            "primitives": primitives,
        }));
        nodes[i]["mesh"] = json!(meshes.len() - 1);
        objects += 1;
    }

    let mut root = json!({
        "asset": {
            "version": "2.0",
            "generator": "quench3d gltf_export",
        },
        "scene": 0,
        "scenes": [{ "nodes": roots }],
    });
    // 动画：每个 AnimationDoc 一条 clip；位置/旋转轨道各一个 sampler/channel。
    // 目标按物体名找节点；找不到或轨道非法就跳过并报告，不影响几何。
    for (anim_index, anim) in doc.animations.iter().enumerate() {
        let name = anim
            .name
            .clone()
            .unwrap_or_else(|| format!("animation_{anim_index}"));
        let Some(&node) = names.get(&anim.target) else {
            skipped.push(format!("{name}: 目标 {} 不存在，动画跳过", anim.target));
            continue;
        };
        let speed = anim.speed.unwrap_or(1.0);
        if !speed.is_finite() || speed <= 0.0 {
            skipped.push(format!("{name}: speed 必须 > 0，动画跳过"));
            continue;
        }
        let start = anim.start.unwrap_or(0.0);
        if !start.is_finite() {
            skipped.push(format!("{name}: start 不是有限数，动画跳过"));
            continue;
        }
        let mut samplers = Vec::new();
        let mut channels = Vec::new();
        let mut add_track = |track: &crate::scene_doc::TrackDoc,
                             path: &str,
                             output: TrackOutput,
                             samplers: &mut Vec<Value>,
                             channels: &mut Vec<Value>| {
            if track.times.len() != track.values.len()
                || track.times.is_empty()
                || track.times.iter().any(|t| !t.is_finite() || *t < 0.0)
                || track.times.windows(2).any(|w| w[0] >= w[1])
            {
                return Err(format!("{name}: {path} 轨道时间/值不合法"));
            }
            let times: Vec<f32> = track.times.iter().map(|t| start + t / speed).collect();
            let times =
                push_scalar_accessor(&mut buffer, &mut buffer_views, &mut accessors, &times);
            let output = match output {
                TrackOutput::Translation => push_vec3_accessor(
                    &mut buffer,
                    &mut buffer_views,
                    &mut accessors,
                    &track
                        .values
                        .iter()
                        .map(|v| Vec3::from_array(*v))
                        .collect::<Vec<_>>(),
                ),
                TrackOutput::Rotation => {
                    let quats: Vec<[f32; 4]> = track
                        .values
                        .iter()
                        .map(|v| {
                            let q = runner::euler_quat(Some(*v));
                            [q.x, q.y, q.z, q.w]
                        })
                        .collect();
                    push_quat_accessor(&mut buffer, &mut buffer_views, &mut accessors, &quats)
                }
            };
            let sampler = samplers.len();
            samplers.push(json!({
                "input": times,
                "output": output,
                "interpolation": "LINEAR",
            }));
            channels.push(json!({
                "sampler": sampler,
                "target": { "node": node, "path": path },
            }));
            Ok(())
        };
        if let Some(track) = &anim.position {
            if let Err(e) = add_track(
                track,
                "translation",
                TrackOutput::Translation,
                &mut samplers,
                &mut channels,
            ) {
                skipped.push(e);
            }
        }
        if let Some(track) = &anim.rotation {
            if let Err(e) = add_track(
                track,
                "rotation",
                TrackOutput::Rotation,
                &mut samplers,
                &mut channels,
            ) {
                skipped.push(e);
            }
        }
        if channels.is_empty() {
            skipped.push(format!("{name}: 没有可导出的位置/旋转轨道，动画跳过"));
            continue;
        }
        animations.push(json!({
            "name": name,
            "samplers": samplers,
            "channels": channels,
        }));
    }

    // 空场景不要写空数组：glTF 2.0 的 buffers/accessors 等数组出现时要求至少一项。
    if !nodes.is_empty() {
        root["nodes"] = json!(nodes);
        root["meshes"] = json!(meshes);
        root["materials"] = json!(materials);
        root["accessors"] = json!(accessors);
        root["bufferViews"] = json!(buffer_views);
    }
    if !images.is_empty() {
        root["images"] = json!(images);
        root["textures"] = json!(textures);
        root["samplers"] = json!([{
            "magFilter": 9729, // LINEAR
            "minFilter": 9987, // LINEAR_MIPMAP_LINEAR
            "wrapS": 10497,    // REPEAT
            "wrapT": 10497,
        }]);
    }
    if !animations.is_empty() {
        root["animations"] = json!(animations);
    }
    if unlit {
        root["extensionsUsed"] = json!(["KHR_materials_unlit"]);
    }
    // 必须在动画/贴图/几何都追加完之后再编码：否则 accessor 指向的字节不在 URI 里。
    if !buffer.is_empty() {
        root["buffers"] = json!([{
            "byteLength": buffer.len(),
            "uri": format!("data:application/octet-stream;base64,{}", base64(&buffer)),
        }]);
    }
    let json =
        serde_json::to_string_pretty(&root).map_err(|e| format!("glTF JSON 序列化失败：{e}"))?;
    Ok(GltfExport {
        json,
        objects,
        triangles,
        vertices,
        textures: images.len(),
        animations: animations.len(),
        skipped,
    })
}

/// 与渲染器/OBJ 导出一致的材质近似：基础色、金属度、粗糙度、自发光、alpha、贴图。
fn material_json(
    doc: &SceneDoc,
    name: &str,
    textures: &BTreeMap<String, Option<usize>>,
    unlit: &mut bool,
    skipped: &mut Vec<String>,
) -> Value {
    let Some(m) = doc.materials.get(name) else {
        return json!({
            "name": name,
            "pbrMetallicRoughness": {
                "baseColorFactor": [0.72, 0.72, 0.75, 1.0],
                "metallicFactor": 0.0,
                "roughnessFactor": 0.6,
            },
        });
    };
    let alpha = m.alpha.unwrap_or(1.0).clamp(0.0, 1.0);
    let emissive = m.emissive_color.unwrap_or(m.color);
    let strength = m.emissive.max(0.0).clamp(0.0, 1.0);
    let mut out = json!({
        "name": name,
        "pbrMetallicRoughness": {
            "baseColorFactor": [m.color[0], m.color[1], m.color[2], alpha],
            "metallicFactor": m.metallic.clamp(0.0, 1.0),
            "roughnessFactor": m.roughness.clamp(0.0, 1.0),
        },
        "emissiveFactor": [
            (emissive[0] * strength).clamp(0.0, 1.0),
            (emissive[1] * strength).clamp(0.0, 1.0),
            (emissive[2] * strength).clamp(0.0, 1.0),
        ],
    });
    let tex = |path: &Option<String>| {
        path.as_deref()
            .and_then(|p| textures.get(p).copied().flatten())
    };
    if let Some(index) = tex(&m.base_color_texture) {
        out["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": index });
    }
    if let Some(index) = tex(&m.metallic_roughness_texture) {
        out["pbrMetallicRoughness"]["metallicRoughnessTexture"] = json!({ "index": index });
    }
    if let Some(index) = tex(&m.normal_texture) {
        out["normalTexture"] = json!({
            "index": index,
            "scale": m.normal_scale.unwrap_or(1.0),
        });
    }
    if let Some(index) = tex(&m.emissive_texture) {
        out["emissiveTexture"] = json!({ "index": index });
    }
    if let Some(index) = tex(&m.ao_texture) {
        out["occlusionTexture"] = json!({
            "index": index,
            "strength": m.ao_strength.unwrap_or(1.0).clamp(0.0, 1.0),
        });
    }
    if m.uv_scale.is_some_and(|s| (s - 1.0).abs() > 1e-6) {
        skipped.push(format!(
            "材质 {name}: uv_scale 的平铺不写进 glTF（core 无此字段）"
        ));
    }
    let cutoff = m.alpha_cutoff.unwrap_or(0.0);
    if cutoff > 0.0 {
        out["alphaMode"] = json!("MASK");
        out["alphaCutoff"] = json!(cutoff.clamp(0.0, 1.0));
    } else if alpha < 1.0 || m.alpha_blend == Some(true) {
        out["alphaMode"] = json!("BLEND");
    }
    if m.double_sided == Some(true) {
        out["doubleSided"] = json!(true);
    }
    if m.unlit == Some(true) {
        out["extensions"] = json!({ "KHR_materials_unlit": {} });
        *unlit = true;
    }
    out
}

/// 读一张贴图并把字节登记到同一个 buffer；失败只记 `skipped`，不阻断导出。
#[allow(clippy::too_many_arguments)]
fn load_texture(
    base: Option<&Path>,
    path: &str,
    kind: &str,
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
    images: &mut Vec<Value>,
    textures: &mut Vec<Value>,
    cache: &mut BTreeMap<String, Option<usize>>,
    skipped: &mut Vec<String>,
) {
    if cache.contains_key(path) {
        return;
    }
    let index = match read_image(base, path) {
        Ok((data, mime)) => {
            let view = push_image_view(buffer, views, &data);
            let image_index = images.len();
            images.push(json!({
                "bufferView": view,
                "mimeType": mime,
            }));
            let texture_index = textures.len();
            textures.push(json!({
                "sampler": 0,
                "source": image_index,
            }));
            Some(texture_index)
        }
        Err(e) => {
            skipped.push(format!("{kind}贴图未嵌入：{path}（{e}）"));
            None
        }
    };
    cache.insert(path.to_string(), index);
}

/// 读一张图并判断 MIME；相对路径拼到场景目录上。
#[cfg(not(target_arch = "wasm32"))]
fn read_image(base: Option<&Path>, path: &str) -> Result<(Vec<u8>, &'static str), String> {
    let raw = Path::new(path);
    let full = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        let Some(base) = base else {
            return Err("没有场景目录可解析相对路径".to_string());
        };
        base.join(raw)
    };
    let data = std::fs::read(&full).map_err(|e| format!("读 {} 失败：{e}", full.display()))?;
    let mime = if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        "image/png"
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else {
        return Err("不是 PNG/JPEG".to_string());
    };
    Ok((data, mime))
}

/// Web 端没有文件系统；返回明确原因，让 `skipped` 说明为什么没贴图。
#[cfg(target_arch = "wasm32")]
fn read_image(_base: Option<&Path>, _path: &str) -> Result<(Vec<u8>, &'static str), String> {
    Err("Web 端没有本地文件系统，贴图需由桌面端导出".to_string())
}

fn push_image_view(buffer: &mut Vec<u8>, views: &mut Vec<Value>, data: &[u8]) -> usize {
    align4(buffer);
    let offset = buffer.len();
    buffer.extend_from_slice(data);
    views.push(json!({
        "buffer": 0,
        "byteOffset": offset,
        "byteLength": data.len(),
    }));
    views.len() - 1
}

fn align4(buffer: &mut Vec<u8>) {
    while !buffer.len().is_multiple_of(4) {
        buffer.push(0);
    }
}

/// 动画轨道输出类型（glTF channel path 的两种）。
enum TrackOutput {
    Translation,
    Rotation,
}

/// 动画的 bufferView 不带 `target`：`target` 只用于顶点/索引缓冲。
fn push_anim_view(buffer: &mut Vec<u8>, views: &mut Vec<Value>, data: &[u8]) -> usize {
    align4(buffer);
    let offset = buffer.len();
    buffer.extend_from_slice(data);
    views.push(json!({
        "buffer": 0,
        "byteOffset": offset,
        "byteLength": data.len(),
    }));
    views.len() - 1
}

fn push_scalar_accessor(
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
    accessors: &mut Vec<Value>,
    values: &[f32],
) -> usize {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for v in values {
        bytes.extend_from_slice(&v.to_le_bytes());
        min = min.min(*v);
        max = max.max(*v);
    }
    let view = push_anim_view(buffer, views, &bytes);
    accessors.push(json!({
        "bufferView": view,
        "componentType": 5126,
        "count": values.len(),
        "type": "SCALAR",
        "min": [min],
        "max": [max],
    }));
    accessors.len() - 1
}

fn push_vec3_accessor(
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
    accessors: &mut Vec<Value>,
    values: &[Vec3],
) -> usize {
    let mut bytes = Vec::with_capacity(values.len() * 12);
    for v in values {
        for x in [v.x, v.y, v.z] {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
    }
    let view = push_anim_view(buffer, views, &bytes);
    accessors.push(json!({
        "bufferView": view,
        "componentType": 5126,
        "count": values.len(),
        "type": "VEC3",
    }));
    accessors.len() - 1
}

fn push_quat_accessor(
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
    accessors: &mut Vec<Value>,
    values: &[[f32; 4]],
) -> usize {
    let mut bytes = Vec::with_capacity(values.len() * 16);
    for q in values {
        for x in q {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
    }
    let view = push_anim_view(buffer, views, &bytes);
    accessors.push(json!({
        "bufferView": view,
        "componentType": 5126,
        "count": values.len(),
        "type": "VEC4",
    }));
    accessors.len() - 1
}

fn push_view(buffer: &mut Vec<u8>, views: &mut Vec<Value>, data: &[u8], target: u32) -> usize {
    align4(buffer);
    let offset = buffer.len();
    buffer.extend_from_slice(data);
    views.push(json!({
        "buffer": 0,
        "byteOffset": offset,
        "byteLength": data.len(),
        "target": target,
    }));
    views.len() - 1
}

fn push_vec3(
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
    accessors: &mut Vec<Value>,
    values: &[Vec3],
) -> usize {
    let mut bytes = Vec::with_capacity(values.len() * 12);
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for v in values {
        for x in [v.x, v.y, v.z] {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        min = min.min(*v);
        max = max.max(*v);
    }
    let view = push_view(buffer, views, &bytes, 34962);
    accessors.push(json!({
        "bufferView": view,
        "componentType": 5126,
        "count": values.len(),
        "type": "VEC3",
        "min": [min.x, min.y, min.z],
        "max": [max.x, max.y, max.z],
    }));
    accessors.len() - 1
}

fn push_vec2(
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
    accessors: &mut Vec<Value>,
    values: &[quench3d::render::Vertex],
) -> usize {
    let mut bytes = Vec::with_capacity(values.len() * 8);
    for v in values {
        bytes.extend_from_slice(&v.uv[0].to_le_bytes());
        bytes.extend_from_slice(&v.uv[1].to_le_bytes());
    }
    let view = push_view(buffer, views, &bytes, 34962);
    accessors.push(json!({
        "bufferView": view,
        "componentType": 5126,
        "count": values.len(),
        "type": "VEC2",
    }));
    accessors.len() - 1
}

fn push_indices(
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
    accessors: &mut Vec<Value>,
    values: &[u32],
) -> usize {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for i in values {
        bytes.extend_from_slice(&i.to_le_bytes());
    }
    let view = push_view(buffer, views, &bytes, 34963);
    accessors.push(json!({
        "bufferView": view,
        "componentType": 5125,
        "count": values.len(),
        "type": "SCALAR",
    }));
    accessors.len() - 1
}

/// 标准 base64（带 `=` 填充）；只给二进制 buffer 用，避免为一个函数加依赖。
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 把 [`GltfExport`] 转成二进制 `.glb`（JSON chunk + BIN chunk，无 base64）。
///
/// `.gltf` 里的 `data:...;base64,...` 是单文件方案；`.glb` 把同一份字节放进 BIN
/// chunk，省掉 base64 的 33% 膨胀，加载器也不需要解码 data URI。JSON 里的
/// `buffers[0].uri` 必须删掉，否则规范要求外部资源。
pub fn to_glb(export: &GltfExport) -> Result<Vec<u8>, String> {
    glb_from_json(&export.json)
}

/// 从已经序列化好的 `.gltf` JSON 转 GLB；编辑器拿到的是 `SavedGltf.json`，走这个入口，
/// 不必再把整个 `GltfExport` 搬来搬去。
pub fn glb_from_json(json: &str) -> Result<Vec<u8>, String> {
    let mut root: Value =
        serde_json::from_str(json).map_err(|e| format!("glTF JSON 解析失败：{e}"))?;
    let bin = match root["buffers"].get(0) {
        Some(buffer) => {
            let uri = buffer["uri"]
                .as_str()
                .ok_or("glTF buffer 缺少内嵌 data URI")?;
            let payload = uri
                .strip_prefix("data:application/octet-stream;base64,")
                .ok_or("glTF buffer 不是内嵌 base64")?;
            base64_decode(payload)?
        }
        None => Vec::new(),
    };
    if let Some(buffer) = root["buffers"].get_mut(0) {
        buffer
            .as_object_mut()
            .ok_or("buffers[0] 不是对象")?
            .remove("uri");
    }
    let json = serde_json::to_string(&root).map_err(|e| format!("glTF JSON 序列化失败：{e}"))?;
    let mut json_bytes = json.into_bytes();
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }
    let mut bin_bytes = bin;
    while !bin_bytes.len().is_multiple_of(4) {
        bin_bytes.push(0);
    }
    let has_bin = !bin_bytes.is_empty();
    let total = 12 + 8 + json_bytes.len() + if has_bin { 8 + bin_bytes.len() } else { 0 };
    let total = u32::try_from(total).map_err(|_| "GLB 超过 4 GiB")?;
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // 'JSON'
    out.extend_from_slice(&json_bytes);
    if has_bin {
        out.extend_from_slice(&(bin_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E_4942u32.to_le_bytes()); // 'BIN\0'
        out.extend_from_slice(&bin_bytes);
    }
    Ok(out)
}

/// 解标准 base64（带/不带 `=` 填充都接受）；只给 GLB 的 BIN 提取用。
fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(value) = TABLE.iter().position(|b| *b == byte) else {
            return Err(format!("base64 里有非法字符：{}", byte as char));
        };
        acc = (acc << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits != 0 && bits != 2 && bits != 4 {
        return Err("base64 尾部位数不合法".to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_doc::SceneDoc;

    fn scene() -> SceneDoc {
        SceneDoc::from_json(
            r#"{
                "materials": { "红": { "color": [0.8, 0.2, 0.1], "roughness": 0.4 } },
                "objects": [
                    { "name": "箱", "shape": { "type": "box", "size": [1, 2, 3] },
                      "position": [1, 0, 0], "material": "红" },
                    { "name": "子", "shape": { "type": "sphere", "radius": 0.5 },
                      "position": [0, 2, 0], "parent": "箱" }
                ]
            }"#,
        )
        .expect("场景必须能解析")
    }

    #[test]
    fn exports_valid_gltf_with_embedded_buffer_and_transforms() {
        let out = export_gltf(&scene()).expect("导出必须成功");
        let json: Value = serde_json::from_str(&out.json).expect("必须是合法 JSON");
        assert_eq!(json["asset"]["version"], "2.0");
        assert_eq!(out.objects, 2);
        assert_eq!(json["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(json["meshes"].as_array().unwrap().len(), 2);
        assert!(json["buffers"][0]["uri"]
            .as_str()
            .unwrap()
            .starts_with("data:application/octet-stream;base64,"));
        assert!(out.triangles > 0 && out.vertices > 0);

        // 父链是真正的节点树：根节点只含父，子节点挂在 children 下。
        assert_eq!(json["scenes"][0]["nodes"], json!([0]));
        assert_eq!(json["nodes"][0]["children"], json!([1]));
        assert_eq!(json["nodes"][0]["translation"], json!([1.0, 0.0, 0.0]));
        assert_eq!(json["nodes"][1]["translation"], json!([0.0, 2.0, 0.0]));
        // 子几何只到物体局部空间（球心在原点），世界位移由节点 translation 提供。
        let pos_accessor = json["meshes"][1]["primitives"][0]["attributes"]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        let min = json["accessors"][pos_accessor]["min"].as_array().unwrap();
        assert!(
            (min[0].as_f64().unwrap() + 0.5).abs() < 1e-3,
            "子物体局部 X 范围不对：{min:?}"
        );
        assert!(
            (min[1].as_f64().unwrap() + 0.5).abs() < 1e-3,
            "子物体局部 Y 范围不对：{min:?}"
        );
    }

    #[test]
    fn animation_tracks_become_translation_and_rotation_channels() {
        let doc = SceneDoc::from_json(
            r#"{
                "objects": [{ "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] } }],
                "animations": [{
                    "name": "移动",
                    "target": "箱",
                    "start": 1.0,
                    "speed": 2.0,
                    "position": { "times": [0.0, 1.0], "values": [[0, 0, 0], [2, 0, 0]] },
                    "rotation": { "times": [0.0, 1.0], "values": [[0, 0, 0], [0, 90, 0]] }
                }]
            }"#,
        )
        .unwrap();
        let out = export_gltf(&doc).expect("导出必须成功");
        assert_eq!(out.animations, 1);
        let json: Value = serde_json::from_str(&out.json).unwrap();
        let anim = &json["animations"][0];
        assert_eq!(anim["name"], "移动");
        assert_eq!(anim["channels"].as_array().unwrap().len(), 2);
        assert_eq!(anim["samplers"].as_array().unwrap().len(), 2);
        assert_eq!(anim["channels"][0]["target"]["node"], 0);
        assert_eq!(anim["channels"][0]["target"]["path"], "translation");
        assert_eq!(anim["channels"][1]["target"]["path"], "rotation");
        // start=1、speed=2：输入时间 0..1 被烘成 1..1.5。
        let input = anim["samplers"][0]["input"].as_u64().unwrap() as usize;
        let accessor = &json["accessors"][input];
        assert_eq!(accessor["type"], "SCALAR");
        assert_eq!(accessor["count"], 2);
        assert_eq!(accessor["min"][0], 1.0);
        assert_eq!(accessor["max"][0], 1.5);
        let output = anim["samplers"][1]["output"].as_u64().unwrap() as usize;
        assert_eq!(json["accessors"][output]["type"], "VEC4");
        // buffer URI 必须在动画 accessor 也追加完之后再编码。
        let uri = json["buffers"][0]["uri"].as_str().unwrap();
        let raw = base64_decode(
            uri.strip_prefix("data:application/octet-stream;base64,")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            raw.len(),
            json["buffers"][0]["byteLength"].as_u64().unwrap() as usize
        );
        let view = json["accessors"][output]["bufferView"].as_u64().unwrap() as usize;
        let end = json["bufferViews"][view]["byteOffset"].as_u64().unwrap() as usize
            + json["bufferViews"][view]["byteLength"].as_u64().unwrap() as usize;
        assert!(end <= raw.len(), "accessor 越过了内嵌 buffer");
    }

    #[test]
    fn glb_has_aligned_json_and_bin_chunks_without_data_uri() {
        let out = export_gltf(&scene()).expect("导出必须成功");
        let glb = to_glb(&out).expect("GLB 转换必须成功");
        assert_eq!(&glb[0..4], b"glTF");
        assert_eq!(u32::from_le_bytes(glb[4..8].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(glb[8..12].try_into().unwrap()) as usize,
            glb.len()
        );
        let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        assert!(json_len.is_multiple_of(4), "JSON chunk 必须 4 字节对齐");
        assert_eq!(
            u32::from_le_bytes(glb[16..20].try_into().unwrap()),
            0x4E4F_534A
        );
        let json: Value = serde_json::from_slice(&glb[20..20 + json_len]).unwrap();
        assert_eq!(json["asset"]["version"], "2.0");
        assert!(
            json["buffers"][0].get("uri").is_none(),
            "GLB 不能留 data URI"
        );
        let bin_header = 20 + json_len;
        let bin_len =
            u32::from_le_bytes(glb[bin_header..bin_header + 4].try_into().unwrap()) as usize;
        assert_eq!(
            u32::from_le_bytes(glb[bin_header + 4..bin_header + 8].try_into().unwrap()),
            0x004E_4942
        );
        assert!(bin_len > 0 && bin_len.is_multiple_of(4));
        assert_eq!(bin_header + 8 + bin_len, glb.len());
        assert_eq!(
            json["buffers"][0]["byteLength"].as_u64().unwrap() as usize,
            bin_len
        );
    }

    #[test]
    fn embeds_texture_bytes_and_maps_material_fields() {
        let dir = std::env::temp_dir().join(format!("quench3d-gltf-tex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 合法的 1x1 PNG（RGBA）；导入路径会真的解码，CRC 不能是假的。
        let png = base64_decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
        )
        .unwrap();
        std::fs::write(dir.join("tex.png"), &png).unwrap();
        let doc = SceneDoc::from_json(
            r#"{
                "materials": {
                    "带图": {
                        "color": [1, 1, 1],
                        "base_color_texture": "tex.png",
                        "ao_texture": "tex.png", "ao_strength": 0.4,
                        "alpha_cutoff": 0.4,
                        "double_sided": true
                    }
                },
                "objects": [{ "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] }, "material": "带图" }]
            }"#,
        )
        .unwrap();
        let out = export_gltf_with_base(&doc, Some(&dir)).expect("导出必须成功");
        let json: Value = serde_json::from_str(&out.json).unwrap();
        assert_eq!(out.textures, 1);
        assert_eq!(json["images"][0]["mimeType"], "image/png");
        assert_eq!(json["textures"][0]["source"], 0);
        let pbr = &json["materials"][0]["pbrMetallicRoughness"];
        assert_eq!(pbr["baseColorTexture"]["index"], 0);
        assert_eq!(json["materials"][0]["occlusionTexture"]["index"], 0);
        assert!(
            (json["materials"][0]["occlusionTexture"]["strength"]
                .as_f64()
                .unwrap()
                - 0.4)
                .abs()
                < 1e-6
        );
        assert_eq!(json["materials"][0]["alphaMode"], "MASK");

        // 同一份导出转成 GLB 再从字节流读回来：AO 贴图与强度不能丢。
        let glb = to_glb(&out).expect("转 GLB");
        let loaded = quench3d::assets::load_model_from_bytes("ao.glb", &glb).expect("读回 GLB");
        let m = &loaded.materials[0];
        assert_eq!(m.ao_texture, Some(0), "glTF 导入的 AO 贴图下标应当保留");
        assert!(
            (m.desc.ao_strength - 0.4).abs() < 1e-6,
            "glTF 导入的 AO 强度应当保留，实际 {}",
            m.desc.ao_strength
        );
        assert!((json["materials"][0]["alphaCutoff"].as_f64().unwrap() - 0.4).abs() < 1e-5);
        assert_eq!(json["materials"][0]["doubleSided"], true);
        // 图字节必须真的在同一个 data URI buffer 里（不是只写了个 image 索引）。
        assert!(json["buffers"][0]["uri"].as_str().unwrap().len() > 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_texture_is_reported_without_blocking_geometry() {
        let dir = std::env::temp_dir().join(format!("quench3d-gltf-miss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let doc = SceneDoc::from_json(
            r#"{
                "materials": { "缺图": { "color": [1, 0, 0], "base_color_texture": "nope.png" } },
                "objects": [{ "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] }, "material": "缺图" }]
            }"#,
        )
        .unwrap();
        let out = export_gltf_with_base(&doc, Some(&dir)).expect("缺贴图不应让几何导出失败");
        assert_eq!(out.objects, 1);
        assert_eq!(out.textures, 0);
        assert!(out.skipped.iter().any(|s| s.contains("贴图未嵌入")));
        let json: Value = serde_json::from_str(&out.json).unwrap();
        assert!(json.get("images").is_none());
        assert!(json["materials"][0]["pbrMetallicRoughness"]
            .get("baseColorTexture")
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nested_nodes_keep_three_levels_of_translation() {
        let doc = SceneDoc::from_json(
            r#"{
                "objects": [
                    { "name": "根", "shape": { "type": "sphere", "radius": 0.5 }, "position": [1, 0, 0] },
                    { "name": "中", "shape": { "type": "sphere", "radius": 0.5 }, "parent": "根", "position": [0, 2, 0] },
                    { "name": "孙", "shape": { "type": "sphere", "radius": 0.5 }, "parent": "中", "position": [0, 0, 3] }
                ]
            }"#,
        )
        .unwrap();
        let out = export_gltf(&doc).expect("导出必须成功");
        let json: Value = serde_json::from_str(&out.json).unwrap();
        assert_eq!(json["scenes"][0]["nodes"], json!([0]));
        assert_eq!(json["nodes"][0]["children"], json!([1]));
        assert_eq!(json["nodes"][1]["children"], json!([2]));
        assert_eq!(json["nodes"][1]["translation"], json!([0.0, 2.0, 0.0]));
        assert_eq!(json["nodes"][2]["translation"], json!([0.0, 0.0, 3.0]));
        // 孙球局部包围盒仍是单位球，世界位置靠 1 + 2 + 3 的节点链得到。
        let pos_accessor = json["meshes"][2]["primitives"][0]["attributes"]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        let min = json["accessors"][pos_accessor]["min"].as_array().unwrap();
        assert!((min[0].as_f64().unwrap() + 0.5).abs() < 1e-3);
        assert!((min[1].as_f64().unwrap() + 0.5).abs() < 1e-3);
    }

    #[test]
    fn convex_is_skipped_and_reported() {
        let doc = SceneDoc::from_json(
            r#"{"objects": [{"name": "凸", "shape": {"type": "convex", "points": [[0,0,0],[1,0,0],[0,1,0],[0,0,1]]}}]}"#,
        )
        .expect("场景必须能解析");
        let out = export_gltf(&doc).expect("导出必须成功");
        assert_eq!(out.objects, 0);
        assert!(out.skipped.iter().any(|s| s.contains("凸包")));
        assert!(out.skipped.iter().any(|s| s.contains("没有可导出")));
    }
}
