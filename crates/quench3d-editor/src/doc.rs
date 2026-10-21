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
    /// UV 平铺次数。
    pub uv_scale: Option<f32>,
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

/// 编辑器文档层：场景内容的可撤销副本 + 界面状态（选中、状态栏、帮助开关）。
pub struct Doc {
    /// 当前选中的场景对象下标（`None` = 没选）。
    pub selected: Option<usize>,
    /// 场景文件路径（桌面用真实路径；浏览器里当下载文件名）。
    pub file_path: String,
    /// 最近一条给用户看的状态提示。
    pub status: String,
    /// 是否显示帮助窗口。
    pub show_help: bool,
    /// 撤销 / 重做历史（整场景快照，见 [`crate::history`]）。
    pub history: crate::history::History,
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
            });
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
            file_path: "scene.json".to_string(),
            status: "就绪".to_string(),
            show_help: false,
            history: crate::history::History::default(),
            textures: std::collections::HashMap::new(),
            model_instances: Vec::new(),
            base_doc: None,
            baseline: Vec::new(),
            spawn_counter: 0,
        }
    }

    /// 编辑完成之后记一步历史（与栈顶相同则自动跳过）。
    pub fn record_edit(&mut self, engine: &Engine) {
        let json = scene_json(engine, self);
        self.history.record_json(json);
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

    /// 撤销/重做之后，底稿文档也要跟着快照走 —— 否则"撤销回到带光照的场景、
    /// 再存一次"会把光照丢掉。
    fn refresh_base_from(&mut self, snapshot: &str) {
        self.base_doc = quench3d_cli::scene_doc::SceneDoc::from_json(snapshot).ok();
    }

    /// 以当前场景为起点重开历史（打开文件 / 装载新场景后调用）。
    pub fn reset_history(&mut self, engine: &Engine) {
        let json = scene_json(engine, self);
        self.history.reset_json(json);
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
        if let Some(index) = self.selected {
            if index >= engine.scene.len() {
                self.selected = None;
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
        quench3d_cli::runner::apply_lighting(engine, &no_lighting);

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
        self.selected = Some(1);
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
        self.selected = Some(index);
        self.status = format!("已添加 {name}");
        index
    }

    /// 复制当前选中的物体（位置往旁边挪一点，免得完全重叠）。
    pub fn duplicate_selected(&mut self, engine: &mut Engine) {
        let Some(object) = self.selected_object(engine).cloned() else {
            return;
        };
        let name = self.unique_name(engine, &object.name);
        let position = object.transform.position + Vec3::new(1.2, 0.2, 0.0);
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
        let index = match body {
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
        engine.scene.objects[index].color = object.color;
        engine.scene.objects[index].metallic = object.metallic;
        engine.scene.objects[index].roughness = object.roughness;
        engine.scene.objects[index].emissive_strength = object.emissive_strength;
        // 复制体跟原物体**同一个父物体**（同在父坐标系里，偏移量也在这个坐标系里；
        // 父物体带旋转时偏移方向会跟着转，这是"同级复制"该有的行为）。
        engine.scene.objects[index].parent = object.parent;
        self.selected = Some(index);
        self.status = format!("已复制 {name}");
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

    /// 删掉选中的物体（连同它的刚体）。
    pub fn delete_selected(&mut self, engine: &mut Engine) {
        let Some(index) = self.selected else {
            return;
        };
        if index >= engine.scene.objects.len() {
            self.selected = None;
            return;
        }
        // 删的是模型展开出来的网格吗？删之前先问清楚（删完下标就变了）。
        let was_model_mesh = self.model_of_object(index).is_some();
        // 子物体数也要在删之前数（删完它们的父下标已经变了）。
        let children = engine.scene.children_of(index).len();
        // 用 `scene.remove`：它会把子物体提到祖父、并把后面所有物体的父下标前移。
        let Some(object) = engine.scene.remove(index) else {
            return;
        };
        if let Some(handle) = object.body {
            engine.world.remove_body(handle);
        }
        // 物体下标整体前移：模型映射必须跟着挪，否则"拖模型"会拖到别人身上。
        self.forget_object(index);
        self.selected = if engine.scene.objects.is_empty() {
            None
        } else {
            Some(index.min(engine.scene.objects.len() - 1))
        };
        self.status = format!("已删除 {}", object.name);
        if children > 0 {
            self.status
                .push_str(&format!("（{children} 个子物体提到了上一级）"));
        }
        if was_model_mesh {
            self.status
                .push_str("（模型引用还在文件里；这个网格存盘后重开会随模型回来）");
        }
    }

    /// 清空场景（地面也不留）。
    pub fn clear(&mut self, engine: &mut Engine) {
        engine.scene.clear();
        engine.world.clear();
        self.model_instances.clear();
        self.selected = None;
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
            if !object.visible {
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
}
