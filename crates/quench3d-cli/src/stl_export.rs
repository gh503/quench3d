//! 把场景里的**图元物体**导出成二进制 STL（3D 打印/切片软件通用）。
//!
//! 与 [`crate::obj_export`] 共用 `resolve_world` / `mesh_for_shape`：父子变换、
//! 圆角盒/高度场的专用网格都走同一份代码，避免 OBJ 与 STL 的位姿/尺寸对不上。
//!
//! **边界**：
//! * 只导出 `objects` + `templates` + `instances` 展开后的图元；`models[]` 不导出，
//!   凸包没有三角网格，都会进 `skipped`。
//! * STL 没有材质/贴图/单位；写的是米制坐标，由下游软件决定单位。
//! * 二进制 STL：80 字节头 + `u32` 三角形数 + 每个三角形 50 字节
//!   （法线 3×f32、三个顶点 3×f32、属性 `u16` = 0）。
//! * 导出的是**文档初始位姿**，不是物理步进后的快照。

use std::collections::BTreeMap;

use crate::obj_export::{mesh_for_shape, resolve_world};
use crate::scene_doc::SceneDoc;
use quench3d::prelude::{Mat4, Vec3};

/// STL 导出结果。
pub struct StlExport {
    /// 二进制 STL 字节。
    pub bytes: Vec<u8>,
    /// 实际写出的物体数。
    pub objects: usize,
    /// 三角形数。
    pub triangles: usize,
    /// 顶点记录数（STL 每个三角形存 3 个顶点，= `triangles * 3`）。
    pub vertices: usize,
    /// 没法写进 STL 的物体/部件说明。
    pub skipped: Vec<String>,
}

/// 导出二进制 STL。
pub fn export_stl(doc: &SceneDoc, out_name: &str) -> Result<StlExport, String> {
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

    let mut triangles: Vec<([f32; 3], [[f32; 3]; 3])> = Vec::new();
    let mut objects = 0usize;
    let mut skipped = Vec::new();

    for (i, object) in doc.objects.iter().enumerate() {
        let name = object
            .name
            .clone()
            .unwrap_or_else(|| format!("objects[{i}]"));
        let place = worlds[i].unwrap_or(Mat4::IDENTITY);
        let scale = Vec3::from_array(object.scale.unwrap_or([1.0, 1.0, 1.0]));
        let object_mat = place * Mat4::from_scale(scale);
        let parts = object.parts();
        if parts.is_empty() {
            skipped.push(format!("{name}: 没有形状"));
            continue;
        }
        let mut wrote = false;
        for (part_index, part) in parts.iter().enumerate() {
            let Some((mesh, mesh_scale)) = mesh_for_shape(&part.shape) else {
                skipped.push(format!(
                    "{name} 部件 #{part_index}: {} 没有三角网格，跳过",
                    kind_label(part.shape.kind)
                ));
                continue;
            };
            let part_mat =
                Mat4::from_translation(Vec3::from_array(part.position.unwrap_or([0.0, 0.0, 0.0])))
                    * Mat4::from_quat(crate::runner::euler_quat(part.rotation_deg));
            let transform = object_mat * part_mat;
            for tri in mesh.indices.as_chunks::<3>().0 {
                let mut points = [[0.0f32; 3]; 3];
                for (slot, &index) in tri.iter().enumerate() {
                    let Some(v) = mesh.vertices.get(index as usize) else {
                        continue;
                    };
                    let p = transform.transform_point3(v.pos() * mesh_scale);
                    points[slot] = [p.x, p.y, p.z];
                }
                let a = Vec3::from_array(points[0]);
                let b = Vec3::from_array(points[1]);
                let c = Vec3::from_array(points[2]);
                // 退化三角形没有法线；STL 允许写 0，切片器会自行忽略。
                let n = (b - a).cross(c - a).normalize_or_zero();
                triangles.push(([n.x, n.y, n.z], points));
            }
            wrote = true;
        }
        if wrote {
            objects += 1;
        }
    }

    let count = u32::try_from(triangles.len())
        .map_err(|_| format!("三角形太多（{}），超过 STL 的 u32 上限", triangles.len()))?;
    let mut bytes = Vec::with_capacity(84 + triangles.len() * 50);
    let mut header = [0u8; 80];
    // 头里写名称，尾部补 0；STL 规范只要求 80 字节、内容自由。
    let label = format!(
        "quench3d STL export - {}",
        std::path::Path::new(out_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("scene.stl")
    );
    let raw = label.as_bytes();
    header[..raw.len().min(80)].copy_from_slice(&raw[..raw.len().min(80)]);
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(&count.to_le_bytes());
    for (normal, points) in &triangles {
        for value in normal.iter().chain(points.iter().flatten()) {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&0u16.to_le_bytes());
    }

    Ok(StlExport {
        bytes,
        objects,
        triangles: triangles.len(),
        vertices: triangles.len() * 3,
        skipped,
    })
}

fn kind_label(kind: crate::scene_doc::ShapeKind) -> &'static str {
    use crate::scene_doc::ShapeKind::*;
    match kind {
        Box => "box",
        Sphere => "sphere",
        Capsule => "capsule",
        Cylinder => "cylinder",
        Cone => "cone",
        Plane => "plane",
        Convex => "convex",
        RoundedBox => "rounded_box",
        HeightField => "heightfield",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(json: &str) -> SceneDoc {
        SceneDoc::from_json(json).expect("场景应能解析")
    }

    #[test]
    fn box_exports_twelve_binary_triangles() {
        let doc = scene(r#"{"objects":[{"name":"箱","shape":{"type":"box","size":[1,2,3]}}]}"#);
        let out = export_stl(&doc, "box.stl").unwrap();
        assert_eq!(out.objects, 1);
        assert_eq!(out.triangles, 12);
        assert_eq!(out.vertices, 36);
        assert_eq!(out.bytes.len(), 84 + 12 * 50);
        assert_eq!(
            u32::from_le_bytes(out.bytes[80..84].try_into().unwrap()),
            12
        );
        assert!(out.bytes.starts_with(b"quench3d STL export"));
    }

    #[test]
    fn triangle_normals_and_translation_are_written() {
        let doc =
            scene(r#"{"objects":[{"name":"板","position":[10,2,0],"shape":{"type":"plane"}}]}"#);
        let out = export_stl(&doc, "plane.stl").unwrap();
        assert_eq!(out.triangles, 2);
        // 逐三角形读回：法线单位长，且所有顶点 y = 2（平面被平移）。
        for i in 0..out.triangles {
            let start = 84 + i * 50;
            let read = |offset: usize| {
                f32::from_le_bytes(
                    out.bytes[start + offset..start + offset + 4]
                        .try_into()
                        .unwrap(),
                )
            };
            let n = Vec3::new(read(0), read(4), read(8));
            assert!((n.length() - 1.0).abs() < 1e-5, "法线不是单位长：{n:?}");
            for v in 0..3 {
                let base = 12 + v * 12;
                assert!((read(base + 4) - 2.0).abs() < 1e-5, "顶点没有跟随平移");
            }
        }
    }

    #[test]
    fn convex_hulls_are_reported_not_silently_dropped() {
        let doc = scene(
            r#"{"objects":[{"name":"壳","shape":{"type":"convex","points":[[0,0,0],[1,0,0],[0,1,0],[0,0,1]]}}]}"#,
        );
        let out = export_stl(&doc, "hull.stl").unwrap();
        assert_eq!(out.triangles, 0);
        assert_eq!(u32::from_le_bytes(out.bytes[80..84].try_into().unwrap()), 0);
        assert!(
            out.skipped.iter().any(|s| s.contains("convex")),
            "凸包必须进 skipped：{:?}",
            out.skipped
        );
    }
}
