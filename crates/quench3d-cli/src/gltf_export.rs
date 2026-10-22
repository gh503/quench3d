//! 把场景里的**图元物体**导出成 glTF 2.0（`.gltf`，二进制几何用 base64 内嵌）。
//!
//! 和 [`crate::obj_export`] 的边界一致：只导出 `objects`（图元 / 模板实例展开后的形状），
//! `models[]` 引用的外部 glTF/OBJ 资产不重复导出；凸包没有三角网格，跳过并报告。
//! 几何写进一个 `data:application/octet-stream;base64,...` 的 buffer，所以 `.gltf` 是
//! **单文件**，不依赖外部 `.bin`。
//!
//! **贴图**：`base_color_texture` / `normal_texture` / `metallic_roughness_texture` /
//! `emissive_texture` 会按场景文件所在目录解析并内嵌进同一个 buffer（`images` + `textures`），
//! 只接受 PNG/JPEG；路径缺失/读不到/格式不支持会在 `skipped` 里报告，导出继续。
//! Web 端没有文件系统，贴图统一报告为未嵌入，几何照常导出。
//!
//! **刻意不做的事**：节点是扁平的（世界变换直接烘进顶点，父子层级不写进 glTF），
//! `uv_scale` 的平铺也不写（glTF 2.0 core 没有，不擅自加扩展）；目标是"能在别的工具里
//! 打开看到同样的几何/颜色/主要贴图"，不是无损编辑往返。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use crate::obj_export::{mesh_for_shape, resolve_world};
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
    let mut worlds: Vec<Option<Mat4>> = vec![None; doc.objects.len()];
    for i in 0..doc.objects.len() {
        resolve_world(i, &doc.objects, &names, &mut worlds, &mut Vec::new());
    }

    let mut buffer: Vec<u8> = Vec::new();
    let mut buffer_views: Vec<Value> = Vec::new();
    let mut accessors: Vec<Value> = Vec::new();
    let mut meshes: Vec<Value> = Vec::new();
    let mut nodes: Vec<Value> = Vec::new();
    let mut materials: Vec<Value> = Vec::new();
    let mut material_index: BTreeMap<String, usize> = BTreeMap::new();
    let mut images: Vec<Value> = Vec::new();
    let mut textures: Vec<Value> = Vec::new();
    let mut skipped = Vec::new();
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
        let world = worlds[i].unwrap_or(Mat4::IDENTITY);
        let scale = Vec3::from_array(object.scale.unwrap_or([1.0, 1.0, 1.0]));
        let object_mat = world * Mat4::from_scale(scale);
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
            // 与 OBJ 导出同一套变换：物体世界位姿 × 物体缩放 × 部件局部位姿。
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
        nodes.push(json!({
            "name": name,
            "mesh": meshes.len() - 1,
        }));
        objects += 1;
    }

    let buffer_uri = if buffer.is_empty() {
        None
    } else {
        Some(format!(
            "data:application/octet-stream;base64,{}",
            base64(&buffer)
        ))
    };
    let mut root = json!({
        "asset": {
            "version": "2.0",
            "generator": "quench3d gltf_export",
        },
        "scene": 0,
        "scenes": [{ "nodes": (0..nodes.len()).collect::<Vec<_>>() }],
    });
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
    if unlit {
        root["extensionsUsed"] = json!(["KHR_materials_unlit"]);
    }
    if let Some(uri) = buffer_uri {
        root["buffers"] = json!([{ "byteLength": buffer.len(), "uri": uri }]);
    }
    let json =
        serde_json::to_string_pretty(&root).map_err(|e| format!("glTF JSON 序列化失败：{e}"))?;
    Ok(GltfExport {
        json,
        objects,
        triangles,
        vertices,
        textures: images.len(),
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

        // 子物体的世界位置 = 父位置 + 子局部位置；几何已经烘进顶点，节点是扁平的。
        let pos_accessor = json["meshes"][1]["primitives"][0]["attributes"]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        let min = json["accessors"][pos_accessor]["min"].as_array().unwrap();
        // 子球半径 0.5、中心 x=1（父在 x=1）→ 最小 X 是 0.5。
        assert!(
            (min[0].as_f64().unwrap() - 0.5).abs() < 1e-3,
            "子物体 X 范围不对：{min:?}"
        );
        assert!(
            (min[1].as_f64().unwrap() - 1.5).abs() < 1e-3,
            "子物体 Y 范围不对：{min:?}"
        );
    }

    #[test]
    fn embeds_texture_bytes_and_maps_material_fields() {
        let dir = std::env::temp_dir().join(format!("quench3d-gltf-tex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 1x1 PNG（合法签名/IHDR/IDAT/IEND），只用于验证内嵌结构与 MIME 判断。
        let png: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00,
            0x00, 0xb5, 0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x63, 0xfc, 0xff, 0x1f, 0x00, 0x02, 0xeb, 0x01, 0xf5, 0x8f, 0x59, 0x97, 0x5b,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        std::fs::write(dir.join("tex.png"), png).unwrap();
        let doc = SceneDoc::from_json(
            r#"{
                "materials": {
                    "带图": {
                        "color": [1, 1, 1],
                        "base_color_texture": "tex.png",
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
        assert_eq!(json["materials"][0]["alphaMode"], "MASK");
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
