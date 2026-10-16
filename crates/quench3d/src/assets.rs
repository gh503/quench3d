//! 资源加载：glTF 2.0（.gltf/.glb）与 Wavefront OBJ/MTL。
//!
//! 加载分两步：
//! 1. 解析到 CPU 端结构体（[`LoadedModel`]），不依赖 GPU；
//! 2. 调用 [`LoadedModel::upload`] 一次性上传到渲染器，得到 `(MeshId, MaterialId)` 列表。
//!
//! ```no_run
//! # fn demo(engine: &mut quench3d::Engine) -> Result<(), Box<dyn std::error::Error>> {
//! use quench3d::assets::load_gltf;
//! let model = load_gltf("assets/DamagedHelmet.gltf")?;
//! for (mesh, material) in model.upload(&mut engine.renderer, &engine.gpu) {
//!     engine.scene.add(mesh, material, quench3d_math::Transform::IDENTITY);
//! }
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use quench3d_math::{Aabb, Transform, Vec2, Vec3, Vec4};
use quench3d_render::{
    GpuContext, ImageData, MaterialDesc, MaterialId, MeshData, MeshId, Renderer,
};

/// 加载错误。
#[derive(Debug)]
pub enum AssetError {
    /// 文件读取失败。
    Io(std::io::Error),
    /// 解析失败。
    Parse(String),
    /// 找不到文件（在若干候选路径中都没有）。
    NotFound(Vec<PathBuf>),
    /// 图像解码失败。
    Image(String),
    /// 当前平台/用法不支持（例如 Web 端读不到文件系统）。
    Unsupported(String),
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetError::Io(e) => write!(f, "读取资源失败: {e}"),
            AssetError::Parse(m) => write!(f, "解析资源失败: {m}"),
            AssetError::NotFound(paths) => {
                write!(f, "找不到资源，已尝试: ")?;
                for (i, p) in paths.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p.display())?;
                }
                Ok(())
            }
            AssetError::Image(m) => write!(f, "图像解码失败: {m}"),
            AssetError::Unsupported(m) => write!(f, "当前环境不支持: {m}"),
        }
    }
}

impl std::error::Error for AssetError {}

impl From<std::io::Error> for AssetError {
    fn from(e: std::io::Error) -> Self {
        AssetError::Io(e)
    }
}

/// CPU 端的一个网格片段。
#[derive(Clone, Debug)]
pub struct LoadedMesh {
    /// 名称。
    pub name: String,
    /// 几何数据。
    pub data: MeshData,
    /// 材质下标（对应 [`LoadedModel::materials`]）。
    pub material: Option<usize>,
}

/// CPU 端的一个材质。
#[derive(Clone, Debug)]
pub struct LoadedMaterial {
    /// 名称。
    pub name: String,
    /// 描述。
    pub desc: MaterialDesc,
    /// 基础色贴图（图像下标）。
    pub base_color_texture: Option<usize>,
    /// 法线贴图（图像下标）。
    pub normal_texture: Option<usize>,
    /// 金属度-粗糙度贴图（图像下标）。
    pub metallic_roughness_texture: Option<usize>,
    /// 自发光贴图（图像下标）。
    pub emissive_texture: Option<usize>,
}

impl Default for LoadedMaterial {
    fn default() -> Self {
        Self {
            name: "default".into(),
            desc: MaterialDesc::new(Vec4::new(1.0, 1.0, 1.0, 1.0)).pbr(0.0, 0.6),
            base_color_texture: None,
            normal_texture: None,
            metallic_roughness_texture: None,
            emissive_texture: None,
        }
    }
}

/// 一个加载好的模型（可复用、可多次上传）。
#[derive(Clone, Debug, Default)]
pub struct LoadedModel {
    /// 名称（通常是文件名）。
    pub name: String,
    /// 网格片段。
    pub meshes: Vec<LoadedMesh>,
    /// 材质。
    pub materials: Vec<LoadedMaterial>,
    /// 图像（RGBA8，未上传到 GPU）。
    pub images: Vec<ImageData>,
}

impl LoadedModel {
    /// 顶点总数。
    pub fn vertex_count(&self) -> usize {
        self.meshes.iter().map(|m| m.data.vertex_count()).sum()
    }

    /// 三角形总数。
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|m| m.data.triangle_count()).sum()
    }

    /// 模型包围盒。
    pub fn bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for m in &self.meshes {
            b.merge(&m.data.aabb());
        }
        b
    }

    /// 把所有网格/材质/贴图上传到渲染器，返回 `(网格, 材质)` 句柄列表。
    pub fn upload(&self, renderer: &mut Renderer, ctx: &GpuContext) -> Vec<(MeshId, MaterialId)> {
        fn upload_image(
            renderer: &mut Renderer,
            ctx: &GpuContext,
            images: &[ImageData],
            index: Option<usize>,
            srgb: bool,
        ) -> Option<quench3d_render::TextureId> {
            index
                .and_then(|k| images.get(k))
                .map(|img| renderer.add_texture(ctx, img, srgb))
        }

        // 1. 材质与贴图
        let mut material_ids: Vec<MaterialId> = Vec::with_capacity(self.materials.len());
        for mat in &self.materials {
            let mut desc = mat.desc.clone();
            desc.base_color_texture =
                upload_image(renderer, ctx, &self.images, mat.base_color_texture, true);
            desc.normal_texture =
                upload_image(renderer, ctx, &self.images, mat.normal_texture, false);
            desc.metallic_roughness_texture = upload_image(
                renderer,
                ctx,
                &self.images,
                mat.metallic_roughness_texture,
                false,
            );
            desc.emissive_texture =
                upload_image(renderer, ctx, &self.images, mat.emissive_texture, true);
            material_ids.push(renderer.add_material_named(ctx, desc, &mat.name));
        }

        // 2. 网格
        let fallback = || MaterialDesc::new(Vec4::new(0.8, 0.8, 0.8, 1.0)).pbr(0.0, 0.6);
        let mut out = Vec::with_capacity(self.meshes.len());
        for mesh in &self.meshes {
            let mesh_id = renderer.add_mesh_named(ctx, &mesh.data, &mesh.name);
            let material_id = mesh
                .material
                .and_then(|i| material_ids.get(i).copied())
                .unwrap_or_else(|| renderer.add_material_named(ctx, fallback(), "fallback"));
            out.push((mesh_id, material_id));
        }
        out
    }

    /// 作为一整块静态几何上传（合并所有网格，只用一个材质），适合无需分材质的模型。
    pub fn upload_merged(
        &self,
        renderer: &mut Renderer,
        ctx: &GpuContext,
        _material: MaterialId,
    ) -> MeshId {
        let mut merged = MeshData::new();
        for m in &self.meshes {
            merged.merge(&m.data);
        }
        renderer.add_mesh_named(ctx, &merged, &self.name)
    }

    /// 把模型注册为场景物体。
    pub fn spawn(
        &self,
        renderer: &mut Renderer,
        ctx: &GpuContext,
        scene: &mut crate::scene::Scene,
        transform: Transform,
    ) -> Vec<usize> {
        self.upload(renderer, ctx)
            .into_iter()
            .enumerate()
            .map(|(i, (mesh, material))| {
                scene.add_named(mesh, material, transform, &format!("{}_{}", self.name, i))
            })
            .collect()
    }
}

/// 解析候选路径：直接路径、`assets/`、`quench3d/assets/`。
pub fn resolve_path(path: impl AsRef<Path>) -> Result<PathBuf, AssetError> {
    let p = path.as_ref();
    let mut candidates = vec![p.to_path_buf()];
    if p.is_relative() {
        candidates.push(Path::new("assets").join(p));
        candidates.push(Path::new("quench3d/assets").join(p));
        candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join(p));
        candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join(p));
    }
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    Err(AssetError::NotFound(candidates))
}

// ---------------------------------------------------------------------------
// glTF
// ---------------------------------------------------------------------------

/// Web（wasm）端**没有文件系统**：给一句能照着做的错误，而不是让它去 `exists()`
/// 然后抛一个"找不到资源，已尝试: chair.glb, assets/chair.glb, …"。
#[cfg(target_arch = "wasm32")]
fn ensure_filesystem_available() -> Result<(), AssetError> {
    Err(AssetError::Unsupported(
        "Web（wasm）端读不到本地文件系统：请由 JS 侧 fetch 之后调用          assets::load_model_from_bytes(名字, 字节)，而不是 load_model(路径)"
            .into(),
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_filesystem_available() -> Result<(), AssetError> {
    Ok(())
}

/// 加载 glTF / GLB，返回可上传的模型。
pub fn load_gltf(path: impl AsRef<Path>) -> Result<LoadedModel, AssetError> {
    ensure_filesystem_available()?;
    let path = resolve_path(path)?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "model".into());
    let (doc, buffers, images) =
        gltf::import(&path).map_err(|e| AssetError::Parse(e.to_string()))?;
    model_from_gltf(name, &doc, &buffers, &images)
}

/// glTF 文档（含已解析的 buffer 与贴图）→ 可上传的模型。
fn model_from_gltf(
    name: String,
    doc: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    images: &[gltf::image::Data],
) -> Result<LoadedModel, AssetError> {
    let mut model = LoadedModel {
        name,
        ..Default::default()
    };

    for img in images {
        let data = gltf_image_to_rgba(img)?;
        model.images.push(data);
    }

    for mat in doc.materials() {
        let pbr = mat.pbr_metallic_roughness();
        let base = pbr.base_color_factor();
        let mut desc = MaterialDesc::new(Vec4::new(base[0], base[1], base[2], base[3]))
            .pbr(pbr.metallic_factor(), pbr.roughness_factor())
            .emissive(
                Vec3::new(
                    mat.emissive_factor()[0],
                    mat.emissive_factor()[1],
                    mat.emissive_factor()[2],
                ),
                1.0,
            );
        if let Some(scale) = mat.normal_texture().map(|t| t.scale()) {
            desc.normal_scale = scale;
        }
        match mat.alpha_mode() {
            gltf::material::AlphaMode::Mask => {
                desc.alpha_cutoff = mat.alpha_cutoff().unwrap_or(0.5)
            }
            // 贴图 alpha 在采样后才知道，颜色因子往往是 1.0 → 必须显式要求混合管线
            gltf::material::AlphaMode::Blend => desc.alpha_blend = true,
            gltf::material::AlphaMode::Opaque => desc.base_color.w = 1.0,
        }
        if mat.double_sided() {
            desc = desc.double_sided();
        }
        model.materials.push(LoadedMaterial {
            name: mat.name().unwrap_or("material").to_string(),
            base_color_texture: pbr.base_color_texture().map(|t| t.texture().index()),
            metallic_roughness_texture: pbr
                .metallic_roughness_texture()
                .map(|t| t.texture().index()),
            normal_texture: mat.normal_texture().map(|t| t.texture().index()),
            emissive_texture: mat.emissive_texture().map(|t| t.texture().index()),
            desc,
        });
    }

    for mesh in doc.meshes() {
        for (pi, prim) in mesh.primitives().enumerate() {
            let reader = prim.reader(|b| buffers.get(b.index()).map(|b| b.0.as_slice()));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(|n| n.collect())
                .unwrap_or_default();
            let uvs: Vec<[f32; 2]> = reader
                .read_tex_coords(0)
                .map(|t| t.into_f32().collect())
                .unwrap_or_default();
            let tangents: Vec<[f32; 4]> = reader
                .read_tangents()
                .map(|t| t.collect())
                .unwrap_or_default();

            let mut data = MeshData::new();
            for (i, p) in positions.iter().enumerate() {
                let mut v = quench3d_render::Vertex::new(
                    Vec3::new(p[0], p[1], p[2]),
                    normals
                        .get(i)
                        .map(|n| Vec3::new(n[0], n[1], n[2]))
                        .unwrap_or(Vec3::Y),
                    uvs.get(i)
                        .map(|t| Vec2::new(t[0], t[1]))
                        .unwrap_or(Vec2::ZERO),
                );
                if let Some(t) = tangents.get(i) {
                    v.tangent = [t[0], t[1], t[2], t[3]];
                }
                data.vertices.push(v);
            }
            match reader.read_indices() {
                Some(indices) => data.indices = indices.into_u32().collect(),
                None => data.indices = (0..positions.len() as u32).collect(),
            }
            if normals.is_empty() {
                data.recompute_smooth_normals();
            }
            if tangents.is_empty() && !uvs.is_empty() {
                data.recompute_tangents();
            }
            model.meshes.push(LoadedMesh {
                name: match mesh.name() {
                    Some(n) if mesh.primitives().len() > 1 => format!("{n}_{pi}"),
                    Some(n) => n.to_string(),
                    None => format!("mesh_{}_{pi}", mesh.index()),
                },
                data,
                material: prim.material().index(),
            });
        }
    }

    if model.materials.is_empty() {
        model.materials.push(LoadedMaterial::default());
    }
    Ok(model)
}

fn gltf_image_to_rgba(img: &gltf::image::Data) -> Result<ImageData, AssetError> {
    use gltf::image::Format;
    let (w, h) = (img.width, img.height);
    let rgba = match img.format {
        Format::R8 => {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for v in &img.pixels {
                out.extend_from_slice(&[*v, *v, *v, 255]);
            }
            out
        }
        Format::R8G8 => {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for px in img.pixels.as_chunks::<2>().0 {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        Format::R8G8B8 => {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for px in img.pixels.as_chunks::<3>().0 {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        Format::R8G8B8A8 => img.pixels.clone(),
        other => {
            return Err(AssetError::Image(format!(
                "暂不支持 16 位 / 特殊格式的 glTF 贴图: {other:?}"
            )))
        }
    };
    ImageData::from_rgba8(w, h, rgba).map_err(|e| AssetError::Image(e.to_string()))
}

// ---------------------------------------------------------------------------
// OBJ / MTL
// ---------------------------------------------------------------------------

/// 按扩展名自动选择解析器加载模型（`.gltf` / `.glb` → glTF，`.obj` → OBJ）。
pub fn load_model(path: impl AsRef<Path>) -> Result<LoadedModel, AssetError> {
    ensure_filesystem_available()?;
    let path = resolve_path(path)?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "gltf" | "glb" => load_gltf(&path),
        "obj" => load_obj(&path),
        other => Err(AssetError::Parse(format!(
            "不支持的模型格式 \"{other}\"（支持 gltf / glb / obj）"
        ))),
    }
}

/// 从**内存字节**加载模型：Web 端、以及任何读不到文件系统的地方走这条路。
///
/// * `name` 只用来判断格式（看扩展名）和模型名，不必是真实存在的文件；
/// * **`.glb` 是自包含的**，任何来源的字节都能直接解析（推荐的 Web 格式）；
///   `.gltf` 若用 `uri` 引用外部 `.bin`/贴图，内存里无从解析 → 报错，请改用 `.glb`
///   或把资源内联成 `data:` URI；
/// * `.obj` 只解析几何：`mtllib` 不会被读取（没有目录可查），材质用缺省值。
///
/// Web 端用法（JS 侧 fetch，字节交给 wasm）：
///
/// ```no_run
/// # fn demo(bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
/// let model = quench3d::assets::load_model_from_bytes("chair.glb", bytes)?;
/// # Ok(()) }
/// ```
pub fn load_model_from_bytes(name: &str, bytes: &[u8]) -> Result<LoadedModel, AssetError> {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "model".into());
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "glb" | "gltf" => {
            let (doc, buffers, images) = gltf::import_slice(bytes).map_err(|e| {
                AssetError::Parse(format!(
                    "解析 glTF 字节流失败: {e}；若 .gltf 引用了外部 .bin/贴图，请改用自包含的 .glb"
                ))
            })?;
            model_from_gltf(stem, &doc, &buffers, &images)
        }
        "obj" => {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| AssetError::Parse(format!("OBJ 字节流不是合法 UTF-8: {e}")))?;
            let mut model = parse_obj(text, &stem)?;
            if model.materials.is_empty() {
                model.materials.push(LoadedMaterial::default());
            }
            Ok(model)
        }
        other => Err(AssetError::Parse(format!(
            "不支持的模型格式 \"{other}\"（支持 glb / gltf / obj）"
        ))),
    }
}

/// 加载 Wavefront OBJ（会尝试解析同目录的 MTL 材质库）。
pub fn load_obj(path: impl AsRef<Path>) -> Result<LoadedModel, AssetError> {
    ensure_filesystem_available()?;
    let path = resolve_path(path)?;
    let text = std::fs::read_to_string(&path)?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "model".into());
    let mut model = parse_obj(&text, &name)?;

    // 尝试加载 MTL
    for mtl_file in mtl_libraries(&text) {
        let mtl_path = path
            .parent()
            .map(|p| p.join(&mtl_file))
            .unwrap_or_else(|| PathBuf::from(&mtl_file));
        if let Ok(mtl_text) = std::fs::read_to_string(&mtl_path) {
            let parsed = parse_mtl(&mtl_text);
            for (i, mat) in model.materials.iter_mut().enumerate() {
                if let Some(m) = parsed.get(&mat.name) {
                    mat.desc = m.desc.clone();
                    mat.base_color_texture = None;
                    // 贴图在下一步处理（需要目录信息）
                    let _ = i;
                }
            }
            // 贴图
            let base = mtl_path.parent().unwrap_or(Path::new("."));
            for mat in model.materials.iter_mut() {
                if let Some(m) = parsed.get(&mat.name) {
                    if let Some(file) = &m.base_color_map {
                        let tex_path = base.join(file);
                        if let Ok(img) = ImageData::from_file(&tex_path) {
                            let index = model.images.len();
                            model.images.push(img);
                            mat.base_color_texture = Some(index);
                        }
                    }
                }
            }
        }
    }
    if model.materials.is_empty() {
        model.materials.push(LoadedMaterial::default());
    }
    Ok(model)
}

fn mtl_libraries(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("mtllib "))
        .flat_map(|rest| rest.split_whitespace().map(|s| s.to_string()))
        .collect()
}

/// 解析 OBJ 文本（按 `o` / `g` 分组）。
pub fn parse_obj(text: &str, name: &str) -> Result<LoadedModel, AssetError> {
    let mut model = LoadedModel {
        name: name.to_string(),
        ..Default::default()
    };
    let mut positions: Vec<Vec3> = Vec::new();
    let mut normals: Vec<Vec3> = Vec::new();
    let mut uvs: Vec<Vec2> = Vec::new();
    let mut has_normals = false;

    // 每个分组独立积累
    struct Group {
        name: String,
        vertices: Vec<quench3d_render::Vertex>,
        indices: Vec<u32>,
        map: HashMap<(i64, i64, i64), u32>,
        material: Option<usize>,
    }
    let mut group = Group {
        name: format!("{name}_0"),
        vertices: Vec::new(),
        indices: Vec::new(),
        map: HashMap::new(),
        material: None,
    };
    let mut groups: Vec<Group> = Vec::new();
    let mut material_names: Vec<String> = Vec::new();
    let mut current_material: Option<usize> = None;

    let resolve = |idx: i64, len: usize| -> usize {
        if idx > 0 {
            (idx - 1) as usize
        } else if idx < 0 {
            (len as i64 + idx) as usize
        } else {
            0
        }
    };

    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(tag) = parts.next() else { continue };
        match tag {
            "v" => {
                let v: Vec<f32> = parts.filter_map(|p| p.parse().ok()).collect();
                if v.len() >= 3 {
                    positions.push(Vec3::new(v[0], v[1], v[2]));
                }
            }
            "vn" => {
                let v: Vec<f32> = parts.filter_map(|p| p.parse().ok()).collect();
                if v.len() >= 3 {
                    normals.push(Vec3::new(v[0], v[1], v[2]));
                    has_normals = true;
                }
            }
            "vt" => {
                let v: Vec<f32> = parts.filter_map(|p| p.parse().ok()).collect();
                if v.len() >= 2 {
                    uvs.push(Vec2::new(v[0], v[1]));
                }
            }
            "o" | "g" => {
                let new_name = parts.next().unwrap_or("group").to_string();
                if !group.vertices.is_empty() || !group.indices.is_empty() {
                    groups.push(std::mem::replace(
                        &mut group,
                        Group {
                            name: new_name,
                            vertices: Vec::new(),
                            indices: Vec::new(),
                            map: HashMap::new(),
                            material: current_material,
                        },
                    ));
                } else {
                    group.name = new_name;
                }
            }
            "usemtl" => {
                let mat_name = parts.next().unwrap_or("default").to_string();
                let index = material_names
                    .iter()
                    .position(|m| m == &mat_name)
                    .unwrap_or_else(|| {
                        material_names.push(mat_name.clone());
                        material_names.len() - 1
                    });
                current_material = Some(index);
                group.material = Some(index);
            }
            "f" => {
                let corners: Vec<&str> = parts.collect();
                if corners.len() < 3 {
                    continue;
                }
                let mut poly: Vec<u32> = Vec::with_capacity(corners.len());
                for c in &corners {
                    let mut it = c.split('/');
                    let vi = it.next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(1);
                    let ti = it.next().and_then(|s| {
                        if s.is_empty() {
                            None
                        } else {
                            s.parse::<i64>().ok()
                        }
                    });
                    let ni = it.next().and_then(|s| {
                        if s.is_empty() {
                            None
                        } else {
                            s.parse::<i64>().ok()
                        }
                    });
                    let pi = resolve(vi, positions.len());
                    let ti_res = ti.map(|i| resolve(i, uvs.len()));
                    let ni_res = ni.map(|i| resolve(i, normals.len()));
                    let key = (vi, ti.unwrap_or(0), ni.unwrap_or(0));
                    let vertex_index = match group.map.get(&key) {
                        Some(i) => *i,
                        None => {
                            let pos = positions.get(pi).copied().unwrap_or(Vec3::ZERO);
                            let nrm = ni_res
                                .and_then(|i| normals.get(i).copied())
                                .unwrap_or(Vec3::Y);
                            let uv = ti_res
                                .and_then(|i| uvs.get(i).copied())
                                .unwrap_or(Vec2::ZERO);
                            let mut v = quench3d_render::Vertex::new(pos, nrm, uv);
                            // OBJ 的 V 轴向下，翻转以匹配常规纹理坐标
                            v.uv[1] = 1.0 - v.uv[1];
                            let i = group.vertices.len() as u32;
                            group.vertices.push(v);
                            group.map.insert(key, i);
                            i
                        }
                    };
                    poly.push(vertex_index);
                }
                // 扇形三角化
                for i in 1..poly.len() - 1 {
                    group.indices.push(poly[0]);
                    group.indices.push(poly[i]);
                    group.indices.push(poly[i + 1]);
                }
            }
            _ => {
                let _ = line_no;
            }
        }
    }
    groups.push(group);

    for g in groups {
        if g.vertices.is_empty() {
            continue;
        }
        let mut data = MeshData::from_parts(g.vertices, g.indices);
        if !has_normals {
            // 文件里没有法线：用面法线，保持低多边形的硬边外观
            data.recompute_flat_normals();
        }
        model.meshes.push(LoadedMesh {
            name: g.name,
            data,
            material: g.material,
        });
    }

    for name in material_names {
        model.materials.push(LoadedMaterial {
            name,
            ..Default::default()
        });
    }
    if model.meshes.is_empty() {
        return Err(AssetError::Parse("OBJ 中没有可用的面数据".into()));
    }
    Ok(model)
}

/// MTL 中的一条材质记录。
#[derive(Clone, Debug, Default)]
pub struct MtlMaterial {
    /// 材质描述。
    pub desc: MaterialDesc,
    /// 基础色贴图文件名。
    pub base_color_map: Option<String>,
}

/// 解析 MTL 文本。
pub fn parse_mtl(text: &str) -> HashMap<String, MtlMaterial> {
    let mut out: HashMap<String, MtlMaterial> = HashMap::new();
    let mut current: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(tag) = parts.next() else { continue };
        match tag {
            "newmtl" => {
                let name = parts.next().unwrap_or("default").to_string();
                out.entry(name.clone()).or_insert_with(|| MtlMaterial {
                    desc: MaterialDesc::new(Vec4::new(0.8, 0.8, 0.8, 1.0)).pbr(0.0, 0.6),
                    base_color_map: None,
                });
                current = Some(name);
            }
            "Kd" => {
                if let Some(name) = &current {
                    let v: Vec<f32> = parts.filter_map(|p| p.parse().ok()).collect();
                    if v.len() >= 3 {
                        if let Some(m) = out.get_mut(name) {
                            let linear = quench3d_render::color::color3_srgb_to_linear(Vec3::new(
                                v[0], v[1], v[2],
                            ));
                            m.desc.base_color = linear.extend(m.desc.base_color.w);
                        }
                    }
                }
            }
            "Ks" => {
                if let Some(name) = &current {
                    let v: Vec<f32> = parts.filter_map(|p| p.parse().ok()).collect();
                    if v.len() >= 3 {
                        let spec = (v[0] + v[1] + v[2]) / 3.0;
                        if let Some(m) = out.get_mut(name) {
                            // 用高光强度粗略映射金属度
                            m.desc.metallic = spec.clamp(0.0, 1.0) * 0.6;
                        }
                    }
                }
            }
            "Ns" => {
                if let Some(name) = &current {
                    let n: f32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(32.0);
                    if let Some(m) = out.get_mut(name) {
                        // 高光指数 -> 粗糙度
                        m.desc.roughness = (1.0 - (n / 1000.0)).clamp(0.05, 1.0);
                    }
                }
            }
            "d" | "Tr" => {
                if let Some(name) = &current {
                    let a: f32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1.0);
                    if let Some(m) = out.get_mut(name) {
                        let alpha = if tag == "Tr" { 1.0 - a } else { a };
                        m.desc.base_color.w = alpha.clamp(0.0, 1.0);
                    }
                }
            }
            "map_Kd" => {
                if let Some(name) = &current {
                    let file = parts.next().map(|s| s.to_string());
                    if let Some(m) = out.get_mut(name) {
                        m.base_color_map = file;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJ: &str = r#"
# 一个立方体的一个面
mtllib cube.mtl
o face
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
vt 0 0
vt 1 0
vt 1 1
vt 0 1
vn 0 0 1
f 1/1/1 2/2/1 3/3/1 4/4/1
"#;

    #[test]
    fn parse_obj_quad_triangulates() {
        let model = parse_obj(OBJ, "cube").unwrap();
        assert_eq!(model.meshes.len(), 1);
        assert_eq!(model.meshes[0].data.triangle_count(), 2);
        assert_eq!(model.meshes[0].data.vertex_count(), 4);
    }

    #[test]
    fn parse_obj_negative_indices() {
        let obj = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\n";
        let model = parse_obj(obj, "tri").unwrap();
        assert_eq!(model.meshes[0].data.triangle_count(), 1);
    }

    #[test]
    fn parse_mtl_basic() {
        let mtl = "newmtl red\nKd 1.0 0.0 0.0\nNs 200\nd 0.5\n";
        let parsed = parse_mtl(mtl);
        let m = parsed.get("red").unwrap();
        assert!((m.desc.base_color.w - 0.5).abs() < 1e-6);
        assert!(m.desc.roughness < 1.0);
        // sRGB 红色转线性后 R 仍为 1，G/B 为 0
        assert!((m.desc.base_color.x - 1.0).abs() < 1e-6);
    }
}

/// 估算一个模型占多少字节（几何 + 贴图）。
///
/// 顶点按 32 字节/个估（位置 12 + 法线 12 + 贴图坐标 8），索引 4 字节/个，
/// 贴图按 RGBA8 实际长度算。**这是估算**：它只用来决定缓存淘汰（[`ModelCache`]），
/// 不用来报内存占用——要精确数字得问分配器。
pub fn approx_bytes(model: &LoadedModel) -> usize {
    let mut bytes = 0usize;
    for m in &model.meshes {
        bytes += m.data.vertices.len() * 32 + m.data.indices.len() * 4;
    }
    for img in &model.images {
        bytes += img.rgba.len();
    }
    bytes
}

/// 模型缓存统计快照（给报告用；`entries`/`bytes` 是当前占用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelCacheStats {
    /// 命中次数。
    pub hits: u64,
    /// 未命中次数（真正解析的次数）。
    pub misses: u64,
    /// 因为预算淘汰的次数。
    pub evictions: u64,
    /// 当前缓存条目数。
    pub entries: usize,
    /// 当前占用（估算字节）。
    pub bytes: usize,
    /// 预算（估算字节）。
    pub budget: usize,
}

struct CacheEntry {
    model: std::sync::Arc<LoadedModel>,
    bytes: usize,
    tick: u64,
    len: u64,
    mtime: Option<std::time::SystemTime>,
}

/// 进程内的模型缓存：同一个文件只解析一次。
///
/// **为什么需要它**：解析是最贵的一步（解压 / 解析 glTF / 解码贴图），而同一次运行里
/// 同一个文件常被读好几遍——建碰撞体、上屏、`info` 各一遍；场景里同一个 `.glb` 被引用
/// 两次（两把相同的椅子）时更是如此。
///
/// 失效判断用 `(文件长度, 修改时间)`：文件被改了就当没缓存，**不会拿旧几何骗人**。
/// 预算按 [`approx_bytes`] 的估算值算，超出时按 LRU 淘汰；单个模型自己就超过预算时
/// 仍然照常返回，只是不留在缓存里（宁可每次重解，也不让预算形同虚设）。
#[derive(Default)]
pub struct ModelCache {
    budget: usize,
    held: usize,
    tick: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
    entries: std::collections::HashMap<String, CacheEntry>,
}

impl ModelCache {
    /// 建一个缓存。`budget_bytes = 0` 表示"不缓存任何东西"（此时它只统计 miss）。
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget: budget_bytes,
            ..Default::default()
        }
    }

    /// 当前预算（估算字节）。
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// 改预算，并立即按 LRU 淘汰到预算以内。
    pub fn set_budget(&mut self, budget_bytes: usize) {
        self.budget = budget_bytes;
        self.evict_to_budget();
    }

    /// 载入模型：命中则返回缓存里的共享引用，否则真正解析并（在预算允许时）留下。
    pub fn load(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<std::sync::Arc<LoadedModel>, AssetError> {
        let path = path.as_ref();
        let key = std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());
        let meta = std::fs::metadata(path).ok();
        let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta.as_ref().and_then(|m| m.modified().ok());
        self.tick += 1;
        if let Some(e) = self.entries.get_mut(&key) {
            if e.len == len && e.mtime == mtime {
                e.tick = self.tick;
                self.hits += 1;
                return Ok(std::sync::Arc::clone(&e.model));
            }
        }
        self.misses += 1;
        let model = std::sync::Arc::new(load_model(path)?);
        let bytes = approx_bytes(&model);
        if self.budget > 0 && bytes <= self.budget {
            self.held += bytes;
            self.entries.insert(
                key,
                CacheEntry {
                    model: std::sync::Arc::clone(&model),
                    bytes,
                    tick: self.tick,
                    len,
                    mtime,
                },
            );
            self.evict_to_budget();
        }
        Ok(model)
    }

    /// 统计快照。
    pub fn stats(&self) -> ModelCacheStats {
        ModelCacheStats {
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            entries: self.entries.len(),
            bytes: self.held,
            budget: self.budget,
        }
    }

    /// 清空缓存（渲染器重建、要腾地方时用）。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.held = 0;
    }

    /// 按 LRU 淘汰到预算以内。
    fn evict_to_budget(&mut self) {
        while self.held > self.budget && !self.entries.is_empty() {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.tick)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(e) = self.entries.remove(&key) {
                self.held = self.held.saturating_sub(e.bytes);
                self.evictions += 1;
            }
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    /// 写一个最小的 OBJ（三个顶点一个面）到临时目录，返回路径。
    fn write_obj(tag: &str, extra: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "quench3d_cache_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join(format!("{tag}.obj"));
        std::fs::write(
            &path,
            format!("v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n{extra}"),
        )
        .expect("写 obj");
        path
    }

    #[test]
    fn cache_hits_misses_and_invalidates_on_file_change() {
        let path = write_obj("hit", "");
        let mut cache = ModelCache::new(64 * 1024 * 1024);
        let first = cache.load(&path).expect("第一次要能加载");
        assert_eq!(first.vertex_count(), 3);
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().hits, 0);
        assert_eq!(cache.stats().entries, 1);

        let second = cache.load(&path).expect("第二次要能加载");
        assert_eq!(cache.stats().misses, 1, "第二次不该再解析");
        assert_eq!(cache.stats().hits, 1);
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "命中的应当是同一个共享对象"
        );

        // 文件变了（长度变了）→ 必须重新解析，不能拿旧几何骗人
        std::fs::write(
            &path,
            "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\nf 1 2 3\nf 2 4 3\n",
        )
        .expect("改文件");
        let third = cache.load(&path).expect("改完还要能加载");
        assert_eq!(cache.stats().misses, 2, "文件变了要重新解析");
        assert_eq!(third.vertex_count(), 4, "拿到的必须是新几何");
    }

    #[test]
    fn cache_respects_budget_evicts_lru_and_can_be_cleared() {
        let a = write_obj("a", "");
        let b = write_obj("b", "");
        // 预算 0：一个都不留（只统计 miss）
        let mut cache = ModelCache::new(0);
        cache.load(&a).expect("加载");
        cache.load(&a).expect("加载");
        assert_eq!(cache.stats().entries, 0, "预算 0 不该留东西");
        assert_eq!(cache.stats().misses, 2);

        // 预算只够一个模型：载入第二个应当淘汰第一个，且不会超预算
        let one = ModelCache::new(usize::MAX);
        let mut probe = one;
        probe.set_budget(usize::MAX);
        let size = {
            let m = probe.load(&a).expect("加载");
            let s = probe.stats().bytes;
            assert_eq!(s, approx_bytes(&m));
            s
        };
        let mut cache = ModelCache::new(size);
        cache.load(&a).expect("加载 a");
        cache.load(&b).expect("加载 b");
        let st = cache.stats();
        assert!(st.bytes <= size, "不该超预算：{st:?}");
        assert_eq!(st.entries, 1, "预算只够一个 → 只剩一个：{st:?}");
        assert_eq!(st.evictions, 1, "应当淘汰了一个：{st:?}");

        cache.clear();
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().bytes, 0);
    }
}
