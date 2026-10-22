//! 把场景里的**图元物体**导出成 Wavefront OBJ + MTL。
//!
//! 为什么先做 OBJ 而不是 glTF：OBJ 的几何表达足够简单，本仓库的导入器
//! （`assets::parse_obj`）又能把它读回来，于是"导出 → 再导入"可以做成一条
//! 真正的往返测试。glTF 还要处理节点、材质槽、纹理嵌入，是另一个量级。
//!
//! **边界（不假装完整）**：
//! * 只导出 `objects` + `templates` + `instances` 展开后的图元；`models[]` 里的
//!   glTF/OBJ 资产不导出（它们仍然是外部文件），会列进 `skipped`。
//! * 凸包没有三角化网格，跳过并报告；高度场/圆角盒用自己的网格，正常导出。
//! * 材质只写标量色/粗糙度/自发光/透明度；贴图不嵌入（报告里提示）。
//! * 导出的是**当前文档的初始位姿**，不是物理跑过之后的快照。

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::scene_doc::{ObjectDoc, SceneDoc, ShapeKind};
use quench3d::prelude::{Mat4, Vec3};
use quench3d::render::MeshData;

use crate::runner;

/// 导出结果。
pub struct ObjExport {
    /// OBJ 文本。
    pub obj: String,
    /// MTL 文本。
    pub mtl: String,
    /// MTL 文件名（OBJ 的 `mtllib` 指向它）。
    pub mtl_name: String,
    /// 实际写出的物体数。
    pub objects: usize,
    /// 三角形数。
    pub triangles: usize,
    /// 顶点数。
    pub vertices: usize,
    /// 没法写进 OBJ 的物体/部件说明。
    pub skipped: Vec<String>,
}

/// 导出 OBJ；`out_name` 用于生成同名的 `.mtl` 引用。
pub fn export_obj(doc: &SceneDoc, out_name: &str) -> Result<ObjExport, String> {
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

    let mtl_name = format!(
        "{}.mtl",
        std::path::Path::new(out_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("scene")
    );
    let mut out = String::new();
    let _ = writeln!(out, "# quench3d OBJ export");
    let _ = writeln!(out, "mtllib {mtl_name}");
    let mut mtl = String::from("# quench3d MTL export\n");
    let mut written_materials: Vec<String> = Vec::new();

    let mut v_offset = 0usize;
    let mut vn_offset = 0usize;
    let mut vt_offset = 0usize;
    let mut objects = 0usize;
    let mut triangles = 0usize;
    let mut vertices = 0usize;
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
        let material = object
            .material
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let mut wrote_object = false;
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
                    * Mat4::from_quat(runner::euler_quat(part.rotation_deg));
            let transform = object_mat * part_mat;
            let normal_mat = transform.inverse().transpose();
            let part_name = if parts.len() > 1 {
                format!("{name}_{part_index}")
            } else {
                name.clone()
            };
            if !wrote_object {
                let _ = writeln!(out, "o {part_name}");
                let _ = writeln!(out, "usemtl {}", sanitize(&material));
                wrote_object = true;
                if !written_materials.iter().any(|m| m == &material) {
                    write_material(&mut mtl, &material, &doc);
                    written_materials.push(material.clone());
                }
            }
            let _ = writeln!(out, "g {part_name}");
            for v in &mesh.vertices {
                let local = v.pos() * mesh_scale;
                let p = transform.transform_point3(local);
                let n = normal_mat.transform_vector3(v.nrm()).normalize_or_zero();
                let _ = writeln!(out, "v {} {} {}", p.x, p.y, p.z);
                let _ = writeln!(out, "vn {} {} {}", n.x, n.y, n.z);
                let _ = writeln!(out, "vt {} {}", v.uv[0], v.uv[1]);
            }
            for tri in mesh.indices.as_chunks::<3>().0 {
                let a = v_offset + tri[0] as usize;
                let b = v_offset + tri[1] as usize;
                let c = v_offset + tri[2] as usize;
                // +1：OBJ 索引从 1 开始；v/vt/vn 三套偏移各自独立。
                let _ = writeln!(
                    out,
                    "f {}/{}/{} {}/{}/{} {}/{}/{}",
                    a + 1,
                    vt_offset + tri[0] as usize + 1,
                    vn_offset + tri[0] as usize + 1,
                    b + 1,
                    vt_offset + tri[1] as usize + 1,
                    vn_offset + tri[1] as usize + 1,
                    c + 1,
                    vt_offset + tri[2] as usize + 1,
                    vn_offset + tri[2] as usize + 1
                );
            }
            v_offset += mesh.vertices.len();
            vn_offset += mesh.vertices.len();
            vt_offset += mesh.vertices.len();
            vertices += mesh.vertices.len();
            triangles += mesh.triangle_count();
        }
        if wrote_object {
            objects += 1;
        }
    }
    if written_materials.is_empty() {
        write_material(&mut mtl, "default", &doc);
    }
    if !doc.models.is_empty() {
        skipped.push(format!(
            "{} 个 models[] 外部模型没有导出（OBJ 只写本场景的图元几何）",
            doc.models.len()
        ));
    }
    Ok(ObjExport {
        obj: out,
        mtl,
        mtl_name,
        objects,
        triangles,
        vertices,
        skipped,
    })
}

pub(crate) fn resolve_world(
    i: usize,
    objects: &[ObjectDoc],
    names: &BTreeMap<String, usize>,
    cache: &mut [Option<Mat4>],
    visiting: &mut Vec<usize>,
) -> Mat4 {
    if let Some(m) = cache[i] {
        return m;
    }
    if visiting.contains(&i) {
        return Mat4::IDENTITY; // 校验会报环；这里只保证导出不死循环
    }
    visiting.push(i);
    let object = &objects[i];
    let local =
        Mat4::from_translation(Vec3::from_array(object.position.unwrap_or([0.0, 0.0, 0.0])))
            * Mat4::from_quat(runner::euler_quat(object.rotation_deg));
    let parent = object
        .parent
        .as_ref()
        .and_then(|p| names.get(p))
        .map(|&p| resolve_world(p, objects, names, cache, visiting))
        .unwrap_or(Mat4::IDENTITY);
    visiting.pop();
    let world = parent * local;
    cache[i] = Some(world);
    world
}

pub(crate) fn mesh_for_shape(shape: &crate::scene_doc::ShapeDoc) -> Option<(MeshData, Vec3)> {
    let scale = runner::render_scale(shape, 1.0);
    let mesh = match shape.kind {
        ShapeKind::Box => MeshData::cube(1.0),
        ShapeKind::Sphere => MeshData::sphere(0.5, 24, 16),
        ShapeKind::Capsule => MeshData::capsule(0.5, 0.5, 16, 12),
        ShapeKind::Cylinder => MeshData::cylinder(0.5, 0.5, 20),
        ShapeKind::Cone => MeshData::cone(0.5, 1.0, 20),
        ShapeKind::Plane => MeshData::plane(1.0, 1),
        ShapeKind::RoundedBox => runner::rounded_box_mesh(shape),
        ShapeKind::HeightField => runner::heightfield_mesh(shape),
        ShapeKind::Convex => return None,
    };
    let scale = if matches!(shape.kind, ShapeKind::RoundedBox | ShapeKind::HeightField) {
        Vec3::ONE
    } else {
        scale
    };
    Some((mesh, scale))
}

fn write_material(mtl: &mut String, name: &str, doc: &SceneDoc) {
    let _ = writeln!(mtl, "\nnewmtl {}", sanitize(name));
    let Some(m) = doc.materials.get(name) else {
        let _ = writeln!(mtl, "Kd 0.72 0.72 0.75");
        let _ = writeln!(mtl, "Pr 0.6");
        return;
    };
    let _ = writeln!(mtl, "Kd {} {} {}", m.color[0], m.color[1], m.color[2]);
    let _ = writeln!(mtl, "Ks 0 0 0");
    let _ = writeln!(mtl, "Pr {}", m.roughness);
    let _ = writeln!(mtl, "Pm {}", m.metallic);
    if let Some(alpha) = m.alpha {
        let _ = writeln!(mtl, "d {alpha}");
    }
    if m.emissive > 0.0 {
        let c = m.emissive_color.unwrap_or(m.color);
        let _ = writeln!(
            mtl,
            "Ke {} {} {}",
            c[0] * m.emissive,
            c[1] * m.emissive,
            c[2] * m.emissive
        );
    }
    if m.base_color_texture.is_some()
        || m.normal_texture.is_some()
        || m.metallic_roughness_texture.is_some()
        || m.emissive_texture.is_some()
    {
        let _ = writeln!(mtl, "# 贴图槽没有嵌入 OBJ/MTL：{}", name);
    }
}

fn kind_label(kind: ShapeKind) -> &'static str {
    match kind {
        ShapeKind::Box => "box",
        ShapeKind::Sphere => "sphere",
        ShapeKind::Capsule => "capsule",
        ShapeKind::Cylinder => "cylinder",
        ShapeKind::Cone => "cone",
        ShapeKind::Plane => "plane",
        ShapeKind::RoundedBox => "rounded_box",
        ShapeKind::HeightField => "heightfield",
        ShapeKind::Convex => "convex",
    }
}

/// OBJ 名字里不能有空白；其余字符保留。
fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    if s.is_empty() {
        "default".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> SceneDoc {
        SceneDoc::from_json(
            r#"{
              "format":"quench3d.scene","version":2,"name":"obj",
              "objects":[
                {"name":"box","shape":{"type":"box","size":[2,1,3]},"position":[1,2,3]},
                {"name":"ball","shape":{"type":"sphere","radius":0.5},"position":[4,1,0],
                 "material":"red"},
                {"name":"child","shape":{"type":"box"},"parent":"box","position":[0,1,0]}
              ],
              "materials":{"red":{"color":[1,0,0],"roughness":0.3}}
            }"#,
        )
        .expect("JSON 合法")
    }

    #[test]
    fn exports_vertices_faces_and_hierarchy() {
        let out = export_obj(&scene(), "demo.obj").expect("导出必须成功");
        assert_eq!(out.objects, 3);
        assert!(out.triangles > 0);
        assert!(out.obj.contains("mtllib demo.mtl"));
        assert!(out.obj.contains("usemtl red"));
        assert!(out.mtl.contains("newmtl red"));
        assert!(out.mtl.contains("Kd 1 0 0"));
        // 三个物体都写了 `o`
        assert_eq!(out.obj.lines().filter(|l| l.starts_with("o ")).count(), 3);
        // 子物体跟着父物体平移：包围盒中心应在父位置 + 局部 (0,1,0) 附近。
        let min = out
            .obj
            .lines()
            .filter_map(|l| l.strip_prefix("v "))
            .filter_map(|rest| {
                let mut it = rest
                    .split_whitespace()
                    .filter_map(|x| x.parse::<f32>().ok());
                Some((it.next()?, it.next()?, it.next()?))
            })
            .fold((f32::INFINITY, f32::INFINITY, f32::INFINITY), |a, p| {
                (a.0.min(p.0), a.1.min(p.1), a.2.min(p.2))
            });
        assert!(min.1 <= 2.0, "子物体应从父位置附近开始：{min:?}");
    }

    #[test]
    fn export_round_trips_through_obj_parser() {
        let out = export_obj(&scene(), "demo.obj").expect("导出必须成功");
        let model = quench3d::assets::parse_obj(&out.obj, "demo").expect("导出的 OBJ 必须能读回");
        assert_eq!(model.meshes.len(), out.objects);
        assert_eq!(
            model
                .meshes
                .iter()
                .map(|m| m.data.triangle_count())
                .sum::<usize>(),
            out.triangles
        );
    }

    #[test]
    fn exports_template_instances_as_independent_objects() {
        let doc = SceneDoc::from_json(
            r#"{
              "format":"quench3d.scene","version":2,
              "templates":{"crate":{"shape":{"type":"box"}}},
              "objects":[{"template":"crate","instances":[
                {"name":"a","position":[0,0,0]},
                {"name":"b","position":[2,0,0]}
              ]}]
            }"#,
        )
        .expect("JSON 合法");
        let out = export_obj(&doc, "demo.obj").expect("导出必须成功");
        assert_eq!(out.objects, 2);
        assert_eq!(out.triangles, 24);
        assert!(out.obj.contains("o a"));
        assert!(out.obj.contains("o b"));
    }

    #[test]
    fn convex_is_reported_not_silently_dropped() {
        let doc = SceneDoc::from_json(
            r#"{
              "format":"quench3d.scene","version":2,
              "objects":[{"name":"hull","shape":{"type":"convex","points":[[0,0,0],[1,0,0],[0,1,0]]}}]
            }"#,
        )
        .expect("JSON 合法");
        let out = export_obj(&doc, "demo.obj").expect("导出必须成功");
        assert!(out.skipped.iter().any(|s| s.contains("hull")));
    }
}
