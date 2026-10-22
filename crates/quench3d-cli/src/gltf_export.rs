//! 把场景里的**图元物体**导出成 glTF 2.0（`.gltf`，二进制几何用 base64 内嵌）。
//!
//! 和 [`crate::obj_export`] 的边界一致：只导出 `objects`（图元 / 模板实例展开后的形状），
//! `models[]` 引用的外部 glTF/OBJ 资产不重复导出；凸包没有三角网格，跳过并报告。
//! 几何写进一个 `data:application/octet-stream;base64,...` 的 buffer，所以 `.gltf` 是
//! **单文件**，不依赖外部 `.bin`。
//!
//! **刻意不做的事**：贴图不嵌入（`base_color_texture` 等会在 `skipped` 里报告）；节点是
//! 扁平的（世界变换直接烘进顶点，父子层级不写进 glTF），因为图元导出的目标是"能在别的
//! 工具里打开看到同样的几何/颜色"，不是无损编辑往返。

use std::collections::BTreeMap;

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
    /// 没法带过去的物体/部件说明。
    pub skipped: Vec<String>,
}

/// 导出 glTF 2.0；返回可直接写进 `.gltf` 的 JSON 文本。
pub fn export_gltf(doc: &SceneDoc) -> Result<GltfExport, String> {
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
    let mut skipped = Vec::new();
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
            materials.push(material_json(&doc, &material));
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

    // 贴图是明确不带的；在报告里说清楚，别让人以为颜色贴图悄悄丢了。
    for object in &doc.objects {
        let Some(material) = object.material.as_ref().and_then(|m| doc.materials.get(m)) else {
            continue;
        };
        for (kind, path) in [
            ("基础色", &material.base_color_texture),
            ("法线", &material.normal_texture),
            ("金属度-粗糙度", &material.metallic_roughness_texture),
            ("自发光", &material.emissive_texture),
        ] {
            if let Some(path) = path {
                skipped.push(format!("贴图不嵌入：{kind} {path}"));
            }
        }
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
        skipped,
    })
}

/// 与渲染器/OBJ 导出一致的材质近似：基础色、金属度、粗糙度、自发光、alpha。
fn material_json(doc: &SceneDoc, name: &str) -> Value {
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
    if alpha < 1.0 {
        out["alphaMode"] = json!("BLEND");
    }
    out
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
