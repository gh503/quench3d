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

use quench3d_math::{Aabb, Mat4, Quat, Transform, Vec2, Vec3, Vec4};
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

/// 一个 glTF 皮肤（关节 + 逆绑定矩阵）。
///
/// `joints` 是节点下标，`inverse_bind[j]` 与 `joints[j]` 一一对应。没有
/// `inverseBindMatrices` 的资产按规范当作单位矩阵（表示顶点已经预乘过）。
#[derive(Clone, Debug, Default)]
pub struct LoadedSkin {
    /// 关节节点下标。
    pub joints: Vec<usize>,
    /// 每个关节的逆绑定矩阵。
    pub inverse_bind: Vec<Mat4>,
}

/// glTF 的一个 morph target：逐顶点的位置/法线/切线位移。
///
/// 数据已经按节点的绑定姿态世界变换烘过（位置是方向变换，法线/切线只转方向），
/// 与 [`LoadedMesh::data`] 的坐标系一致。缺失的分量用空表表示。
#[derive(Clone, Debug, Default)]
pub struct LoadedMorphTarget {
    /// 逐顶点位置位移。
    pub positions: Vec<[f32; 3]>,
    /// 逐顶点法线位移。
    pub normals: Vec<[f32; 3]>,
    /// 逐顶点切线位移。
    pub tangents: Vec<[f32; 3]>,
}

/// CPU 端的一个网格片段。
#[derive(Clone, Debug)]
pub struct LoadedMesh {
    /// 名称。
    pub name: String,
    /// 几何数据（**已经烘进节点的绑定姿态世界变换**，见 [`LoadedModel::mesh_matrix`]）。
    pub data: MeshData,
    /// 材质下标（对应 [`LoadedModel::materials`]）。
    pub material: Option<usize>,
    /// 这个片段来自哪个节点（[`LoadedModel::nodes`] 的下标）。
    ///
    /// OBJ 与"没被任何节点引用"的兜底片段是 `None` —— 它们没有可动画的节点。
    pub node: Option<usize>,
    /// 该节点**绑定姿态**世界矩阵的逆。
    ///
    /// 几何是按绑定姿态烘过的，所以"动画到 t 时刻"只需要 `world(t) × bind_inverse`
    /// 这一个矩阵；存逆矩阵而不是运行时求逆，是因为求逆在每帧每网格都做一次、还会在
    /// 退化节点（零缩放）上出 NaN。
    pub bind_inverse: Mat4,
    /// 这个片段绑定的皮肤下标（`None` = 普通刚性网格）。
    pub skin: Option<usize>,
    /// 每顶点的 4 个关节下标（`JOINTS_0`；没有皮肤时为空）。
    pub joints: Vec<[u16; 4]>,
    /// 每顶点的 4 个关节权重（`WEIGHTS_0`；没有皮肤时为空）。
    pub weights: Vec<[f32; 4]>,
    /// morph target 位移（按 glTF 顺序；没有时为空）。
    pub morph_targets: Vec<LoadedMorphTarget>,
    /// 资产的默认 morph 权重（glTF `mesh.weights` / node.weights；缺省 0）。
    pub morph_weights: Vec<f32>,
}

impl Default for LoadedMesh {
    fn default() -> Self {
        Self {
            name: String::new(),
            data: MeshData::default(),
            material: None,
            node: None,
            bind_inverse: Mat4::IDENTITY,
            skin: None,
            joints: Vec::new(),
            weights: Vec::new(),
            morph_targets: Vec::new(),
            morph_weights: Vec::new(),
        }
    }
}

/// 一个节点的**局部**变换（绑定姿态）。
#[derive(Clone, Debug)]
pub struct LoadedNode {
    /// 名称（glTF 里可能没有）。
    pub name: String,
    /// 父节点下标。
    pub parent: Option<usize>,
    /// 局部平移。
    pub translation: Vec3,
    /// 局部旋转。
    pub rotation: Quat,
    /// 局部缩放。
    pub scale: Vec3,
}

impl Default for LoadedNode {
    fn default() -> Self {
        Self {
            name: String::new(),
            parent: None,
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl LoadedNode {
    /// 局部的 TRS 矩阵。
    pub fn local_matrix(&self) -> Mat4 {
        Mat4::from_rotation_translation_scale(self.translation, self.rotation, self.scale)
    }
}

/// 一条动画轨道作用的属性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimatedProperty {
    /// 平移。
    Translation,
    /// 旋转（四元数）。
    Rotation,
    /// 缩放。
    Scale,
}

/// 关键帧插值方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// 阶跃（不插值）。
    Step,
    /// 线性（旋转用球面插值）。
    Linear,
    /// 三次样条（Hermite）：TRS 轨道按 glTF 的入/出切线逐分量求值，
    /// 旋转在分量求值后归一化；morph 权重轨道仍只取关键帧值（按线性采样）。
    CubicSpline,
}

/// 一条动画轨道：某个节点的某个属性随时间怎么变。
#[derive(Clone, Debug)]
pub struct AnimationTrack {
    /// 节点下标（[`LoadedModel::nodes`]）。
    pub node: usize,
    /// 属性。
    pub property: AnimatedProperty,
    /// 插值方式。
    pub interpolation: Interpolation,
    /// 关键帧时间（秒，升序）。
    pub times: Vec<f32>,
    /// 每个关键帧的值：平移/缩放取前 3 个分量，旋转是 `xyzw`。
    pub values: Vec<[f32; 4]>,
    /// CUBICSPLINE 的**入切线**（每个关键帧一个）；非三次样条为空。
    ///
    /// 长度与 `times`/`values` 相同；平移/缩放只读前 3 个分量，旋转读 `xyzw`。
    pub in_tangents: Vec<[f32; 4]>,
    /// CUBICSPLINE 的**出切线**；非三次样条为空。
    pub out_tangents: Vec<[f32; 4]>,
}

/// 一条 morph target 权重轨道：某个节点上所有 target 的权重随时间怎么变。
///
/// glTF 的 morph 输出按关键帧展平：每个关键帧有 `target_count` 个权重。
/// 本结构已经按关键帧切成 `values[k]`（长度通常等于 target 数）。
#[derive(Clone, Debug)]
pub struct MorphTrack {
    /// 节点下标（[`LoadedModel::nodes`] 的 `mesh` 节点）。
    pub node: usize,
    /// 插值方式；morph 的 `CubicSpline` 当前仍按线性取关键帧值。
    pub interpolation: Interpolation,
    /// 关键帧时间（秒，升序）。
    pub times: Vec<f32>,
    /// 每个关键帧的完整权重数组。
    pub values: Vec<Vec<f32>>,
}

/// 一条动画的**播放状态**（时间推进、速度、循环）。
///
/// 这是纯 CPU 的：`advance(dt)` 推时间，`mesh_matrices(&model)` 给出"每个网格片段
/// 该用的矩阵"，渲染侧只要把它乘到该片段的基础变换上即可。这样播放逻辑（循环取模、
/// 夹到时长、速度）有单测，不需要显卡，也不必在渲染循环里重复实现一遍。
#[derive(Clone, Debug)]
pub struct ModelAnimationPlayer {
    /// 动画名（必须在 `model.animations` 里存在）。
    pub animation: String,
    /// 时长（秒）。
    pub duration: f32,
    /// 当前时间（秒）。
    pub time: f32,
    /// 播放速度（倍速，必须 > 0）。
    pub speed: f32,
    /// 是否循环。
    pub looping: bool,
    /// 不循环时：是否已经播完（时间夹在时长上）。
    pub finished: bool,
}

impl ModelAnimationPlayer {
    /// 推进 `dt` 秒。
    ///
    /// * 循环：时间对时长取模（`rem_euclid`，负 dt 也不会变成负数）。
    /// * 不循环：夹到时长并置 `finished`。
    /// * 时长 <= 0（空动画）：时间恒为 0、立即 `finished`，不除零。
    pub fn advance(&mut self, dt: f32) -> &mut Self {
        if !dt.is_finite() || self.duration <= 0.0 {
            self.time = 0.0;
            self.finished = self.duration <= 0.0;
            return self;
        }
        let speed = if self.speed.is_finite() && self.speed > 0.0 {
            self.speed
        } else {
            1.0
        };
        self.time += dt * speed;
        if self.looping {
            self.time = self.time.rem_euclid(self.duration);
            self.finished = false;
        } else if self.time >= self.duration {
            self.time = self.duration;
            self.finished = true;
        }
        self
    }

    /// 每个网格片段该用的矩阵（长度 == `model.meshes.len()`）。
    ///
    /// 没动画、采样失败、或片段不属于任何节点时给单位矩阵 —— 也就是"保持烘焙好的样子"。
    pub fn mesh_matrices(&self, model: &LoadedModel) -> Vec<Mat4> {
        match model.sample_animation(&self.animation, self.time) {
            Some(worlds) => model.mesh_matrices_from_worlds(&worlds),
            None => vec![Mat4::IDENTITY; model.meshes.len()],
        }
    }

    /// 当前时刻的**节点世界矩阵**（蒙皮要用；与 [`Self::mesh_matrices`] 同源）。
    ///
    /// 动画名不存在/模型没有节点时返回 `None`；调用方应保持绑定姿态而不是猜。
    pub fn node_worlds(&self, model: &LoadedModel) -> Option<Vec<Mat4>> {
        model.sample_animation(&self.animation, self.time)
    }
}

impl LoadedModel {
    /// 为某条动画建一个播放器；名字不存在返回 `None`（不静默给绑定姿态）。
    pub fn player(&self, name: &str) -> Option<ModelAnimationPlayer> {
        let clip = self.animation(name)?;
        Some(ModelAnimationPlayer {
            animation: clip.name.clone(),
            duration: clip.duration,
            time: 0.0,
            speed: 1.0,
            looping: true,
            finished: false,
        })
    }
}

/// 一条动画（glTF 的一个 `animation`）。
#[derive(Clone, Debug, Default)]
pub struct LoadedAnimation {
    /// 名称（glTF 里可能没有名字）。
    pub name: String,
    /// 时长（秒，= 所有轨道最后一个关键帧时间的最大值）。
    pub duration: f32,
    /// 节点 TRS 轨道。
    pub tracks: Vec<AnimationTrack>,
    /// morph target 权重轨道（没有时为空）。
    pub morph_tracks: Vec<MorphTrack>,
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
    /// 节点（glTF 的场景图；OBJ 模型为空）。
    pub nodes: Vec<LoadedNode>,
    /// 动画（glTF 的 `animations`；没有就是空）。
    pub animations: Vec<LoadedAnimation>,
    /// 皮肤（glTF 的 `skins`；没有就是空）。
    pub skins: Vec<LoadedSkin>,
}

impl LoadedModel {
    /// 绑定姿态下每个节点的世界矩阵。
    pub fn bind_pose(&self) -> Vec<Mat4> {
        let mut out = vec![Mat4::IDENTITY; self.nodes.len()];
        // 按 parent 链递归（深度上限防御坏文件里的环）
        fn walk(nodes: &[LoadedNode], i: usize, out: &mut Vec<Mat4>, depth: u32) -> Mat4 {
            if depth > 64 {
                return Mat4::IDENTITY;
            }
            let local = nodes[i].local_matrix();
            let m = match nodes[i].parent {
                Some(p) => walk(nodes, p, out, depth + 1) * local,
                None => local,
            };
            out[i] = m;
            m
        }
        for i in 0..self.nodes.len() {
            walk(&self.nodes, i, &mut out, 0);
        }
        out
    }

    /// 动画名列表（按 glTF 里的顺序）。
    pub fn animation_names(&self) -> Vec<&str> {
        self.animations.iter().map(|a| a.name.as_str()).collect()
    }

    /// 按名字找动画（glTF 里没名字时用 `animation_{下标}`）。
    pub fn animation(&self, name: &str) -> Option<&LoadedAnimation> {
        self.animations.iter().find(|a| a.name == name)
    }

    /// 采样某条动画在 `time` 秒时的**节点世界矩阵**；名字不存在返回 `None`。
    ///
    /// 时间会被夹到 `[0, duration]`（循环是调用方的事：`time.rem_euclid(duration)`）。
    /// 没被任何轨道覆盖的节点保持绑定姿态 —— 这正是"只动一部分关节"的资产需要的。
    pub fn sample_animation(&self, name: &str, time: f32) -> Option<Vec<Mat4>> {
        let locals = self.sample_animation_locals(name, time)?;
        Some(self.worlds_from_locals(&locals))
    }

    /// 采样某条动画在 `time` 秒时的 **morph 权重**（每个节点一项）。
    ///
    /// 返回 `(node, weights)` 列表；`None` = 这条动画没有 morph 轨道。权重数组
    /// 长度按资产里的 target 数，缺项由调用方按 0 处理。**只采样主 clip**；
    /// 双 clip 混合时的 morph 权重目前不做插值（见场景层的边界说明）。
    pub fn morph_weights_at(&self, name: &str, time: f32) -> Option<Vec<(usize, Vec<f32>)>> {
        let clip = self.animation(name)?;
        if clip.morph_tracks.is_empty() {
            return None;
        }
        let t = if self.nodes.is_empty() {
            0.0
        } else {
            time.clamp(0.0, clip.duration.max(0.0))
        };
        Some(
            clip.morph_tracks
                .iter()
                .filter_map(|track| {
                    sample_morph_track(track, t).map(|weights| (track.node, weights))
                })
                .collect(),
        )
    }

    /// 双 clip 的 morph 权重混合：逐节点按 `blend`（0 = 主 clip，1 = 第二 clip）插值。
    ///
    /// * 两条 clip 都有该节点的轨道：逐 target 线性插值（长度不足按 0）。
    /// * 只有主 clip 有：用主 clip（与单 clip 行为一致）。
    /// * 只有第二 clip 有：按 `blend` 缩放（`blend = 0` 时不会凭空把第二 clip 的形变加上去）。
    /// * 两条都没有：不返回该节点，调用方保持资产默认权重。
    ///
    /// `blend` 会被夹到 0..=1；时间非法时由 [`Self::morph_weights_at`] 夹到片段范围。
    pub fn blended_morph_weights_at(
        &self,
        primary: &str,
        secondary: &str,
        blend: f32,
        primary_time: f32,
        secondary_time: f32,
    ) -> Option<Vec<(usize, Vec<f32>)>> {
        let a = self.morph_weights_at(primary, primary_time);
        let b = self.morph_weights_at(secondary, secondary_time);
        if a.is_none() && b.is_none() {
            return None;
        }
        let blend = if blend.is_finite() {
            blend.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let a = a.unwrap_or_default();
        let b = b.unwrap_or_default();
        let mut out: Vec<(usize, Vec<f32>)> = Vec::new();
        for (node, wa) in &a {
            let wb = b.iter().find(|(n, _)| n == node).map(|(_, w)| w);
            let weights = match wb {
                Some(wb) => {
                    let len = wa.len().max(wb.len());
                    let mut w = Vec::with_capacity(len);
                    for k in 0..len {
                        let x = wa.get(k).copied().unwrap_or(0.0);
                        let y = wb.get(k).copied().unwrap_or(0.0);
                        w.push(x * (1.0 - blend) + y * blend);
                    }
                    w
                }
                None => wa.clone(),
            };
            out.push((*node, weights));
        }
        for (node, wb) in &b {
            if a.iter().any(|(n, _)| n == node) {
                continue;
            }
            out.push((*node, wb.iter().map(|x| x * blend).collect::<Vec<f32>>()));
        }
        Some(out)
    }

    /// 采样某条动画在 `time` 秒时的**局部**变换（每个节点一项）。
    ///
    /// 这是 [`Self::sample_animation`] 的第一步；单独抽出来是给动画混合用：
    /// 两条 clip 先各自得到局部 TRS，再对平移/缩放做 lerp、对旋转做 slerp，
    /// 最后只做一次父子累乘。直接混世界矩阵会让旋转/缩放以错误的方式插值。
    fn sample_animation_locals(&self, name: &str, time: f32) -> Option<Vec<(Vec3, Quat, Vec3)>> {
        let clip = self.animation(name)?;
        let t = if self.nodes.is_empty() {
            0.0
        } else {
            time.clamp(0.0, clip.duration.max(0.0))
        };
        // 从绑定姿态的局部变换出发，把轨道采样覆盖上去
        let mut locals: Vec<(Vec3, Quat, Vec3)> = self
            .nodes
            .iter()
            .map(|n| (n.translation, n.rotation, n.scale))
            .collect();
        for track in &clip.tracks {
            let Some((_, _, _)) = locals.get(track.node) else {
                continue;
            };
            let value = sample_track(track, t);
            let slot = &mut locals[track.node];
            match track.property {
                AnimatedProperty::Translation => slot.0 = Vec3::new(value[0], value[1], value[2]),
                AnimatedProperty::Scale => slot.2 = Vec3::new(value[0], value[1], value[2]),
                AnimatedProperty::Rotation => {
                    let q = Quat::from_xyzw(value[0], value[1], value[2], value[3]);
                    // 归一化：关键帧里的四元数理论上已是单位四元数，但外部工具会写坏
                    slot.1 = q.normalize_or_identity();
                }
            }
        }
        Some(locals)
    }

    /// 局部变换 → 节点世界矩阵（按 parent 链；父在子之前算好）。
    fn worlds_from_locals(&self, locals: &[(Vec3, Quat, Vec3)]) -> Vec<Mat4> {
        let mut worlds: Vec<Mat4> = vec![Mat4::IDENTITY; self.nodes.len()];
        let mut done: Vec<bool> = vec![false; self.nodes.len()];
        for i in 0..self.nodes.len() {
            resolve(i, &self.nodes, locals, &mut worlds, &mut done, 0);
        }
        worlds
    }

    /// 两条动画按 `weight` 混合后的节点世界矩阵；任一动画名不存在返回 `None`。
    ///
    /// `weight` 是**后一条**（secondary）的权重，夹到 `0..=1`。0 = 完全是第一条，
    /// 1 = 完全是第二条；中间在局部 TRS 上插值（旋转用 slerp），再统一累乘父子链。
    /// 两条 clip 的关节集合可以不同：没被覆盖的节点各自保持绑定姿态。
    pub fn blended_node_worlds(
        &self,
        primary: &ModelAnimationPlayer,
        secondary: &ModelAnimationPlayer,
        weight: f32,
    ) -> Option<Vec<Mat4>> {
        let a = self.sample_animation_locals(&primary.animation, primary.time)?;
        let b = self.sample_animation_locals(&secondary.animation, secondary.time)?;
        let w = if weight.is_finite() {
            weight.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if w <= 0.0 {
            return Some(self.worlds_from_locals(&a));
        }
        if w >= 1.0 {
            return Some(self.worlds_from_locals(&b));
        }
        let locals: Vec<(Vec3, Quat, Vec3)> = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| {
                (
                    x.0 + (y.0 - x.0) * w,
                    x.1.slerp(y.1, w),
                    x.2 + (y.2 - x.2) * w,
                )
            })
            .collect();
        Some(self.worlds_from_locals(&locals))
    }

    /// 画某个网格片段该用的**刚性**矩阵：`world(t) × bind_inverse`。
    ///
    /// * 绑定姿态（或没有动画）时它**恰好是单位矩阵** —— 静态路径逐位不变。
    /// * 几何是按绑定姿态烘过的，所以乘上"当前世界 × 绑定世界⁻¹"就是动画后的位置。
    /// * 非节点片段（OBJ、兜底）永远返回单位矩阵。
    /// * **蒙皮片段也返回单位矩阵**：它的顶点由 [`Self::skinned_mesh`] 按关节矩阵
    ///   直接算到骨架空间（再乘出生时的 `base` 摆放），若这里再乘一次节点动画就是
    ///   动了两遍。蒙皮网格由 `advance_animations_with_renderer` 更新顶点。
    pub fn mesh_matrix(&self, mesh_index: usize, node_worlds: &[Mat4]) -> Mat4 {
        let Some(mesh) = self.meshes.get(mesh_index) else {
            return Mat4::IDENTITY;
        };
        if mesh.skin.is_some() {
            return Mat4::IDENTITY;
        }
        let Some(node) = mesh.node else {
            return Mat4::IDENTITY;
        };
        match node_worlds.get(node) {
            Some(world) => *world * mesh.bind_inverse,
            None => Mat4::IDENTITY,
        }
    }

    /// 给定节点世界矩阵，一次性算出所有网格片段的**刚性**矩阵。
    ///
    /// 与 [`Self::mesh_matrix`] 同一条规则（蒙皮片段为单位矩阵）；单独提供是因为
    /// 混合动画已经算过一份世界矩阵，不该再为每个片段重新采样两遍。
    pub fn mesh_matrices_from_worlds(&self, node_worlds: &[Mat4]) -> Vec<Mat4> {
        (0..self.meshes.len())
            .map(|i| self.mesh_matrix(i, node_worlds))
            .collect()
    }

    /// 这个模型是否含可蒙皮网格。
    pub fn has_skinning(&self) -> bool {
        self.meshes.iter().any(|m| m.skin.is_some())
    }

    /// 这个模型是否含 morph target。
    pub fn has_morph_targets(&self) -> bool {
        self.meshes.iter().any(|m| !m.morph_targets.is_empty())
    }

    /// 按 `weights` 计算某个片段的 **morph 变形**网格（绑定姿态坐标系）。
    ///
    /// `weights = None` 用资产里的默认权重；显式数组按 target 下标对应，缺项按 0。
    /// 没有 morph target 的片段返回原始几何的克隆（不是 `None`，调用方可以无条件取）。
    /// 位移只加在位置/法线/切线上，索引与拓扑不变；法线/切线最后重新归一化。
    pub fn morphed_mesh(&self, mesh_index: usize, weights: Option<&[f32]>) -> Option<MeshData> {
        let mesh = self.meshes.get(mesh_index)?;
        let mut out = mesh.data.clone();
        if mesh.morph_targets.is_empty() {
            return Some(out);
        }
        for (ti, target) in mesh.morph_targets.iter().enumerate() {
            let weight = match weights {
                Some(w) => w.get(ti).copied().unwrap_or(0.0),
                None => mesh.morph_weights.get(ti).copied().unwrap_or(0.0),
            };
            if !weight.is_finite() || weight == 0.0 {
                continue;
            }
            for (vi, vertex) in out.vertices.iter_mut().enumerate() {
                if let Some(d) = target.positions.get(vi) {
                    vertex.position[0] += d[0] * weight;
                    vertex.position[1] += d[1] * weight;
                    vertex.position[2] += d[2] * weight;
                }
                if let Some(d) = target.normals.get(vi) {
                    vertex.normal[0] += d[0] * weight;
                    vertex.normal[1] += d[1] * weight;
                    vertex.normal[2] += d[2] * weight;
                }
                if let Some(d) = target.tangents.get(vi) {
                    vertex.tangent[0] += d[0] * weight;
                    vertex.tangent[1] += d[1] * weight;
                    vertex.tangent[2] += d[2] * weight;
                }
            }
        }
        for vertex in &mut out.vertices {
            let n = Vec3::from_array(vertex.normal).normalize_or_zero();
            if n != Vec3::ZERO {
                vertex.normal = n.to_array();
            }
            let t = Vec3::from_array([vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]])
                .normalize_or_zero();
            if t != Vec3::ZERO {
                vertex.tangent = [t.x, t.y, t.z, vertex.tangent[3]];
            }
        }
        Some(out)
    }

    /// 先 morph 再蒙皮；没有蒙皮时返回 morph 后的绑定姿态网格。
    ///
    /// 这是"morph target + skin"同时存在时的正确顺序：位移定义在绑定姿态，
    /// 必须先进骨架空间再乘关节矩阵，不能反过来。
    pub fn deformed_mesh(
        &self,
        mesh_index: usize,
        node_worlds: &[Mat4],
        weights: Option<&[f32]>,
    ) -> Option<MeshData> {
        let mesh = self.meshes.get(mesh_index)?;
        let base = self.morphed_mesh(mesh_index, weights)?;
        if mesh.skin.is_some() {
            self.skin_data(mesh, &base, node_worlds)
        } else {
            Some(base)
        }
    }

    /// 按当前节点世界矩阵计算某个蒙皮片段的**变形后**网格。
    ///
    /// 返回 `None` 表示这个片段不是蒙皮网格（调用方继续走刚性矩阵路径）。
    /// 几何在装载时已经烘进绑定姿态的节点世界变换，所以每个顶点的最终变换是
    /// `Σ w_j · world(joint_j) · inverseBind_j · bind_inverse(mesh)`；
    /// 权重和为 0 的坏顶点保持绑定姿态，不产生 NaN。
    ///
    /// **CPU 蒙皮**：法线/切线用每个关节矩阵的线性部分变换后归一化（非均匀缩放下
    /// 不是严格的逆转置，和装载时烘节点变换的取舍一致），顶点位置是精确的。
    pub fn skinned_mesh(&self, mesh_index: usize, node_worlds: &[Mat4]) -> Option<MeshData> {
        let mesh = self.meshes.get(mesh_index)?;
        mesh.skin?;
        let base = self.morphed_mesh(mesh_index, None)?;
        self.skin_data(mesh, &base, node_worlds)
    }

    /// 蒙皮内核算子：`base` 是绑定姿态（可已做 morph）的网格。
    fn skin_data(
        &self,
        mesh: &LoadedMesh,
        base: &MeshData,
        node_worlds: &[Mat4],
    ) -> Option<MeshData> {
        let skin_index = mesh.skin?;
        let skin = self.skins.get(skin_index)?;
        if mesh.joints.len() != base.vertices.len() || mesh.weights.len() != base.vertices.len() {
            return None;
        }
        // 每个关节的 `world × inverseBind × bindInverse` 只算一次。
        let mut joint_mats = Vec::with_capacity(skin.joints.len());
        for (j, &node) in skin.joints.iter().enumerate() {
            let world = node_worlds.get(node).copied().unwrap_or(Mat4::IDENTITY);
            let inverse_bind = skin.inverse_bind.get(j).copied().unwrap_or(Mat4::IDENTITY);
            joint_mats.push(world * inverse_bind * mesh.bind_inverse);
        }
        let mut out = base.clone();
        for (vi, vertex) in out.vertices.iter_mut().enumerate() {
            let p = vertex.pos();
            let n = vertex.nrm();
            let t = Vec3::from_array([vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]]);
            let mut pos = Vec3::ZERO;
            let mut nrm = Vec3::ZERO;
            let mut tan = Vec3::ZERO;
            let mut total = 0.0f32;
            for k in 0..4 {
                let weight = mesh.weights[vi][k];
                if !weight.is_finite() || weight <= 0.0 {
                    continue;
                }
                let joint = mesh.joints[vi][k] as usize;
                let Some(m) = joint_mats.get(joint) else {
                    continue;
                };
                pos += m.transform_point3(p) * weight;
                nrm += m.to_mat3().mul_vec3(n) * weight;
                tan += m.to_mat3().mul_vec3(t) * weight;
                total += weight;
            }
            if total > 1e-8 {
                let inv = 1.0 / total;
                vertex.position = (pos * inv).to_array();
                let n = nrm.normalize_or_zero();
                vertex.normal = if n == Vec3::ZERO {
                    Vec3::Y.to_array()
                } else {
                    n.to_array()
                };
                let t = tan.normalize_or_zero();
                vertex.tangent = [t.x, t.y, t.z, vertex.tangent[3]];
            }
        }
        Some(out)
    }

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
        for (i, mesh) in self.meshes.iter().enumerate() {
            // 上传的是"默认 morph 权重"下的几何；场景覆盖权重后再调
            // `update_mesh_vertices`，与动画路径共用同一个 MeshId。
            let data = self
                .morphed_mesh(i, None)
                .unwrap_or_else(|| mesh.data.clone());
            let mesh_id = renderer.add_mesh_named(ctx, &data, &mesh.name);
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
    ///
    /// 返回 [`SpawnedModel`] 而不是裸 `Vec<usize>`：动画要把 `world(t) × bind_inverse`
    /// 乘到**每个片段自己的基础变换**上，所以调用方必须同时拿到"片段 → 物体下标"和
    /// "片段 → 出生变换"两份映射。只给一份的话，动画只能靠猜或另抄一份映射。
    pub fn spawn(
        &self,
        renderer: &mut Renderer,
        ctx: &GpuContext,
        scene: &mut crate::scene::Scene,
        transform: Transform,
    ) -> SpawnedModel {
        let objects: Vec<usize> = self
            .upload(renderer, ctx)
            .into_iter()
            .enumerate()
            .map(|(i, (mesh, material))| {
                scene.add_named(mesh, material, transform, &format!("{}_{}", self.name, i))
            })
            .collect();
        SpawnedModel {
            // 所有片段出生时用同一个变换（模型整体摆位），动画只在这个基础上叠加。
            base: vec![transform; objects.len()],
            objects,
        }
    }
}

/// `LoadedModel::spawn` 的结果：片段 → 场景物体的两份**同序**映射。
///
/// 下标就是 `LoadedModel::meshes` 的下标；`objects[i]` 是那个片段在场景里的物体，
/// `base[i]` 是它出生时的变换（`Scene::apply_animation` 的基准）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpawnedModel {
    /// 每个网格片段对应的场景物体下标。
    pub objects: Vec<usize>,
    /// 每个网格片段出生时的基础变换。
    pub base: Vec<Transform>,
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
/// glTF 的 4×4 矩阵是**列主序**的 `[[f32; 4]; 4]`（`m[列][行]`）。
type Matrix = [[f32; 4]; 4];

const IDENTITY: Matrix = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn mat_mul(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [[0.0f32; 4]; 4];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, cell) in col.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

fn transform_point(m: &Matrix, p: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for (r, cell) in out.iter_mut().enumerate() {
        *cell = m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r];
    }
    out
}

fn transform_dir(m: &Matrix, d: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for (r, cell) in out.iter_mut().enumerate() {
        *cell = m[0][r] * d[0] + m[1][r] * d[1] + m[2][r] * d[2];
    }
    out
}

/// 局部 → 世界：递归解析父节点（`done` 防重复，深度上限防环）。
fn resolve(
    i: usize,
    nodes: &[LoadedNode],
    locals: &[(Vec3, Quat, Vec3)],
    worlds: &mut [Mat4],
    done: &mut [bool],
    depth: u32,
) {
    if done[i] || depth > 64 {
        return;
    }
    let (t, r, s) = locals[i];
    let local = Mat4::from_rotation_translation_scale(t, r, s);
    let world = match nodes[i].parent {
        Some(p) => {
            resolve(p, nodes, locals, worlds, done, depth + 1);
            worlds[p] * local
        }
        None => local,
    };
    worlds[i] = world;
    done[i] = true;
}

/// 采样一条轨道在 `t` 时刻的值（4 分量；平移/缩放只用前 3 个）。
fn sample_morph_track(track: &MorphTrack, t: f32) -> Option<Vec<f32>> {
    let n = track.times.len().min(track.values.len());
    if n == 0 {
        return None;
    }
    if n == 1 || t <= track.times[0] {
        return Some(track.values[0].clone());
    }
    if t >= track.times[n - 1] {
        return Some(track.values[n - 1].clone());
    }
    let mut lo = 0usize;
    let mut hi = n - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if track.times[mid] <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let t0 = track.times[lo];
    let t1 = track.times[hi];
    let a = &track.values[lo];
    let b = &track.values[hi];
    if track.interpolation == Interpolation::Step || (t1 - t0).abs() < 1e-9 {
        return Some(a.clone());
    }
    let u = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
    let len = a.len().min(b.len());
    let mut out = Vec::with_capacity(len);
    for k in 0..len {
        out.push(a[k] + (b[k] - a[k]) * u);
    }
    Some(out)
}

fn sample_track(track: &AnimationTrack, t: f32) -> [f32; 4] {
    let n = track.times.len().min(track.values.len());
    if n == 0 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    if n == 1 || t <= track.times[0] {
        return track.values[0];
    }
    if t >= track.times[n - 1] {
        return track.values[n - 1];
    }
    // 二分找区间 [i, i+1)
    let mut lo = 0usize;
    let mut hi = n - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if track.times[mid] <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let t0 = track.times[lo];
    let t1 = track.times[hi];
    let a = track.values[lo];
    let b = track.values[hi];
    if track.interpolation == Interpolation::Step || (t1 - t0).abs() < 1e-9 {
        return a;
    }
    let u = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
    if track.interpolation == Interpolation::CubicSpline
        && track.in_tangents.len() >= n
        && track.out_tangents.len() >= n
    {
        // glTF 的切线是"每秒"的导数；乘区间长度换成对 u 的导数。
        let td = t1 - t0;
        let m0 = track.out_tangents[lo];
        let m1 = track.in_tangents[hi];
        let u2 = u * u;
        let u3 = u2 * u;
        let h00 = 2.0 * u3 - 3.0 * u2 + 1.0;
        let h10 = u3 - 2.0 * u2 + u;
        let h01 = -2.0 * u3 + 3.0 * u2;
        let h11 = u3 - u2;
        let mut out = [0.0f32; 4];
        for k in 0..4 {
            out[k] = h00 * a[k] + h10 * td * m0[k] + h01 * b[k] + h11 * td * m1[k];
        }
        if track.property == AnimatedProperty::Rotation {
            let q = Quat::from_xyzw(out[0], out[1], out[2], out[3]);
            if q.length_squared() > 1e-12 {
                let q = q.normalize_or_identity();
                return [q.x, q.y, q.z, q.w];
            }
            return a;
        }
        return out;
    }
    if track.property == AnimatedProperty::Rotation {
        let qa = Quat::from_xyzw(a[0], a[1], a[2], a[3]);
        let qb = Quat::from_xyzw(b[0], b[1], b[2], b[3]);
        let q = if qa.length_squared() > 1e-12 && qb.length_squared() > 1e-12 {
            qa.normalize_or_identity()
                .slerp(qb.normalize_or_identity(), u)
        } else {
            qa
        };
        [q.x, q.y, q.z, q.w]
    } else {
        let mut out = [0.0f32; 4];
        for k in 0..4 {
            out[k] = a[k] + (b[k] - a[k]) * u;
        }
        out
    }
}

/// 每个节点的**世界**变换（含父子累乘）。
///
/// glTF 没有 `parent()`，只能从 `scenes` 的根节点往下走；没被任何场景引用的节点
/// （有些导出器会留下这种孤儿）按"自己的局部变换 = 世界变换"处理，免得它们消失。
/// 深度上限是防御坏文件里的环 —— 递归爆栈是不可恢复的。
fn node_world_matrices(doc: &gltf::Document) -> Vec<Option<Matrix>> {
    fn walk(node: &gltf::Node, parent: &Matrix, out: &mut [Option<Matrix>], depth: u32) {
        if depth > 64 {
            return;
        }
        let world = mat_mul(parent, &node.transform().matrix());
        out[node.index()] = Some(world);
        for child in node.children() {
            walk(&child, &world, out, depth + 1);
        }
    }
    let mut out: Vec<Option<Matrix>> = vec![None; doc.nodes().len()];
    for scene in doc.scenes() {
        for node in scene.nodes() {
            walk(&node, &IDENTITY, &mut out, 0);
        }
    }
    for node in doc.nodes() {
        if out[node.index()].is_none() {
            walk(&node, &IDENTITY, &mut out, 0);
        }
    }
    out
}

/// 每个节点的父节点下标（glTF 没有 `parent()`，只能从 `children()` 反推）。
fn node_parents(doc: &gltf::Document) -> Vec<Option<usize>> {
    let mut parents: Vec<Option<usize>> = vec![None; doc.nodes().len()];
    for node in doc.nodes() {
        for child in node.children() {
            parents[child.index()] = Some(node.index());
        }
    }
    parents
}

/// 列主序 `[[f32; 4]; 4]` → [`Mat4`]。
fn matrix_to_mat4(m: &Matrix) -> Mat4 {
    let mut a = [0.0f32; 16];
    for (c, col) in m.iter().enumerate() {
        a[c * 4..c * 4 + 4].copy_from_slice(col);
    }
    Mat4::from_cols_array(&a)
}

/// 网格片段的名字：优先用引用它的**节点**名（同一个网格常被多个节点引用，
/// 名字带上节点才分得清"左轮"和"右轮"），其次用网格名，最后用下标。
fn mesh_label(doc: &gltf::Document, mesh_index: usize) -> String {
    for node in doc.nodes() {
        if let Some(mesh) = node.mesh() {
            if mesh.index() == mesh_index {
                return match node.name() {
                    Some(n) => n.to_string(),
                    None => format!("node_{}", node.index()),
                };
            }
        }
    }
    doc.meshes()
        .nth(mesh_index)
        .and_then(|m| m.name().map(|s| s.to_string()))
        .unwrap_or_else(|| format!("mesh_{mesh_index}"))
}

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

    // 皮肤：关节节点下标 + 逆绑定矩阵。逆矩阵缺失时按规范补单位矩阵。
    for skin in doc.skins() {
        let joints: Vec<usize> = skin.joints().map(|n| n.index()).collect();
        let inverse_bind: Vec<Mat4> = skin
            .reader(|b| buffers.get(b.index()).map(|b| b.0.as_slice()))
            .read_inverse_bind_matrices()
            .map(|it| it.map(|m| matrix_to_mat4(&m)).collect())
            .unwrap_or_default();
        let mut inverse_bind = inverse_bind;
        inverse_bind.resize(joints.len(), Mat4::IDENTITY);
        model.skins.push(LoadedSkin {
            joints,
            inverse_bind,
        });
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

    // 【曾经是 bug】这里原来直接遍历 `doc.meshes()`，**完全不看节点**：节点上的
    // 平移/旋转/缩放被丢掉，于是多节点模型的所有部件都堆在原点（单网格的
    // DamagedHelmet 看不出来，两个轮子的车一眼就看出来了），同一个网格被多个节点
    // 引用时也只出现一次。现在按节点走，把世界变换烘进顶点。
    // 这也是 gap #23（动画/蒙皮）的前置：动画就是对同一批节点改变换。
    let worlds = node_world_matrices(doc);
    let mut used_meshes: Vec<bool> = vec![false; doc.meshes().len()];
    // ── 节点表（场景图）与动画 ──────────────────────────────────────────
    // 节点表是动画的地基：动画改的就是这些节点的局部 TRS。
    let parents = node_parents(doc);
    for node in doc.nodes() {
        let (t, r, s) = node.transform().decomposed();
        let rot = Quat::from_xyzw(r[0], r[1], r[2], r[3]);
        model.nodes.push(LoadedNode {
            name: node
                .name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("node_{}", node.index())),
            parent: parents[node.index()],
            translation: Vec3::new(t[0], t[1], t[2]),
            rotation: rot.normalize_or_identity(),
            scale: Vec3::new(s[0], s[1], s[2]),
        });
    }
    for (ai, anim) in doc.animations().enumerate() {
        let mut clip = LoadedAnimation {
            name: anim
                .name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("animation_{ai}")),
            duration: 0.0,
            tracks: Vec::new(),
            morph_tracks: Vec::new(),
        };
        for channel in anim.channels() {
            // 在这一版 gltf crate 里 `target().node()` 直接返回节点（恒有）
            let target_node = channel.target().node();
            let target_node_index = target_node.index();
            let reader = channel.reader(|b| buffers.get(b.index()).map(|b| b.0.as_slice()));
            let Some(times) = reader.read_inputs() else {
                continue;
            };
            let times: Vec<f32> = times.collect();
            let Some(outputs) = reader.read_outputs() else {
                continue;
            };
            let interpolation = match channel.sampler().interpolation() {
                gltf::animation::Interpolation::Step => Interpolation::Step,
                gltf::animation::Interpolation::Linear => Interpolation::Linear,
                gltf::animation::Interpolation::CubicSpline => Interpolation::CubicSpline,
            };
            // CUBICSPLINE 的每个关键帧有 3 个值（入切线、值、出切线）。
            let cubic = interpolation == Interpolation::CubicSpline;
            // morph 输出是展平的标量数组，必须先知道这个节点的 target 数才能切帧。
            let morph_target_count = target_node
                .mesh()
                .and_then(|m| m.primitives().next())
                .map(|p| p.morph_targets().count())
                .unwrap_or(0);
            let (property, values): (AnimatedProperty, Vec<[f32; 4]>) = match outputs {
                gltf::animation::util::ReadOutputs::Translations(it) => (
                    AnimatedProperty::Translation,
                    it.map(|v| [v[0], v[1], v[2], 0.0]).collect(),
                ),
                gltf::animation::util::ReadOutputs::Scales(it) => (
                    AnimatedProperty::Scale,
                    it.map(|v| [v[0], v[1], v[2], 0.0]).collect(),
                ),
                gltf::animation::util::ReadOutputs::Rotations(it) => (
                    AnimatedProperty::Rotation,
                    it.into_f32().map(|v| [v[0], v[1], v[2], v[3]]).collect(),
                ),
                gltf::animation::util::ReadOutputs::MorphTargetWeights(it) => {
                    if morph_target_count == 0 {
                        continue;
                    }
                    let flat: Vec<f32> = it.into_f32().collect();
                    let stride = morph_target_count;
                    let values: Vec<Vec<f32>> = if cubic {
                        // CUBICSPLINE：每个关键帧是 [入切线 | 值 | 出切线]，
                        // 每个块 target_count 个权重；只取中间的值块。
                        flat.chunks(stride * 3)
                            .filter_map(|chunk| {
                                let value = chunk.get(stride..stride * 2)?;
                                Some(value.to_vec())
                            })
                            .collect()
                    } else {
                        flat.chunks(stride).map(|chunk| chunk.to_vec()).collect()
                    };
                    if times.is_empty() || values.is_empty() {
                        continue;
                    }
                    let duration = times.last().copied().unwrap_or(0.0);
                    clip.duration = clip.duration.max(duration);
                    clip.morph_tracks.push(MorphTrack {
                        node: target_node_index,
                        interpolation,
                        times,
                        values,
                    });
                    continue;
                }
            };
            let (values, in_tangents, out_tangents) = if cubic {
                // 布局是 [入切线, 值, 出切线] × N；三组分别收集，供 Hermite 用。
                let raw = values;
                let mut vals = Vec::with_capacity(raw.len() / 3);
                let mut ins = Vec::with_capacity(raw.len() / 3);
                let mut outs = Vec::with_capacity(raw.len() / 3);
                for chunk in raw.as_chunks::<3>().0 {
                    ins.push(chunk[0]);
                    vals.push(chunk[1]);
                    outs.push(chunk[2]);
                }
                (vals, ins, outs)
            } else {
                (values, Vec::new(), Vec::new())
            };
            if times.is_empty() || values.is_empty() {
                continue;
            }
            let duration = times.last().copied().unwrap_or(0.0);
            clip.duration = clip.duration.max(duration);
            clip.tracks.push(AnimationTrack {
                node: target_node_index,
                property,
                interpolation,
                times,
                values,
                in_tangents,
                out_tangents,
            });
        }
        model.animations.push(clip);
    }

    // 每份实例带上**节点**标签：同一个网格被多个节点引用时，标签才分得清左右轮
    /// 一个待展开的「网格 + 节点变换 + 节点/皮肤下标」条目。
    type NodeMeshEntry = (
        usize,
        Matrix,
        String,
        Option<usize>,
        Option<usize>,
        Option<Vec<f32>>,
    );
    let mut node_meshes: Vec<NodeMeshEntry> = Vec::new();
    for node in doc.nodes() {
        if let Some(mesh) = node.mesh() {
            used_meshes[mesh.index()] = true;
            let label = node
                .name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("node_{}", node.index()));
            node_meshes.push((
                mesh.index(),
                worlds[node.index()].unwrap_or(IDENTITY),
                label,
                Some(node.index()),
                node.skin().map(|s| s.index()),
                node.weights().map(|w| w.to_vec()),
            ));
        }
    }
    // 没被任何节点引用的网格（坏文件/极端导出器）：仍然按单位变换输出，
    // 免得"以前能看见、现在看不见"。引擎宁可多画一个，也不静默吞掉几何。
    for (i, used) in used_meshes.iter().enumerate() {
        if !used {
            node_meshes.push((i, IDENTITY, mesh_label(doc, i), None, None, None));
        }
    }
    for (mesh_index, world, label, node_index, skin_index, node_weights) in node_meshes {
        let mesh = doc.meshes().nth(mesh_index).expect("下标来自 doc.meshes()");
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
            // JOINTS_0 / WEIGHTS_0 只在有皮肤时读；下标必须是 u16（u8 会由
            // gltf crate 的 into_u16 升位），权重归一化交给运行时。
            let joints: Vec<[u16; 4]> = if skin_index.is_some() {
                reader
                    .read_joints(0)
                    .map(|j| j.into_u16().collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let weights: Vec<[f32; 4]> = if skin_index.is_some() {
                reader
                    .read_weights(0)
                    .map(|w| w.into_f32().collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            // morph target：positions 必须逐顶点对齐，否则整条 target 丢弃（宁可不动，
            // 也不能让一部分顶点被推走）；法线/切线缺失时保持 0 位移。
            let vertex_count = positions.len();
            let mut morph_targets: Vec<LoadedMorphTarget> = Vec::new();
            for (p, n, t) in reader.read_morph_targets() {
                let Some(p) = p else { continue };
                let p: Vec<[f32; 3]> = p.collect();
                if p.len() != vertex_count {
                    continue;
                }
                let n: Vec<[f32; 3]> = n
                    .map(|it| it.collect::<Vec<_>>())
                    .filter(|v| v.len() == vertex_count)
                    .unwrap_or_default();
                let t: Vec<[f32; 3]> = t
                    .map(|it| it.collect::<Vec<_>>())
                    .filter(|v| v.len() == vertex_count)
                    .unwrap_or_default();
                morph_targets.push(LoadedMorphTarget {
                    positions: p,
                    normals: n,
                    tangents: t,
                });
            }
            let mut morph_weights: Vec<f32> = node_weights
                .clone()
                .or_else(|| mesh.weights().map(|w| w.to_vec()))
                .unwrap_or_default();
            if morph_targets.is_empty() {
                morph_weights.clear();
            }

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
            // 把节点的世界变换烘进顶点：位置按点变换、法线/切线按方向变换后归一化。
            // 【边界】非均匀缩放下法线严格来说要用逆转置矩阵；这里用方向变换 + 归一化，
            // 偏差只在非均匀缩放的部件上出现（视觉上表现为高光角度略偏），换来的是一条
            // 不需要求逆、不会因奇异矩阵出 NaN 的路径。
            for v in &mut data.vertices {
                v.position = transform_point(&world, v.position);
                let n = transform_dir(&world, v.normal);
                let nv = Vec3::new(n[0], n[1], n[2]);
                v.normal = if nv.length_squared() > 1e-12 {
                    nv.normalize().to_array()
                } else {
                    Vec3::Y.to_array()
                };
                let t = transform_dir(&world, [v.tangent[0], v.tangent[1], v.tangent[2]]);
                let tv = Vec3::new(t[0], t[1], t[2]);
                if tv.length_squared() > 1e-12 {
                    let tv = tv.normalize();
                    v.tangent = [tv.x, tv.y, tv.z, v.tangent[3]];
                }
            }
            // morph 位移与顶点在同一个坐标系：位置按方向变换（不含平移），
            // 法线/切线只转方向。target 的权重稍后由场景覆盖。
            for target in &mut morph_targets {
                for p in &mut target.positions {
                    *p = transform_dir(&world, *p);
                }
                for n in &mut target.normals {
                    *n = transform_dir(&world, *n);
                }
                for t in &mut target.tangents {
                    *t = transform_dir(&world, *t);
                }
            }
            // 皮肤数据必须与顶点一一对应；缺一个顶点就整体退回刚性路径，
            // 而不是让一部分顶点蒙皮、一部分不动（那会撕裂网格）。
            let skin = skin_index.filter(|s| {
                model.skins.get(*s).is_some()
                    && joints.len() == data.vertices.len()
                    && weights.len() == data.vertices.len()
            });
            model.meshes.push(LoadedMesh {
                name: match mesh.name() {
                    Some(n) if mesh.primitives().len() > 1 => format!("{label}/{n}_{pi}"),
                    Some(n) => format!("{label}/{n}"),
                    None => format!("{label}/prim_{pi}"),
                },
                data,
                material: prim.material().index(),
                node: node_index,
                // 几何烘的是节点**绑定姿态**的世界变换，逆矩阵留给运行时算动画
                bind_inverse: matrix_to_mat4(&world).inverse(),
                skin,
                joints: if skin.is_some() { joints } else { Vec::new() },
                weights: if skin.is_some() { weights } else { Vec::new() },
                morph_targets,
                morph_weights,
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
            node: None,
            bind_inverse: Mat4::IDENTITY,
            skin: None,
            joints: Vec::new(),
            weights: Vec::new(),
            morph_targets: Vec::new(),
            morph_weights: Vec::new(),
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
    fn cubic_spline_uses_tangents() {
        // p0=0, p1=1, 出切线 2/s、入切线 0：u=.5 的 Hermite 值是 .75，不是线性的 .5。
        let track = AnimationTrack {
            node: 0,
            property: AnimatedProperty::Translation,
            interpolation: Interpolation::CubicSpline,
            times: vec![0.0, 1.0],
            values: vec![[0.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]],
            in_tangents: vec![[0.0; 4], [0.0; 4]],
            out_tangents: vec![[2.0, 0.0, 0.0, 0.0], [0.0; 4]],
        };
        let mid = sample_track(&track, 0.5);
        assert!(
            (mid[0] - 0.75).abs() < 1e-6,
            "Hermite 中点应为 .75，实际 {}",
            mid[0]
        );
        assert!((sample_track(&track, 0.0)[0]).abs() < 1e-6);
        assert!((sample_track(&track, 1.0)[0] - 1.0).abs() < 1e-6);

        // 切线字段不完整（旧数据/手写）时退回线性，不 panic。
        let mut broken = track.clone();
        broken.in_tangents.clear();
        assert!((sample_track(&broken, 0.5)[0] - 0.5).abs() < 1e-6);
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
