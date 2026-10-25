//! 编辑器文档：把界面上的一次操作，翻译成对引擎 / 物理世界的修改。
//!
//! 设计原则：
//!
//! 1. **没有第二份场景副本**。真相仍然在 `Engine::scene` 与 `Engine::world` 里，
//!    这里只存「选中了谁、基线是什么、文件路径」这类界面状态 —— 两份副本必然漂移。
//! 2. **每个操作都可解释**：加物体 = 建刚体 + 建场景对象；删除 = 两者一起删；
//!    重置 = 回到基线。没有藏在角落里的隐式状态。
//! 3. **基线按名字记**，不按下标：删掉中间一个物体，下标会整体前移，
//!    按名字记就不会把基线错位到别的物体上。

use quench3d::prelude::*;

/// 可添加的图元。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Primitive {
    /// 立方体。
    Cube,
    /// 球。
    Sphere,
    /// 圆柱。
    Cylinder,
    /// 胶囊。
    Capsule,
    /// 圆锥。
    Cone,
}

impl Primitive {
    /// 工具栏里按这个顺序排。
    pub const ALL: [Primitive; 5] = [
        Primitive::Cube,
        Primitive::Sphere,
        Primitive::Cylinder,
        Primitive::Capsule,
        Primitive::Cone,
    ];

    /// 界面上的名字（也是物体默认名的前缀）。
    pub fn label(self) -> &'static str {
        match self {
            Primitive::Cube => "立方体",
            Primitive::Sphere => "球",
            Primitive::Cylinder => "圆柱",
            Primitive::Capsule => "胶囊",
            Primitive::Cone => "圆锥",
        }
    }

    /// 用哪个预设网格。
    pub fn mesh(self, assets: &AssetCache) -> MeshId {
        match self {
            Primitive::Cube => assets.unit_cube,
            Primitive::Sphere => assets.unit_sphere,
            Primitive::Cylinder => assets.unit_cylinder,
            Primitive::Capsule => assets.unit_capsule,
            Primitive::Cone => assets.unit_cone,
        }
    }

    /// 物理形状（与预设网格同尺寸：立方体棱长 1、球半径 0.5…）。
    pub fn shape(self) -> Shape {
        match self {
            Primitive::Cube => Shape::Box {
                half_extents: Vec3::splat(0.5),
            },
            Primitive::Sphere => Shape::Sphere { radius: 0.5 },
            Primitive::Cylinder => Shape::Cylinder {
                half_height: 0.5,
                radius: 0.5,
            },
            Primitive::Capsule => Shape::Capsule {
                half_height: 0.5,
                radius: 0.5,
            },
            Primitive::Cone => Shape::Cone {
                half_height: 0.5,
                radius: 0.5,
            },
        }
    }
}

/// 物体在物理上是什么。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BodyKind {
    /// 动态刚体：受重力、会碰撞。
    Dynamic,
    /// 静态刚体：别人撞它它不动。
    Static,
    /// 纯显示：不参与物理。
    Visual,
}

impl BodyKind {
    /// 界面标签。
    pub fn label(self) -> &'static str {
        match self {
            BodyKind::Dynamic => "动态",
            BodyKind::Static => "静态",
            BodyKind::Visual => "仅显示",
        }
    }
}

/// 一条基线记录：重置时把物体拉回这个状态。
#[derive(Clone)]
struct Baseline {
    name: String,
    transform: Transform,
    body_transform: Option<Transform>,
    linear_velocity: Vec3,
    angular_velocity: Vec3,
}

/// 编辑器文档状态。
/// 编辑器里**显式指定**的材质贴图与开关（按物体名存）。
///
/// 为什么按名字而不是索引：物体增删会挪索引，名字是场景格式里本来就有的稳定键；
/// 导出时也是"同名物体继承原文档材质"的口径，两者一致。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TexturePaths {
    /// 基础色贴图路径（相对场景文件或绝对路径）。
    pub base_color: Option<String>,
    /// 切线空间法线贴图路径。
    pub normal: Option<String>,
    /// 金属度-粗糙度贴图路径。
    pub metallic_roughness: Option<String>,
    /// 自发光贴图路径。
    pub emissive: Option<String>,
    /// 环境光遮蔽（AO）贴图路径。
    pub ao: Option<String>,
    /// AO 强度（0~1）。
    pub ao_strength: Option<f32>,
    /// UV 平铺次数。
    pub uv_scale: Option<f32>,
    /// UV 偏移（度/贴图坐标）。
    pub uv_offset: Option<[f32; 2]>,
    /// UV 旋转（度）。
    pub uv_rotation: Option<f32>,
    /// 法线强度。
    pub normal_scale: Option<f32>,
    /// alpha 裁剪阈值。
    pub alpha_cutoff: Option<f32>,
    /// 走混合管线。
    pub alpha_blend: Option<bool>,
    /// 无光照着色。
    pub unlit: Option<bool>,
    /// 双面渲染。
    pub double_sided: Option<bool>,
}

/// 模型文档里的世界位置（缺省 = 原点）。
fn model_position(m: &quench3d_cli::scene_doc::ModelDoc) -> Vec3 {
    let p = m.position.unwrap_or([0.0, 0.0, 0.0]);
    Vec3::new(p[0], p[1], p[2])
}

/// 模型文档里的欧拉角（度，`Rx·Ry·Rz` 顺序）→ 四元数；缺省 = 无旋转。
///
/// 顺序必须与 `runner::euler_quat` 一致：单轴无所谓，复合旋转差一个顺序就是十几度。
fn model_rotation(m: &quench3d_cli::scene_doc::ModelDoc) -> Quat {
    match m.rotation_deg {
        None => Quat::IDENTITY,
        Some(d) => {
            let r = std::f32::consts::PI / 180.0;
            Quat::from_euler_xyz(d[0] * r, d[1] * r, d[2] * r)
        }
    }
}

/// `index` 的祖先链里有没有被选中的物体；用于把"父+子同选"折叠成只动父。
fn has_selected_ancestor(engine: &Engine, index: usize, selected: &[usize]) -> bool {
    let mut parent = engine
        .scene
        .objects
        .get(index)
        .and_then(|object| object.parent);
    let mut guard = 0;
    while let Some(p) = parent {
        if p >= engine.scene.objects.len() || guard > 1024 {
            return false;
        }
        if selected.contains(&p) {
            return true;
        }
        parent = engine.scene.objects[p].parent;
        guard += 1;
    }
    false
}

/// 剪贴板里的一份物体快照（世界变换 + 可重建的物理形状）。
#[derive(Clone)]
struct ClipboardObject {
    object: SceneObject,
    world: Transform,
    shape: Option<Shape>,
    dynamic: bool,
}

/// 编辑器文档层：场景内容的可撤销副本 + 界面状态（选中、状态栏、帮助开关）。
pub struct Doc {
    /// 当前**主选中**的场景对象下标（`None` = 没选）；gizmo 的轴心默认在它上面。
    pub selected: Option<usize>,
    /// 额外选中的物体（Shift / Ctrl 点选）。不包含 `selected` 本身。
    pub extra_selected: Vec<usize>,
    /// 场景文件路径（桌面用真实路径；浏览器里当下载文件名）。
    pub file_path: String,
    /// 场景树搜索框（只影响界面过滤，不写进场景文件）。
    pub outliner_filter: String,
    /// 桌面端打开的文件选择器（Web 用浏览器原生文件输入，不用它）。
    pub picker: Option<crate::picker::FilePicker>,
    /// 选择器选完后要执行的动作。
    pub picker_action: crate::picker::PickAction,
    /// 桌面端「导出 glTF/GLB」在文件选择器里指定的一次性目标路径。
    pub export_path: Option<std::path::PathBuf>,
    /// 最近一条给用户看的状态提示。
    pub status: String,
    /// 最近一次从 `cameras` 段套用的命名相机（只作界面状态，不写进场景）。
    pub selected_camera: Option<String>,
    /// 画中画使用的命名相机；`None` = 关闭。
    pub pip_camera: Option<String>,
    /// 画中画角落：0 右下、1 左下、2 右上、3 左上。
    pub pip_corner: u8,
    /// 画中画宽度占视口宽度的比例（0.15~0.45）。
    pub pip_scale: f32,
    /// 画中画回读分辨率：0 = 160×90，1 = 320×180，2 = 480×270。
    pub pip_resolution: u8,
    /// 画中画刷新间隔（帧）；默认 12 ≈ 5 Hz。
    pub pip_interval: u64,
    /// 画中画最近一帧（CPU 回读后上传的 egui 纹理）；仅在桌面端使用。
    pub pip_texture: Option<egui::TextureHandle>,
    /// 上一次画中画捕获的帧号，用来限制回读频率。
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) pip_last_frame: u64,
    /// 是否显示帮助窗口。
    pub show_help: bool,
    /// 是否在视口里画出场景探针（球形/盒形区域）。
    pub show_probes: bool,
    /// 是否画出选中物体的橙色包围盒。
    pub show_selection: bool,
    /// 本帧指针悬停命中的物体（只影响高亮，不写场景）。
    pub hovered: Option<usize>,
    /// 左/右面板宽度（使用偏好，不写进场景文件）。
    pub layout: crate::layout::Layout,
    /// 崩溃恢复文件的内容（`Some` = 工具栏显示"恢复 / 丢弃"）。
    pub recovery: Option<String>,
    /// 一次性自动保存请求：恢复出来的场景在历史里是"干净"的（`can_undo()==false`），
    /// 但它与启动场景不同，仍应马上写一个新恢复点，否则恢复后再崩就丢了。
    pub needs_autosave: bool,
    /// 撤销 / 重做历史（整场景快照，见 [`crate::history`]）。
    pub history: crate::history::History,
    /// 编辑器内部剪贴板（Ctrl+C / Ctrl+V）；不碰系统剪贴板。
    clipboard: Vec<ClipboardObject>,
    /// 用户显式设置的材质贴图/开关（空 = 沿用原文档的继承规则）。
    pub textures: std::collections::HashMap<String, TexturePaths>,
    /// 模型实例映射：`models[i]` ↔ 引擎里由它展开出来的物体。
    ///
    /// 由 [`crate::scene_io`] 在装载、撤销、重做时刷新 —— 物体增删会让下标整体挪动，
    /// 映射必须跟着重建，否则"拖模型"会拖到别人身上。
    pub model_instances: Vec<quench3d_cli::runner::ModelInstance>,
    /// 最近一次载入的场景文档（原样留着）。
    ///
    /// 编辑器只"懂"物体、地面、材质标量和相机；`lighting` / `simulate` / `render` /
    /// `joints` / `probes` / `actions` 这些段它既不改也不该丢。导出时以这份文档为底稿，
    /// 只覆盖编辑器真正管得着的部分 —— 否则「CLI 生成 → 编辑器改一改 → CLI 渲染」
    /// 会把光照与渲染设置**静默清零**（这曾经是真事）。
    pub(crate) base_doc: Option<quench3d_cli::scene_doc::SceneDoc>,
    baseline: Vec<Baseline>,
    /// 场景树里正在拖动的物体（拖到另一行上改父子）。
    ///
    /// 只是 UI 的瞬时状态，不写进场景文件；松开指针后必须清掉。
    pub drag_source: Option<usize>,
    /// 场景树拖放产生的改父请求；由 `outliner` 在只读遍历结束后统一执行。
    pub reparent_request: Option<(usize, Option<usize>)>,
    /// 名字自增计数，保证不重名。
    spawn_counter: u32,
}

/// 把当前场景序列化成历史快照。失败（例如场景里有表达不了的物体）返回 `None`，
/// 这时宁可不记历史，也不要记一份不完整的快照——撤销到残缺状态比不能撤销更糟。
fn scene_json(engine: &Engine, doc: &Doc) -> Option<String> {
    crate::scene_io::scene_to_json(engine, doc)
        .ok()
        .map(|saved| saved.json)
}

/// 场景树搜索：空查询匹配全部；否则大小写不敏感的子串匹配。
///
/// 抽成纯函数是为了让"搜索到底匹配什么"可以单测，而不是只能靠点界面。
pub fn outliner_query_matches(name: &str, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    name.to_lowercase().contains(&query.to_lowercase())
}

impl Default for Doc {
    fn default() -> Self {
        Self::new()
    }
}

impl Doc {
    /// 载入时保留的原文档。导出以它为底稿，编辑器**不建模**的段（`models` / `probes` /
    /// 点光源等）也靠它原样带出去。
    pub fn base_doc(&self) -> Option<&quench3d_cli::scene_doc::SceneDoc> {
        self.base_doc.as_ref()
    }

    /// 可变访问底稿（编辑器"模型"面板用它把模型变换写回 `models`）。
    pub fn base_doc_mut(&mut self) -> Option<&mut quench3d_cli::scene_doc::SceneDoc> {
        self.base_doc.as_mut()
    }

    /// 画中画回读尺寸（宽，高）。
    pub fn pip_size(&self) -> (u32, u32) {
        match self.pip_resolution {
            0 => (160, 90),
            2 => (480, 270),
            _ => (320, 180),
        }
    }

    /// 解析编辑器里输入的贴图路径：绝对路径原样用；相对路径先按场景文件所在目录
    /// 找，找不到再按场景声明的 `assets.search_paths`，最后交给调用方按工作目录处理。
    ///
    /// 与 CLI 的 `SceneDoc::resolve_asset` 同一优先级思路，但这里不要求文件已经存在
    /// （`exists` 只用来决定候选路径），最终是否读得动由解码器报错。
    pub fn resolve_texture_path(&self, path: &str) -> std::path::PathBuf {
        let raw = std::path::Path::new(path);
        if raw.is_absolute() || self.file_path.trim().is_empty() {
            return raw.to_path_buf();
        }
        if let Some(parent) = std::path::Path::new(&self.file_path).parent() {
            let candidate = parent.join(raw);
            if candidate.exists() {
                return candidate;
            }
            // 场景自己声明的资源库：与 CLI `asset_candidates` 同序（场景目录之后、
            // 工作目录之前），相对路径以场景目录为基准。
            if let Some(assets) = self.base_doc().and_then(|base| base.assets.as_ref()) {
                for search in &assets.search_paths {
                    let dir = if search.is_absolute() {
                        search.clone()
                    } else {
                        parent.join(search)
                    };
                    let candidate = dir.join(raw);
                    if candidate.exists() {
                        return candidate;
                    }
                }
            }
        }
        raw.to_path_buf()
    }

    // -----------------------------------------------------------------------
    // 多选
    // -----------------------------------------------------------------------

    /// 当前选中的全部物体（主选中在前，去重）。
    pub fn selection(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.extra_selected.len() + 1);
        if let Some(primary) = self.selected {
            out.push(primary);
        }
        for &i in &self.extra_selected {
            if !out.contains(&i) {
                out.push(i);
            }
        }
        out
    }

    /// 某个物体是否被选中（主选或附加）。
    pub fn is_selected(&self, index: usize) -> bool {
        self.selected == Some(index) || self.extra_selected.contains(&index)
    }

    /// 只选中一个物体（普通点击 / 打开场景 / 新建物体）。
    pub fn select_only(&mut self, index: Option<usize>) {
        self.selected = index;
        self.extra_selected.clear();
    }

    /// Shift/Ctrl 点选：切换该物体，保留其它已选。
    ///
    /// 关掉主选中时把第一个附加选中提为主选中，而不是把整组清空——
    /// 这样连续 Shift 点选时 gizmo 不会突然消失。
    pub fn toggle_selected(&mut self, index: usize) {
        match self.selected {
            None => self.selected = Some(index),
            Some(primary) if primary == index => {
                if self.extra_selected.is_empty() {
                    self.selected = None;
                } else {
                    self.selected = Some(self.extra_selected.remove(0));
                }
            }
            Some(_) => {
                if let Some(pos) = self.extra_selected.iter().position(|&i| i == index) {
                    self.extra_selected.remove(pos);
                } else {
                    self.extra_selected.push(index);
                }
            }
        }
    }

    /// 多选 gizmo 的轴心：所有选中物体世界位置的平均值。
    pub fn selection_center(&self, engine: &Engine) -> Option<Vec3> {
        let mut sum = Vec3::ZERO;
        let mut n = 0usize;
        for index in self.selection() {
            if index < engine.scene.objects.len() {
                sum += engine.scene.world_transform(index).position;
                n += 1;
            }
        }
        (n > 0).then(|| sum / n as f32)
    }

    /// 组变换要一起动的附加物体（下标 + **按下时**的世界变换）。
    ///
    /// 同一个模型展开出来的其它网格不在这里：主物体已经通过
    /// [`Self::apply_transform_to_model_group`] 把整组模型带走了，再动一次会双倍位移。
    pub fn group_selection(&self, engine: &Engine) -> Vec<(usize, Transform)> {
        let Some(primary) = self.selected else {
            return Vec::new();
        };
        let selected = self.selection();
        let selected_set = selected.clone();
        let driver = self.drag_driver(engine, primary);
        let primary_model = self.model_of_object(primary);
        selected
            .into_iter()
            .filter(|&i| i < engine.scene.objects.len() && i != driver)
            // 只保留"选中集合里的根"：父物体和子物体同时选中时，子物体跟着父物体走，
            // 不能再单独应用一次变换（会走两倍）。
            .filter(|&i| !has_selected_ancestor(engine, i, &selected_set))
            .filter(|&i| match primary_model {
                Some(model) => self.model_of_object(i) != Some(model),
                None => true,
            })
            .map(|i| (i, engine.scene.world_transform(i)))
            .collect()
    }

    /// 拖拽时真正该被写变换的物体：如果主选物体有**被选中的祖先**，最上层的那个
    /// 祖先就是驱动者（子物体会跟着它走）。否则就是主选物体自己。
    pub fn drag_driver(&self, engine: &Engine, index: usize) -> usize {
        let selected = self.selection();
        let mut driver = index;
        let mut parent = engine
            .scene
            .objects
            .get(index)
            .and_then(|object| object.parent);
        let mut guard = 0;
        while let Some(p) = parent {
            if p >= engine.scene.objects.len() || guard > 1024 {
                break;
            }
            if selected.contains(&p) {
                driver = p;
            }
            parent = engine.scene.objects[p].parent;
            guard += 1;
        }
        driver
    }

    // -----------------------------------------------------------------------
    // 模型实例（导入的 glTF/OBJ）↔ 引擎物体
    // -----------------------------------------------------------------------

    /// 装载 / 撤销 / 重做之后刷新映射（物体下标会被增删挪动）。
    pub fn set_model_instances(&mut self, instances: Vec<quench3d_cli::runner::ModelInstance>) {
        self.model_instances = instances;
    }

    /// 某个模型实例（`models` 下标）展开出来的物体下标；没有这个实例时 `None`。
    pub fn model_objects(&self, model: usize) -> Option<&[usize]> {
        self.model_instances
            .iter()
            .find(|i| i.model == model)
            .map(|i| i.objects.as_slice())
    }

    /// 这个物体是哪个模型实例展开出来的（返回 `models` 里的下标）。
    pub fn model_of_object(&self, object: usize) -> Option<usize> {
        self.model_instances
            .iter()
            .find(|i| i.contains(object))
            .map(|i| i.model)
    }

    /// 追加一个状态机状态（名重复/空名/没有 animator 时返回 `None`）。
    pub fn add_animator_state(
        &mut self,
        objects: &[usize],
        name: &str,
        clip: &str,
    ) -> Option<String> {
        if name.is_empty() {
            return None;
        }
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        let animator = doc.animator.as_mut()?;
        if animator.states.iter().any(|s| s.name == name) {
            return None;
        }
        animator
            .states
            .push(quench3d_cli::scene_doc::ModelAnimatorStateDoc {
                name: name.to_string(),
                animation: clip.to_string(),
                speed: None,
                looping: None,
                start: None,
            });
        Some(path)
    }

    /// 删除一个状态机状态；同时删掉引用它的过渡，并在需要时改写 `initial`。
    ///
    /// 只剩一个状态时拒绝删除（状态机至少要有一个状态）。
    pub fn remove_animator_state(&mut self, objects: &[usize], index: usize) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        let animator = doc.animator.as_mut()?;
        if animator.states.len() <= 1 || index >= animator.states.len() {
            return None;
        }
        let removed = animator.states.remove(index).name;
        animator
            .transitions
            .retain(|t| t.from != removed && t.to != removed);
        if animator.initial == removed {
            animator.initial = animator.states[0].name.clone();
        }
        Some(path)
    }

    /// 追加一条状态机过渡；`from/to` 必须都存在，且不能自环。
    pub fn add_animator_transition(
        &mut self,
        objects: &[usize],
        from: &str,
        to: &str,
        after: f32,
        fade: f32,
        on_finished: bool,
    ) -> Option<String> {
        if from == to {
            return None;
        }
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        let animator = doc.animator.as_mut()?;
        if !animator.states.iter().any(|s| s.name == from)
            || !animator.states.iter().any(|s| s.name == to)
        {
            return None;
        }
        animator
            .transitions
            .push(quench3d_cli::scene_doc::ModelAnimatorTransitionDoc {
                from: from.to_string(),
                to: to.to_string(),
                after,
                on_finished,
                fade: Some(fade),
                conditions: Vec::new(),
                on_event: None,
            });
        Some(path)
    }

    /// 读回底稿里的状态机初始状态名。
    pub fn animator_initial(&self, objects: &[usize]) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_ref()?;
        base.models
            .get(model)?
            .animator
            .as_ref()
            .map(|a| a.initial.clone())
    }

    /// 改写状态机初始状态；未知状态/没有 animator 时返回 `None`。
    pub fn set_animator_initial(&mut self, objects: &[usize], name: &str) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        let animator = doc.animator.as_mut()?;
        if !animator.states.iter().any(|s| s.name == name) {
            return None;
        }
        animator.initial = name.to_string();
        Some(path)
    }

    /// 改写第 `index` 条过渡的起点/终点状态；自环或未知状态返回 `None`。
    pub fn set_animator_transition_ends(
        &mut self,
        objects: &[usize],
        index: usize,
        from: &str,
        to: &str,
    ) -> Option<String> {
        if from == to {
            return None;
        }
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        let animator = doc.animator.as_mut()?;
        if !animator.states.iter().any(|s| s.name == from)
            || !animator.states.iter().any(|s| s.name == to)
        {
            return None;
        }
        let transition = animator.transitions.get_mut(index)?;
        transition.from = from.to_string();
        transition.to = to.to_string();
        Some(path)
    }

    /// 删除第 `index` 条状态机过渡。
    pub fn remove_animator_transition(
        &mut self,
        objects: &[usize],
        index: usize,
    ) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        let animator = doc.animator.as_mut()?;
        if index >= animator.transitions.len() {
            return None;
        }
        animator.transitions.remove(index);
        Some(path)
    }

    /// 把状态机第 `index` 个状态的 clip 名与起始时间写回底稿。
    ///
    /// 返回模型路径；没有映射、模型没有 animator 或下标越界时返回 `None`。
    pub fn set_animator_state(
        &mut self,
        objects: &[usize],
        index: usize,
        clip: &str,
        start: f32,
    ) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        let state = doc.animator.as_mut()?.states.get_mut(index)?;
        state.animation = clip.to_string();
        state.start = Some(start);
        Some(path)
    }

    /// 把模型的手动 morph 权重写回 `models[i].morph_weights`。
    ///
    /// `Some(vec![])` 与 `None` 不同：前者是显式全 0 覆盖，后者删除覆盖。返回模型路径；
    /// 没有映射或模型下标不存在时返回 `None`。
    pub fn set_morph_weights(
        &mut self,
        objects: &[usize],
        weights: Option<Vec<f32>>,
    ) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        doc.morph_weights = weights;
        Some(doc.path.display().to_string())
    }

    /// 把 `models[i].ik[chain].target` 写回底稿（模型局部空间）。
    ///
    /// 返回模型路径；没有映射、模型没有 `ik` 段或下标越界时返回 `None`。
    pub fn set_ik_target(
        &mut self,
        objects: &[usize],
        chain: usize,
        target: [f32; 3],
    ) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        let ik = doc.ik.as_mut()?.get_mut(chain)?;
        ik.target = target;
        Some(path)
    }

    /// 把 `models[i].events` 整组写回底稿；空数组写成 `None`（不留下空段）。
    ///
    /// 返回模型路径；没有映射或模型下标越界时返回 `None`。
    pub fn set_animation_events(
        &mut self,
        objects: &[usize],
        events: Vec<quench3d_cli::scene_doc::ModelEventDoc>,
    ) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        doc.events = if events.is_empty() {
            None
        } else {
            Some(events)
        };
        Some(path)
    }

    /// 把 `models[i].ik[chain].weight` 写回底稿（0~1，IK/FK 混合）。
    ///
    /// 返回模型路径；没有映射、模型没有 `ik` 段或下标越界时返回 `None`。
    pub fn set_ik_weight(
        &mut self,
        objects: &[usize],
        chain: usize,
        weight: f32,
    ) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        let ik = doc.ik.as_mut()?.get_mut(chain)?;
        ik.weight = Some(weight);
        Some(path)
    }

    /// 把状态机第 `index` 条过渡的时间/淡入/结束触发写回底稿。
    ///
    /// 返回模型路径；没有映射、模型没有 animator 或下标越界时返回 `None`。
    pub fn set_animator_transition(
        &mut self,
        objects: &[usize],
        index: usize,
        after: f32,
        fade: f32,
        on_finished: bool,
    ) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        let transition = doc.animator.as_mut()?.transitions.get_mut(index)?;
        transition.after = after;
        transition.fade = Some(fade);
        transition.on_finished = on_finished;
        Some(path)
    }

    /// 把普通模型动画（不是状态机）的 clip 名写回 `models[i].animation.name`。
    ///
    /// 返回模型路径；没有映射、模型只有 `animator` 或没有 `animation` 段时返回 `None`。
    pub fn set_animation_clip(&mut self, objects: &[usize], clip: &str) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        // 状态机的 clip 由 states 管；这里只改普通 animation 段，避免把状态机结构写坏。
        let animation = doc.animation.as_mut()?;
        animation.name = clip.to_string();
        Some(path)
    }

    /// 把动画倍速/循环写回底稿里对应模型的 `animation` 或当前 `animator` 状态。
    ///
    /// `objects` 是这个动画实例驱动的物体（用来反查 `models[i]`）；`state` 是状态机
    /// 当前状态名（非状态机传 `None`）。返回模型路径，没映射/没字段时返回 `None`。
    /// 只改**底稿**，不改运行时播放器 —— 运行时由调用方同步，避免这里依赖 `Engine`。
    pub fn set_animation_params(
        &mut self,
        objects: &[usize],
        state: Option<&str>,
        speed: f32,
        looping: bool,
    ) -> Option<String> {
        let model = objects
            .iter()
            .find_map(|&object| self.model_of_object(object))?;
        let base = self.base_doc.as_mut()?;
        let doc = base.models.get_mut(model)?;
        let path = doc.path.display().to_string();
        if let Some(animator) = &mut doc.animator {
            let state = state?;
            let state = animator.states.iter_mut().find(|s| s.name == state)?;
            state.speed = Some(speed);
            state.looping = Some(looping);
            return Some(path);
        }
        let animation = doc.animation.as_mut()?;
        animation.speed = Some(speed);
        animation.looping = Some(looping);
        Some(path)
    }

    /// 文档里记录的模型变换（位置 / 姿态 / 均匀缩放，缺省 = 原点 / 无旋转 / 1 倍）。
    pub fn model_transform(&self, model: usize) -> Option<Transform> {
        let m = self.base_doc.as_ref()?.models.get(model)?;
        Some(Transform::new(
            model_position(m),
            model_rotation(m),
            Vec3::splat(m.scale.unwrap_or(1.0)),
        ))
    }

    /// 把 `models[model]` 的变换**当场**应用到引擎里由它展开出来的物体。
    ///
    /// 以前模型面板只改底稿，改完视口不动、要重开才生效；有了映射就能立刻动。
    /// 返回真的被改到的物体数（没有映射或没有物体时是 0）。
    pub fn apply_model_transform(&mut self, engine: &mut Engine, model: usize) -> usize {
        let Some(transform) = self.model_transform(model) else {
            return 0;
        };
        let Some(objects) = self.model_objects(model).map(<[usize]>::to_vec) else {
            return 0;
        };
        let mut moved = 0;
        for index in objects {
            if engine.scene.objects.get(index).is_some() {
                // 世界变换写回（模型网格一般没有父物体，但有了也不会写错）。
                engine.scene.set_world_transform(index, transform);
                moved += 1;
            }
        }
        moved
    }

    /// 反过来：把一个物体（视口里被拖动的那个）的变换写回它所属的模型，
    /// 并让它这一组物体**整组**跟着动。
    ///
    /// 返回 `true` 表示这个物体确实属于某个模型实例 —— 调用方据此判断
    /// "这次拖拽是在拖模型"（要整组联动），而不是普通物体。
    ///
    /// 缩放统一取 X 分量：场景格式里的模型只有**均匀**缩放（`scale: f32`），
    /// 视口里对模型成员也会强制走均匀缩放（见 `gizmo.rs`）。
    pub fn apply_transform_to_model_group(
        &mut self,
        engine: &mut Engine,
        object: usize,
        transform: Transform,
    ) -> bool {
        let Some(model) = self.model_of_object(object) else {
            return false;
        };
        let Some(objects) = self.model_objects(model).map(<[usize]>::to_vec) else {
            return false;
        };
        let uniform = Transform::new(
            transform.position,
            transform.rotation,
            Vec3::splat(transform.scale.x),
        );
        for index in objects {
            if engine.scene.objects.get(index).is_some() {
                engine.scene.set_world_transform(index, uniform);
            }
        }
        self.write_model_transform(model, uniform);
        true
    }

    /// 把变换写进底稿的 `models[model]`。没有底稿/没有这一条时返回 `false`。
    fn write_model_transform(&mut self, model: usize, transform: Transform) -> bool {
        let Some(base) = self.base_doc.as_mut() else {
            return false;
        };
        let Some(entry) = base.models.get_mut(model) else {
            return false;
        };
        entry.position = Some([
            transform.position.x,
            transform.position.y,
            transform.position.z,
        ]);
        entry.rotation_deg = Some(crate::scene_io::to_euler_xyz_deg(transform.rotation));
        entry.scale = Some(transform.scale.x);
        true
    }

    /// 删掉一个物体之后维护映射里的下标（后面的物体整体前移一位）。
    fn forget_object(&mut self, removed: usize) {
        for instance in &mut self.model_instances {
            instance.objects.retain(|&o| o != removed);
            for object in &mut instance.objects {
                if *object > removed {
                    *object -= 1;
                }
            }
        }
    }

    /// 空文档。
    pub fn new() -> Self {
        Self {
            selected: None,
            extra_selected: Vec::new(),
            file_path: "scene.json".to_string(),
            outliner_filter: String::new(),
            picker: None,
            picker_action: crate::picker::PickAction::OpenScene,
            export_path: None,
            status: "就绪".to_string(),
            selected_camera: None,
            pip_camera: None,
            pip_corner: 0,
            pip_scale: 0.28,
            pip_resolution: 1,
            pip_interval: 12,
            pip_texture: None,
            pip_last_frame: 0,
            show_help: false,
            show_probes: false,
            show_selection: true,
            hovered: None,
            layout: crate::layout::Layout::default(),
            recovery: None,
            needs_autosave: false,
            history: crate::history::History::default(),
            clipboard: Vec::new(),
            textures: std::collections::HashMap::new(),
            model_instances: Vec::new(),
            base_doc: None,
            baseline: Vec::new(),
            drag_source: None,
            reparent_request: None,
            spawn_counter: 0,
        }
    }

    /// 套用场景 `cameras` 段里的命名视角（只切主相机，不重建场景）。
    ///
    /// 返回 false 表示没有底稿或名字不存在；成功时把视角写进 `engine.orbit`，
    /// 并记下 `selected_camera` 供界面显示。物理/场景对象不受影响。
    pub fn select_named_camera(&mut self, engine: &mut Engine, name: &str) -> bool {
        let Some(camera) = self
            .base_doc
            .as_ref()
            .and_then(|base| base.cameras.get(name))
            .cloned()
        else {
            return false;
        };
        crate::scene_io::apply_camera(engine, &camera);
        engine.camera_mode = quench3d::app::CameraMode::Orbit;
        self.selected_camera = Some(name.to_string());
        self.status = format!("相机：{name}");
        true
    }

    /// 在场景 `cameras` 段里前后切换命名视角；`delta` 为正向后、负向前，循环。
    ///
    /// 没有命名相机时返回 `None`。当前是「编辑器视角」时，正向选第一台、负向选最后。
    pub fn cycle_named_camera(&mut self, engine: &mut Engine, delta: i32) -> Option<String> {
        let names: Vec<String> = self.base_doc.as_ref()?.cameras.keys().cloned().collect();
        if names.is_empty() {
            return None;
        }
        let current = self
            .selected_camera
            .as_ref()
            .and_then(|name| names.iter().position(|n| n == name));
        let next = match current {
            Some(index) => (index as i32 + delta).rem_euclid(names.len() as i32) as usize,
            None if delta >= 0 => 0,
            None => names.len() - 1,
        };
        let name = names[next].clone();
        self.select_named_camera(engine, &name).then_some(name)
    }

    /// 编辑完成之后记一步历史（与栈顶相同则自动跳过）。
    pub fn record_edit(&mut self, engine: &Engine) {
        let json = scene_json(engine, self);
        let label = self.status.clone();
        self.history.record_json_labeled(json, Some(label));
    }

    /// 开始一段连续编辑（拖 gizmo / 拖数值框）：整段只算一步。
    pub fn begin_history_transaction(&mut self) {
        self.history.begin_transaction();
    }

    /// 结束连续编辑。
    pub fn end_history_transaction(&mut self, engine: &Engine) {
        let json = scene_json(engine, self);
        self.history.end_transaction_json(json);
    }

    /// 撤销一步（含状态提示与选区收敛）。
    pub fn undo(&mut self, engine: &mut Engine) {
        match self.history.undo(engine) {
            Some(Ok(restored)) => {
                self.refresh_base_from(&restored.snapshot);
                self.model_instances = restored.instances;
                self.clamp_selection(engine);
                self.status = format!("已撤销 → {} 个物体", restored.objects);
            }
            Some(Err(e)) => self.status = format!("撤销失败：{e}"),
            None => self.status = "没有可撤销的步骤".to_string(),
        }
    }

    /// 重做一步（含状态提示与选区收敛）。
    pub fn redo(&mut self, engine: &mut Engine) {
        match self.history.redo(engine) {
            Some(Ok(restored)) => {
                self.refresh_base_from(&restored.snapshot);
                self.model_instances = restored.instances;
                self.clamp_selection(engine);
                self.status = format!("已重做 → {} 个物体", restored.objects);
            }
            Some(Err(e)) => self.status = format!("重做失败：{e}"),
            None => self.status = "没有可重做的步骤".to_string(),
        }
    }

    /// 跳到历史里的第 `index` 步（0 是最旧），含状态提示与选区收敛。
    pub fn jump_history(&mut self, engine: &mut Engine, index: usize) {
        match self.history.jump_to(index, engine) {
            Some(Ok(restored)) => {
                self.refresh_base_from(&restored.snapshot);
                self.model_instances = restored.instances;
                self.clamp_selection(engine);
                self.status = format!(
                    "已跳到历史第 {} 步 → {} 个物体",
                    index + 1,
                    restored.objects
                );
            }
            Some(Err(e)) => self.status = format!("跳转历史失败：{e}"),
            None => self.status = "这一步就是当前状态".to_string(),
        }
    }

    /// 撤销/重做之后，底稿文档也要跟着快照走 —— 否则"撤销回到带光照的场景、
    /// 再存一次"会把光照丢掉。
    fn refresh_base_from(&mut self, snapshot: &str) {
        self.base_doc = quench3d_cli::scene_doc::SceneDoc::from_json(snapshot).ok();
    }

    /// 以当前场景为起点重开历史（打开文件 / 装载新场景后调用）。
    pub fn reset_history(&mut self, engine: &Engine) {
        let json = scene_json(engine, self);
        self.history.reset_json(json);
        self.clipboard.clear();
        self.needs_autosave = false;
        self.status = "历史已重置".to_string();
    }

    /// 把视口对准选中的物体（`F` 键）。没选中时对准整个场景。
    ///
    /// 只改轨道相机的焦点与距离，不动 FOV、不动物体本身。
    pub fn focus_selected(&mut self, engine: &mut Engine) {
        let indices: Vec<usize> = match self.selected {
            Some(index) if index < engine.scene.len() => vec![index],
            _ => (0..engine.scene.len()).collect(),
        };
        if indices.is_empty() {
            self.status = "场景是空的，没东西可聚焦".to_string();
            return;
        }
        match engine.scene.bounds_of(&engine.renderer, &indices) {
            Some(bounds) => {
                engine.orbit.focus_on(&bounds);
                self.status = match self.selected {
                    Some(_) => "已聚焦选中物体".to_string(),
                    None => "未选中：已聚焦整个场景".to_string(),
                };
            }
            None => self.status = "选中的物体没有可算包围盒的网格".to_string(),
        }
    }

    /// 选中的下标越界就取消选中（撤销/重做后物体数量会变）。
    fn clamp_selection(&mut self, engine: &Engine) {
        self.extra_selected.retain(|&i| i < engine.scene.len());
        if let Some(index) = self.selected {
            if index >= engine.scene.len() {
                self.selected = self.extra_selected.first().copied();
                if !self.extra_selected.is_empty() {
                    self.extra_selected.remove(0);
                }
            }
        }
    }

    /// 当前选中的对象。
    pub fn selected_object<'a>(&self, engine: &'a Engine) -> Option<&'a SceneObject> {
        self.selected.and_then(|i| engine.scene.objects.get(i))
    }

    /// 记下当前状态当基线（重置回到这里）。
    pub fn capture_baseline(&mut self, engine: &Engine) {
        self.baseline = engine
            .scene
            .objects
            .iter()
            .map(|o| {
                let body = o.body.and_then(|h| engine.world.body(h));
                Baseline {
                    name: o.name.clone(),
                    transform: o.transform,
                    body_transform: body.map(|b| b.transform()),
                    linear_velocity: body.map(|b| b.linear_velocity()).unwrap_or(Vec3::ZERO),
                    angular_velocity: body.map(|b| b.angular_velocity()).unwrap_or(Vec3::ZERO),
                }
            })
            .collect();
    }

    /// 回到基线：位置、朝向、速度都复原（刚体同时唤醒，否则睡着的球不会掉）。
    pub fn reset(&mut self, engine: &mut Engine) {
        for base in &self.baseline {
            let Some(index) = engine
                .scene
                .objects
                .iter()
                .position(|o| o.name == base.name)
            else {
                continue;
            };
            engine.scene.objects[index].transform = base.transform;
            if let Some(handle) = engine.scene.objects[index].body {
                if let Some(body) = engine.world.body_mut(handle) {
                    let mut t = base.body_transform.unwrap_or(base.transform);
                    t.scale = Vec3::ONE;
                    body.set_transform(t);
                    body.set_linear_velocity(base.linear_velocity);
                    body.set_angular_velocity(base.angular_velocity);
                    body.wake_up();
                }
            }
        }
        self.status = format!("已重置到基线（{} 个物体）", self.baseline.len());
    }

    /// 单步：不管当前是否暂停，都推进一个固定步长。
    pub fn step_once(&mut self, engine: &mut Engine) {
        let dt = engine.world.fixed_dt();
        engine.world.step(dt);
        if engine.scene.auto_sync_physics {
            let world = &engine.world;
            engine.scene.sync_from_physics(world);
        }
        self.status = format!("单步 {:.1} ms", dt * 1000.0);
    }

    /// 摆一个可以立刻玩起来的默认场景（地面 + 一摞物体）。
    ///
    /// 编辑器一打开就该有东西可看、可点、可推 —— 空白视口没法验证任何功能。
    pub fn build_default_scene(&mut self, engine: &mut Engine) {
        engine.scene.clear();
        engine.world.clear();
        self.model_instances.clear();

        // ---- 光照：与"存盘后重新载入"用**同一份**预设应用 ----
        //
        // 用一个空文档调 CLI 的 `apply_lighting`（语义 = "一个没写 lighting 的场景"），
        // 而不是让引擎留着 `Lighting::default()`：撤销/重做与打开文件都走载入路径，
        // 两条路径的默认光照如果不同，"撤销一次"就会凭空多出预设的补光（第 53 轮实测：
        // 多出 studio 预设的 2 盏点光源：`[-4,5,4]` 暖色与 `[5,4,-3]` 冷色）。
        // 这类分叉以前被 `scene_to_json` 对局部光"沿用底稿"藏住了。
        let no_lighting =
            quench3d_cli::scene_doc::SceneDoc::from_json("{}").expect("空场景文档必须能解析");
        let _ = quench3d_cli::runner::apply_lighting(engine, &no_lighting);

        // ---- 地面：物理上是无限半空间，视觉上是一张同高度的平面 ----
        //
        // 刻意用 `unit_plane` 而不是"压扁的立方体"：立方体的表面在原点上方半个厚度处，
        // 而半空间在原点平面上，两者会差半个板厚（物体看起来半埋在地里），
        // 存成 `ground` 再读回来也会掉下去半个板厚。平面网格则与半空间严格重合。
        let ground_body = engine
            .world
            .add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }));
        let ground_mat = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.44, 0.46, 0.50, 1.0),
            0.0,
            0.9,
        );
        let ground = engine.scene.add_named(
            engine.assets.unit_plane,
            ground_mat,
            Transform::new(Vec3::ZERO, Quat::IDENTITY, Vec3::new(60.0, 60.0, 60.0)),
            "地面",
        );
        engine.scene.objects[ground].body = Some(ground_body);

        // ---- 一摞动态物体：掉下来、堆起来、可以拖 ----
        let palette = [
            Vec4::new(0.91, 0.30, 0.24, 1.0),
            Vec4::new(0.24, 0.60, 0.91, 1.0),
            Vec4::new(0.30, 0.78, 0.44, 1.0),
            Vec4::new(0.95, 0.75, 0.25, 1.0),
            Vec4::new(0.72, 0.42, 0.90, 1.0),
        ];
        let kinds = [
            Primitive::Cube,
            Primitive::Sphere,
            Primitive::Cylinder,
            Primitive::Capsule,
            Primitive::Cube,
        ];
        for (i, (kind, color)) in kinds.iter().zip(palette.iter()).enumerate() {
            let position = Vec3::new((i as f32 - 2.0) * 1.35, 0.6 + (i % 2) as f32 * 1.5, 0.0);
            self.spawn_colored(engine, *kind, position, *color);
        }

        // 一块斜面：方便看摩擦与滚动。
        let ramp_body = engine.world.add_builder(
            BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::new(2.2, 0.15, 1.4),
            })
            .position(Vec3::new(0.0, 0.35, -3.4))
            .rotation(Quat::from_rotation_x(-0.35)),
        );
        let ramp_mat = engine.assets.solid_material(
            &mut engine.renderer,
            &engine.gpu,
            Vec4::new(0.55, 0.53, 0.50, 1.0),
            0.0,
            0.75,
        );
        let ramp = engine.scene.add_named(
            engine.assets.unit_cube,
            ramp_mat,
            Transform::new(
                Vec3::new(0.0, 0.35, -3.4),
                Quat::from_rotation_x(-0.35),
                Vec3::new(4.4, 0.3, 2.8),
            ),
            "斜面",
        );
        engine.scene.objects[ramp].body = Some(ramp_body);

        engine.orbit.focus = Vec3::new(0.0, 1.2, 0.0);
        engine.orbit.distance = 11.0;
        self.select_only(Some(1));
        self.capture_baseline(engine);
        self.status = format!("已建默认场景（{} 个物体）", engine.scene.len());
    }

    /// 加一个图元（默认颜色）。
    pub fn spawn(&mut self, engine: &mut Engine, kind: Primitive) -> usize {
        let color = Vec4::new(0.35, 0.62, 0.92, 1.0);
        let position = engine.orbit.focus + Vec3::new(0.0, 1.2, 0.0);
        self.spawn_colored(engine, kind, position, color)
    }

    /// 加一个图元（指定位置与颜色）。
    pub fn spawn_colored(
        &mut self,
        engine: &mut Engine,
        kind: Primitive,
        position: Vec3,
        color: Vec4,
    ) -> usize {
        let name = self.unique_name(engine, kind.label());
        let mesh = kind.mesh(&engine.assets);
        let material =
            engine
                .assets
                .solid_material(&mut engine.renderer, &engine.gpu, color, 0.05, 0.45);
        let body = engine.world.add_builder(
            BodyBuilder::dynamic(kind.shape())
                .position(position)
                .friction(0.6)
                .restitution(0.2)
                .linear_damping(0.03)
                .angular_damping(0.08),
        );
        let index = engine
            .scene
            .add_with_body(mesh, material, body, &engine.world, &name);
        engine.scene.objects[index].color = Vec4::ONE;
        self.select_only(Some(index));
        self.status = format!("已添加 {name}");
        index
    }

    /// 把当前选择复制进编辑器内部剪贴板（不改变场景，也不记历史）。
    ///
    /// 存的是世界变换：粘贴时即使原物体有父物体，也会以根物体出现在同一世界位置，
    /// 不会因为父坐标系而错位。物理形状单独留一份，粘贴时重建刚体。
    pub fn copy_selected(&mut self, engine: &Engine) -> usize {
        let mut clip = Vec::new();
        for index in self.selection() {
            let Some(object) = engine.scene.objects.get(index) else {
                continue;
            };
            let world = engine.scene.world_transform(index);
            let body = object.body.and_then(|h| engine.world.body(h));
            clip.push(ClipboardObject {
                object: object.clone(),
                world,
                shape: body.map(|b| b.shape.clone()),
                dynamic: body.map(|b| b.is_dynamic()).unwrap_or(false),
            });
        }
        let count = clip.len();
        if count == 0 {
            self.status = "剪贴板：没有选中的物体".to_string();
            return 0;
        }
        self.clipboard = clip;
        self.status = format!("已复制 {count} 个物体到剪贴板");
        count
    }

    /// 粘贴剪贴板内容；返回粘贴出来的物体数。粘贴会记一步历史。
    pub fn paste_clipboard(&mut self, engine: &mut Engine) -> usize {
        if self.clipboard.is_empty() {
            self.status = "剪贴板是空的".to_string();
            return 0;
        }
        let clips = self.clipboard.clone();
        let mut made = Vec::with_capacity(clips.len());
        for (n, clip) in clips.iter().enumerate() {
            let offset = 1.2 * (n as f32 + 1.0);
            let target = Transform::new(
                clip.world.position + Vec3::new(offset, 0.2, 0.0),
                clip.world.rotation,
                clip.world.scale,
            );
            let name = self.unique_name(engine, &clip.object.name);
            let copy = match &clip.shape {
                Some(shape) => {
                    let builder = if clip.dynamic {
                        BodyBuilder::dynamic(shape.clone())
                    } else {
                        BodyBuilder::r#static(shape.clone())
                    };
                    let handle = engine
                        .world
                        .add_builder(builder.position(target.position).rotation(target.rotation));
                    let i = engine.scene.add_with_body(
                        clip.object.mesh,
                        clip.object.material,
                        handle,
                        &engine.world,
                        &name,
                    );
                    engine.scene.set_world_transform(i, target);
                    i
                }
                None => {
                    engine
                        .scene
                        .add_named(clip.object.mesh, clip.object.material, target, &name)
                }
            };
            let dst = &mut engine.scene.objects[copy];
            dst.color = clip.object.color;
            dst.metallic = clip.object.metallic;
            dst.roughness = clip.object.roughness;
            dst.emissive_strength = clip.object.emissive_strength;
            dst.parent = None;
            made.push(copy);
        }
        let count = made.len();
        let mut iter = made.into_iter();
        self.select_only(iter.next());
        self.extra_selected.extend(iter);
        self.record_edit(engine);
        self.status = format!("已粘贴 {count} 个物体");
        count
    }

    /// 复制选中的物体（位置往旁边挪一点，免得完全重叠）；多选时批量复制。
    pub fn duplicate_selected(&mut self, engine: &mut Engine) {
        let originals: Vec<_> = self
            .selection()
            .into_iter()
            .filter(|&i| i < engine.scene.objects.len())
            .collect();
        if originals.is_empty() {
            return;
        }
        let mut made = Vec::with_capacity(originals.len());
        for (n, &index) in originals.iter().enumerate() {
            let object = engine.scene.objects[index].clone();
            let name = self.unique_name(engine, &object.name);
            let offset = 1.2 * (n as f32 + 1.0);
            let position = object.transform.position + Vec3::new(offset, 0.2, 0.0);
            let mut body = None;
            if let Some(handle) = object.body {
                if let Some(src) = engine.world.body(handle) {
                    let shape = src.shape.clone();
                    let kind = if src.is_dynamic() {
                        BodyKind::Dynamic
                    } else {
                        BodyKind::Static
                    };
                    let builder = match kind {
                        BodyKind::Dynamic => BodyBuilder::dynamic(shape),
                        _ => BodyBuilder::r#static(shape),
                    };
                    body = Some(engine.world.add_builder(builder.position(position)));
                }
            }
            let copy = match body {
                Some(handle) => engine.scene.add_with_body(
                    object.mesh,
                    object.material,
                    handle,
                    &engine.world,
                    &name,
                ),
                None => engine.scene.add_named(
                    object.mesh,
                    object.material,
                    Transform::new(position, object.transform.rotation, object.transform.scale),
                    &name,
                ),
            };
            engine.scene.objects[copy].color = object.color;
            engine.scene.objects[copy].metallic = object.metallic;
            engine.scene.objects[copy].roughness = object.roughness;
            engine.scene.objects[copy].emissive_strength = object.emissive_strength;
            // 复制体跟原物体**同一个父物体**（同在父坐标系里，偏移量也在这个坐标系里；
            // 父物体带旋转时偏移方向会跟着转，这是"同级复制"该有的行为）。
            engine.scene.objects[copy].parent = object.parent;
            made.push(copy);
        }
        let count = made.len();
        let mut iter = made.into_iter();
        self.select_only(iter.next());
        self.extra_selected.extend(iter);
        self.status = if count == 1 {
            format!(
                "已复制 {}",
                self.selected_object(engine)
                    .map(|o| o.name.clone())
                    .unwrap_or_default()
            )
        } else {
            format!("已复制 {count} 个物体")
        };
    }

    /// 给物体换父物体（`parent` 为 `None` 就是提成根物体）。
    ///
    /// **保持世界位姿不变**：先记下世界变换，改完父子关系再写回去 —— 否则物体
    /// 一换父就会"跳"到父坐标系里去（改层级时位置突然变了，是最容易被当成 bug 的行为）。
    pub fn set_parent(&mut self, engine: &mut Engine, child: usize, parent: Option<usize>) -> bool {
        if child >= engine.scene.objects.len() {
            return false;
        }
        let world = engine.scene.world_transform(child);
        match engine.scene.set_parent(child, parent) {
            Ok(()) => {
                engine.scene.set_world_transform(child, world);
                let child_name = engine.scene.objects[child].name.clone();
                self.status = match parent {
                    Some(p) => {
                        format!("已把 {child_name} 挂到 {} 下", engine.scene.objects[p].name)
                    }
                    None => format!("已把 {child_name} 提为根物体"),
                };
                true
            }
            Err(e) => {
                self.status = format!("改父物体失败：{e}");
                false
            }
        }
    }

    /// 删掉选中的物体（连同它们的刚体）；多选时一次删完，算一步历史。
    pub fn delete_selected(&mut self, engine: &mut Engine) {
        let mut indices = self.selection();
        indices.retain(|&i| i < engine.scene.objects.len());
        indices.sort_unstable();
        indices.dedup();
        let Some(&first) = indices.first() else {
            self.select_only(None);
            return;
        };
        let count = indices.len();
        let mut names = Vec::with_capacity(count);
        let mut children = 0usize;
        let mut was_model_mesh = false;
        // 从后往前删：前面的下标不会因为后面的删除而改变。
        for &index in indices.iter().rev() {
            if index >= engine.scene.objects.len() {
                continue;
            }
            was_model_mesh |= self.model_of_object(index).is_some();
            children += engine.scene.children_of(index).len();
            let Some(object) = engine.scene.remove(index) else {
                continue;
            };
            names.push(object.name.clone());
            if let Some(handle) = object.body {
                engine.world.remove_body(handle);
            }
            self.forget_object(index);
        }
        names.reverse();
        let next = if engine.scene.objects.is_empty() {
            None
        } else {
            Some(first.min(engine.scene.objects.len() - 1))
        };
        self.select_only(next);
        if count == 1 {
            self.status = format!("已删除 {}", names.first().cloned().unwrap_or_default());
        } else {
            self.status = format!("已删除 {count} 个物体：{}", names.join("、"));
        }
        if children > 0 {
            self.status
                .push_str(&format!("（{children} 个子物体提到了上一级）"));
        }
        if was_model_mesh {
            self.status
                .push_str("（模型引用还在文件里；这些网格存盘后重开会随模型回来）");
        }
    }

    /// 清空场景（地面也不留）。
    pub fn clear(&mut self, engine: &mut Engine) {
        engine.scene.clear();
        engine.world.clear();
        self.model_instances.clear();
        self.select_only(None);
        self.capture_baseline(engine);
        self.status = "已清空场景".to_string();
    }

    /// 换刚体类型（动态 / 静态 / 仅显示）。
    pub fn set_body_kind(&mut self, engine: &mut Engine, kind: BodyKind) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(object) = engine.scene.objects.get(index) else {
            return;
        };
        let transform = object.transform;
        let old = object.body;
        let shape = old
            .and_then(|h| engine.world.body(h))
            .map(|b| b.shape.clone())
            .unwrap_or_else(|| Primitive::Cube.shape());
        if let Some(handle) = old {
            engine.world.remove_body(handle);
        }
        let new_body = match kind {
            BodyKind::Dynamic => Some(
                engine
                    .world
                    .add_builder(BodyBuilder::dynamic(shape).transform(transform)),
            ),
            BodyKind::Static => Some(
                engine
                    .world
                    .add_builder(BodyBuilder::r#static(shape).transform(transform)),
            ),
            BodyKind::Visual => None,
        };
        engine.scene.objects[index].body = new_body;
        self.status = format!("刚体类型 -> {}", kind.label());
    }

    /// 把场景里的变换推回物理世界（手动拖动物体后调）。
    pub fn push_transform_to_physics(&self, engine: &mut Engine) {
        engine.scene.sync_to_physics(&mut engine.world);
    }

    /// 从物理世界拉回变换（模拟后调；`auto_sync_physics` 打开时引擎自己做）。
    pub fn pull_transform_from_physics(&self, engine: &mut Engine) {
        let world = &engine.world;
        engine.scene.sync_from_physics(world);
    }

    /// 屏幕射线拾取：返回最近命中的对象下标。
    ///
    /// 用**网格包围盒**（`Scene::bounds_of`）而不是三角形：编辑器要的是
    /// "点到谁"，不是像素级精确，而且对哪些网格都成立（含导入模型）。
    pub fn pick(&self, engine: &Engine, ray: &Ray) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (index, object) in engine.scene.objects.iter().enumerate() {
            if !object.visible || engine.scene.is_particle_object(index) {
                continue;
            }
            let Some(bounds) = engine.scene.bounds_of(&engine.renderer, &[index]) else {
                continue;
            };
            // 细小物体（薄板、细杆）的包围盒可能退化成一条线，撑开一点更好点。
            let bounds = bounds.grown(0.02);
            if let Some(t) = ray.intersect_aabb(&bounds) {
                if best.map(|(_, bt)| t < bt).unwrap_or(true) {
                    best = Some((index, t));
                }
            }
        }
        best.map(|(index, _)| index)
    }

    /// 框选：把**屏幕中心点**落在 `min..max` 矩形里的可见物体选进来。
    ///
    /// `min/max` 是相对视口左上角的像素坐标，`viewport` 是视口像素尺寸。只按物体
    /// **中心**判断（不做逐三角形/包围盒嵌套），所以大物体的边缘可能落在框内但中心
    /// 在框外——这是有意的便宜实现，和 Blender 的"框选中心"口径一致。
    ///
    /// `additive` 为真时保留已有选择、把新命中追加进去（Shift/Ctrl 拖动）；为假时
    /// 只选命中项，空框会清空选择。返回命中数。
    pub fn select_in_rect(
        &mut self,
        engine: &Engine,
        viewport: Vec2,
        min: Vec2,
        max: Vec2,
        additive: bool,
    ) -> usize {
        let lo = Vec2::new(min.x.min(max.x), min.y.min(max.y));
        let hi = Vec2::new(min.x.max(max.x), min.y.max(max.y));
        let camera = engine.camera();
        let mut hits = Vec::new();
        for (index, object) in engine.scene.objects.iter().enumerate() {
            if !object.visible
                || engine.scene.is_particle_object(index)
                || object.fluid_particle.is_some()
                || object.fluid_surface.is_some()
            {
                continue;
            }
            let world = engine.scene.world_transform(index).position;
            let Some(p) = camera.world_to_screen(world, viewport) else {
                continue;
            };
            if p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y {
                hits.push(index);
            }
        }
        if !additive {
            self.selected = hits.first().copied();
            self.extra_selected = hits.iter().skip(1).copied().collect();
        } else {
            if self.selected.is_none() {
                self.selected = hits.first().copied();
            }
            for &index in &hits {
                if !self.is_selected(index) {
                    self.extra_selected.push(index);
                }
            }
        }
        if !hits.is_empty() {
            self.status = if additive {
                format!(
                    "框选追加 {} 个物体（共 {} 个）",
                    hits.len(),
                    self.selection().len()
                )
            } else {
                format!("框选 {} 个物体", hits.len())
            };
        } else if !additive {
            self.status = "框选：没有命中物体".to_string();
        }
        hits.len()
    }

    /// 生成不重名的对象名（`球`、`球 2`、`球 3`…）。
    fn unique_name(&mut self, engine: &Engine, base: &str) -> String {
        // 先试原名，再试 `base N`；同时用自增计数兜底，避免反复扫场景。
        if engine.scene.find(base).is_none() {
            return base.to_string();
        }
        loop {
            self.spawn_counter += 1;
            let candidate = format!("{base} {}", self.spawn_counter + 1);
            if engine.scene.find(&candidate).is_none() {
                return candidate;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 工具栏里每个图元都必须三件套齐全：名字、物理形状、"有体积"。
    ///
    /// 这条挡住的是"加了枚举值但只接了一半"：`Primitive::ALL` 驱动工具栏按钮，
    /// `shape()` 驱动物理；漏掉任何一个，按钮点下去要么没反应、要么建出个退化刚体。
    /// （`mesh()` 要 `AssetCache`，那要真 GPU，所以不在这里；单位网格与物理形状的
    /// 对账在 `crates/quench3d/tests/unit_primitives.rs`。）
    #[test]
    fn every_primitive_has_a_label_and_a_real_shape() {
        assert_eq!(Primitive::ALL.len(), 5, "工具栏图元数量变了？");
        for p in Primitive::ALL {
            assert!(!p.label().is_empty(), "{p:?} 没有名字");
            let shape = p.shape();
            assert!(shape.volume() > 0.0, "{p:?} 体积非正：{shape:?}");
            assert!(shape.local_aabb().is_valid(), "{p:?} AABB 非法");
            assert!(!shape.is_static_only(), "{p:?} 不该是静态专用形状");
        }
    }

    #[test]
    fn multi_selection_toggle_keeps_a_primary_and_dedups() {
        let mut doc = Doc::new();
        doc.toggle_selected(3);
        assert_eq!(doc.selected, Some(3));
        assert!(doc.extra_selected.is_empty());

        doc.toggle_selected(5);
        doc.toggle_selected(7);
        assert_eq!(doc.selection(), vec![3, 5, 7]);
        assert!(doc.is_selected(5));

        // 再点一次 5：从附加里移除，主选中不变。
        doc.toggle_selected(5);
        assert_eq!(doc.selection(), vec![3, 7]);

        // 点主选中：把第一个附加提为主选中，而不是整组清空。
        doc.toggle_selected(3);
        assert_eq!(doc.selected, Some(7));
        assert!(doc.extra_selected.is_empty());

        doc.toggle_selected(7);
        assert_eq!(doc.selected, None);

        doc.select_only(Some(1));
        assert_eq!(doc.selection(), vec![1]);
        doc.select_only(None);
        assert!(doc.selection().is_empty());
    }

    /// 圆锥（第 61 轮补的）确实被接上了，且尺寸与单位网格一致（半径 0.5、半高 0.5）。
    #[test]
    fn cone_primitive_is_wired_like_the_others() {
        assert!(Primitive::ALL.contains(&Primitive::Cone));
        assert_eq!(Primitive::Cone.label(), "圆锥");
        match Primitive::Cone.shape() {
            Shape::Cone {
                half_height,
                radius,
            } => {
                assert!((half_height - 0.5).abs() < 1e-6, "{half_height}");
                assert!((radius - 0.5).abs() < 1e-6, "{radius}");
            }
            other => panic!("圆锥的物理形状不对：{other:?}"),
        }
        let aabb = Primitive::Cone.shape().local_aabb();
        assert!((aabb.max.x - 0.5).abs() < 1e-6 && (aabb.max.y - 0.5).abs() < 1e-6);
        assert!((aabb.min.y + 0.5).abs() < 1e-6);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn set_animation_params_writes_animation_and_current_animator_state() {
        let mut doc = Doc::new();
        doc.base_doc = Some(
            serde_json::from_value(serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [
                    {"path": "a.glb", "animation": {"name": "idle", "speed": 1.0, "loop": true}},
                    {"path": "b.glb", "animator": {
                        "initial": "idle",
                        "states": [
                            {"name": "idle", "animation": "idle"},
                            {"name": "run", "animation": "run", "speed": 1.0, "loop": true}
                        ]
                    }}
                ]
            }))
            .expect("场景 JSON"),
        );
        doc.set_model_instances(vec![
            quench3d_cli::runner::ModelInstance {
                model: 0,
                path: "a.glb".into(),
                objects: vec![1],
            },
            quench3d_cli::runner::ModelInstance {
                model: 1,
                path: "b.glb".into(),
                objects: vec![2],
            },
        ]);

        let path = doc
            .set_animation_params(&[1], None, 2.5, false)
            .expect("普通 animation 应当写回");
        assert_eq!(path, "a.glb");
        let base = doc.base_doc.as_ref().expect("底稿");
        let animation = base.models[0].animation.as_ref().expect("animation");
        assert_eq!(animation.speed, Some(2.5));
        assert_eq!(animation.looping, Some(false));

        let path = doc
            .set_animation_params(&[2], Some("run"), 0.5, true)
            .expect("状态机当前状态应当写回");
        assert_eq!(path, "b.glb");
        let base = doc.base_doc.as_ref().expect("底稿");
        let animator = base.models[1].animator.as_ref().expect("animator");
        let run = animator.states.iter().find(|s| s.name == "run").unwrap();
        assert_eq!(run.speed, Some(0.5));
        assert_eq!(run.looping, Some(true));
        // 未知状态 / 未知物体都不能误写。
        assert!(doc
            .set_animation_params(&[2], Some("nope"), 3.0, false)
            .is_none());
        assert!(doc.set_animation_params(&[99], None, 3.0, false).is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn set_animation_clip_only_writes_plain_animation_segment() {
        let mut doc = Doc::new();
        doc.base_doc = Some(
            serde_json::from_value(serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [
                    {"path": "a.glb", "animation": {"name": "idle"}},
                    {"path": "b.glb", "animator": {
                        "initial": "idle",
                        "states": [{"name": "idle", "animation": "idle"}]
                    }}
                ]
            }))
            .expect("场景 JSON"),
        );
        doc.set_model_instances(vec![
            quench3d_cli::runner::ModelInstance {
                model: 0,
                path: "a.glb".into(),
                objects: vec![1],
            },
            quench3d_cli::runner::ModelInstance {
                model: 1,
                path: "b.glb".into(),
                objects: vec![2],
            },
        ]);
        assert_eq!(
            doc.set_animation_clip(&[1], "run").as_deref(),
            Some("a.glb")
        );
        assert_eq!(
            doc.base_doc.as_ref().unwrap().models[0]
                .animation
                .as_ref()
                .unwrap()
                .name,
            "run"
        );
        // 状态机没有可单独换的 animation.name；不能把 states 结构改坏。
        assert!(doc.set_animation_clip(&[2], "run").is_none());
        assert!(doc.set_animation_clip(&[99], "run").is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn set_animator_transition_writes_trigger_fields() {
        let mut doc = Doc::new();
        doc.base_doc = Some(
            serde_json::from_value(serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{"path": "a.glb", "animator": {
                    "initial": "idle",
                    "states": [
                        {"name": "idle", "animation": "idle"},
                        {"name": "run", "animation": "run"}
                    ],
                    "transitions": [
                        {"from": "idle", "to": "run", "after": 0.0, "fade": 0.0}
                    ]
                }}]
            }))
            .expect("场景 JSON"),
        );
        doc.set_model_instances(vec![quench3d_cli::runner::ModelInstance {
            model: 0,
            path: "a.glb".into(),
            objects: vec![7],
        }]);
        assert_eq!(
            doc.set_animator_transition(&[7], 0, 1.5, 0.25, true)
                .as_deref(),
            Some("a.glb")
        );
        let transition = &doc.base_doc.as_ref().unwrap().models[0]
            .animator
            .as_ref()
            .unwrap()
            .transitions[0];
        assert!((transition.after - 1.5).abs() < 1e-6);
        assert_eq!(transition.fade, Some(0.25));
        assert!(transition.on_finished);
        // 越界/未知物体不能误写。
        assert!(doc
            .set_animator_transition(&[7], 9, 0.0, 0.0, false)
            .is_none());
        assert!(doc
            .set_animator_transition(&[99], 0, 0.0, 0.0, false)
            .is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn set_morph_weights_writes_and_removes_override() {
        let mut doc = Doc::new();
        doc.base_doc = Some(
            serde_json::from_value(serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{"path": "morph.glb"}]
            }))
            .expect("场景 JSON"),
        );
        doc.set_model_instances(vec![quench3d_cli::runner::ModelInstance {
            model: 0,
            path: "morph.glb".into(),
            objects: vec![4],
        }]);
        assert_eq!(
            doc.set_morph_weights(&[4], Some(vec![0.25, 0.75]))
                .as_deref(),
            Some("morph.glb")
        );
        assert_eq!(
            doc.base_doc.as_ref().unwrap().models[0].morph_weights,
            Some(vec![0.25, 0.75])
        );
        assert!(doc.set_morph_weights(&[4], None).is_some());
        assert_eq!(doc.base_doc.as_ref().unwrap().models[0].morph_weights, None);
        assert!(doc.set_morph_weights(&[99], Some(vec![0.0])).is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn set_ik_target_writes_the_model_chain() {
        let mut doc = Doc::new();
        doc.base_doc = Some(
            serde_json::from_value(serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{
                    "path": "rig.glb",
                    "animation": {"name": "idle"},
                    "ik": [{
                        "root": "thigh", "mid": "shin", "tip": "foot",
                        "target": [0.0, 0.0, 0.0]
                    }]
                }]
            }))
            .expect("场景 JSON"),
        );
        doc.set_model_instances(vec![quench3d_cli::runner::ModelInstance {
            model: 0,
            path: "rig.glb".into(),
            objects: vec![4],
        }]);
        assert_eq!(
            doc.set_ik_target(&[4], 0, [0.25, 0.5, -1.0]).as_deref(),
            Some("rig.glb")
        );
        assert_eq!(
            doc.base_doc.as_ref().unwrap().models[0]
                .ik
                .as_ref()
                .unwrap()[0]
                .target,
            [0.25, 0.5, -1.0]
        );
        assert!(doc.set_ik_target(&[4], 1, [0.0; 3]).is_none());
        assert!(doc.set_ik_target(&[99], 0, [0.0; 3]).is_none());
        assert_eq!(doc.set_ik_weight(&[4], 0, 0.25).as_deref(), Some("rig.glb"));
        assert_eq!(
            doc.base_doc.as_ref().unwrap().models[0]
                .ik
                .as_ref()
                .unwrap()[0]
                .weight,
            Some(0.25)
        );
        assert!(doc.set_ik_weight(&[4], 2, 0.5).is_none());
        assert!(doc.set_ik_weight(&[99], 0, 0.5).is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn set_animation_events_writes_and_clears_the_model_events() {
        let mut doc = Doc::new();
        doc.base_doc = Some(
            serde_json::from_value(serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{
                    "path": "hero.glb",
                    "animation": {"name": "walk"},
                    "events": [{"name": "old", "time": 0.1}]
                }]
            }))
            .expect("场景 JSON"),
        );
        doc.set_model_instances(vec![quench3d_cli::runner::ModelInstance {
            model: 0,
            path: "hero.glb".into(),
            objects: vec![4],
        }]);
        assert_eq!(
            doc.set_animation_events(
                &[4],
                vec![quench3d_cli::scene_doc::ModelEventDoc {
                    name: "footstep".into(),
                    time: 0.35,
                    clip: None,
                    payload: Some("step=2".into()),
                }],
            )
            .as_deref(),
            Some("hero.glb")
        );
        let events = doc.base_doc.as_ref().unwrap().models[0]
            .events
            .as_ref()
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "footstep");
        assert!((events[0].time - 0.35).abs() < 1e-6);
        // 空数组应清掉整段，而不是留下 `events: []`。
        assert!(doc.set_animation_events(&[4], Vec::new()).is_some());
        assert!(doc.base_doc.as_ref().unwrap().models[0].events.is_none());
        assert!(doc.set_animation_events(&[99], Vec::new()).is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn set_animator_state_writes_clip_and_start() {
        let mut doc = Doc::new();
        doc.base_doc = Some(
            serde_json::from_value(serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{"path": "a.glb", "animator": {
                    "initial": "idle",
                    "states": [{"name": "idle", "animation": "idle"}]
                }}]
            }))
            .expect("场景 JSON"),
        );
        doc.set_model_instances(vec![quench3d_cli::runner::ModelInstance {
            model: 0,
            path: "a.glb".into(),
            objects: vec![8],
        }]);
        assert_eq!(
            doc.set_animator_state(&[8], 0, "run", 0.75).as_deref(),
            Some("a.glb")
        );
        let state = &doc.base_doc.as_ref().unwrap().models[0]
            .animator
            .as_ref()
            .unwrap()
            .states[0];
        assert_eq!(state.animation, "run");
        assert_eq!(state.start, Some(0.75));
        assert!(doc.set_animator_state(&[8], 9, "run", 0.0).is_none());
        assert!(doc.set_animator_state(&[99], 0, "run", 0.0).is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn add_remove_animator_states_and_transitions() {
        let mut doc = Doc::new();
        doc.base_doc = Some(
            serde_json::from_value(serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{"path": "a.glb", "animator": {
                    "initial": "idle",
                    "states": [
                        {"name": "idle", "animation": "idle"},
                        {"name": "walk", "animation": "walk"}
                    ],
                    "transitions": [
                        {"from": "idle", "to": "walk", "after": 0.0}
                    ]
                }}]
            }))
            .expect("场景 JSON"),
        );
        doc.set_model_instances(vec![quench3d_cli::runner::ModelInstance {
            model: 0,
            path: "a.glb".into(),
            objects: vec![8],
        }]);
        assert_eq!(
            doc.add_animator_state(&[8], "run", "walk").as_deref(),
            Some("a.glb")
        );
        assert!(doc.add_animator_state(&[8], "run", "idle").is_none());
        assert_eq!(
            doc.add_animator_transition(&[8], "run", "idle", 0.5, 0.1, false)
                .as_deref(),
            Some("a.glb")
        );
        assert!(doc
            .add_animator_transition(&[8], "run", "run", 0.0, 0.0, false)
            .is_none());
        assert!(doc
            .add_animator_transition(&[8], "run", "nope", 0.0, 0.0, false)
            .is_none());
        // 删掉 idle→walk，再删 idle：引用 idle 的 run→idle 也要一起删，initial 改写。
        assert!(doc.remove_animator_transition(&[8], 0).is_some());
        assert!(doc.remove_animator_transition(&[8], 9).is_none());
        assert!(doc.remove_animator_state(&[8], 0).is_some());
        {
            let animator = doc.base_doc.as_ref().unwrap().models[0]
                .animator
                .as_ref()
                .unwrap();
            assert_eq!(
                animator
                    .states
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["walk", "run"]
            );
            assert!(animator.transitions.is_empty());
            assert_eq!(animator.initial, "walk");
        }
        assert!(doc.remove_animator_state(&[8], 0).is_some());
        assert_eq!(
            doc.base_doc.as_ref().unwrap().models[0]
                .animator
                .as_ref()
                .unwrap()
                .states
                .len(),
            1
        );
        assert!(doc.remove_animator_state(&[8], 0).is_none());
        assert!(doc.add_animator_state(&[99], "x", "x").is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn set_animator_transition_ends_rejects_self_loop() {
        let mut doc = Doc::new();
        doc.base_doc = Some(
            serde_json::from_value(serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{"path": "a.glb", "animator": {
                    "initial": "idle",
                    "states": [
                        {"name": "idle", "animation": "idle"},
                        {"name": "walk", "animation": "walk"},
                        {"name": "run", "animation": "run"}
                    ],
                    "transitions": [
                        {"from": "idle", "to": "walk", "after": 0.0}
                    ]
                }}]
            }))
            .expect("场景 JSON"),
        );
        doc.set_model_instances(vec![quench3d_cli::runner::ModelInstance {
            model: 0,
            path: "a.glb".into(),
            objects: vec![8],
        }]);
        assert_eq!(
            doc.set_animator_transition_ends(&[8], 0, "walk", "run")
                .as_deref(),
            Some("a.glb")
        );
        let transition = &doc.base_doc.as_ref().unwrap().models[0]
            .animator
            .as_ref()
            .unwrap()
            .transitions[0];
        assert_eq!(
            (transition.from.as_str(), transition.to.as_str()),
            ("walk", "run")
        );
        assert!(doc
            .set_animator_transition_ends(&[8], 0, "run", "run")
            .is_none());
        assert!(doc
            .set_animator_transition_ends(&[8], 0, "walk", "nope")
            .is_none());
        assert!(doc
            .set_animator_transition_ends(&[8], 9, "idle", "walk")
            .is_none());
        assert!(doc
            .set_animator_transition_ends(&[99], 0, "idle", "walk")
            .is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn set_animator_initial_rejects_unknown_state() {
        let mut doc = Doc::new();
        doc.base_doc = Some(
            serde_json::from_value(serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{"path": "a.glb", "animator": {
                    "initial": "idle",
                    "states": [
                        {"name": "idle", "animation": "idle"},
                        {"name": "walk", "animation": "walk"}
                    ]
                }}]
            }))
            .expect("场景 JSON"),
        );
        doc.set_model_instances(vec![quench3d_cli::runner::ModelInstance {
            model: 0,
            path: "a.glb".into(),
            objects: vec![8],
        }]);
        assert_eq!(doc.animator_initial(&[8]).as_deref(), Some("idle"));
        assert_eq!(
            doc.set_animator_initial(&[8], "walk").as_deref(),
            Some("a.glb")
        );
        assert_eq!(doc.animator_initial(&[8]).as_deref(), Some("walk"));
        assert!(doc.set_animator_initial(&[8], "nope").is_none());
        assert!(doc.set_animator_initial(&[99], "walk").is_none());
        assert!(doc.animator_initial(&[99]).is_none());
    }
}

#[cfg(test)]
mod outliner_filter_tests {
    use super::outliner_query_matches;

    #[test]
    fn outliner_query_matches_case_insensitively() {
        assert!(outliner_query_matches("Box A", ""));
        assert!(outliner_query_matches("Box A", "  "));
        assert!(outliner_query_matches("Box A", "box"));
        assert!(outliner_query_matches("箱子", "箱"));
        assert!(!outliner_query_matches("Box A", "sphere"));
        assert!(!outliner_query_matches("Box A", "box b"));
    }
}
