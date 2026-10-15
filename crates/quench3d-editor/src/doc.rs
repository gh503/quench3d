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
}

impl Primitive {
    /// 工具栏里按这个顺序排。
    pub const ALL: [Primitive; 4] = [
        Primitive::Cube,
        Primitive::Sphere,
        Primitive::Cylinder,
        Primitive::Capsule,
    ];

    /// 界面上的名字（也是物体默认名的前缀）。
    pub fn label(self) -> &'static str {
        match self {
            Primitive::Cube => "立方体",
            Primitive::Sphere => "球",
            Primitive::Cylinder => "圆柱",
            Primitive::Capsule => "胶囊",
        }
    }

    /// 用哪个预设网格。
    pub fn mesh(self, assets: &AssetCache) -> MeshId {
        match self {
            Primitive::Cube => assets.unit_cube,
            Primitive::Sphere => assets.unit_sphere,
            Primitive::Cylinder => assets.unit_cylinder,
            Primitive::Capsule => assets.unit_capsule,
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

    /// 空文档。
    pub fn new() -> Self {
        Self {
            selected: None,
            file_path: "scene.json".to_string(),
            status: "就绪".to_string(),
            show_help: false,
            history: crate::history::History::default(),
            textures: std::collections::HashMap::new(),
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
            Some(Ok((count, snapshot))) => {
                self.refresh_base_from(&snapshot);
                self.clamp_selection(engine);
                self.status = format!("已撤销 → {count} 个物体");
            }
            Some(Err(e)) => self.status = format!("撤销失败：{e}"),
            None => self.status = "没有可撤销的步骤".to_string(),
        }
    }

    /// 重做一步（含状态提示与选区收敛）。
    pub fn redo(&mut self, engine: &mut Engine) {
        match self.history.redo(engine) {
            Some(Ok((count, snapshot))) => {
                self.refresh_base_from(&snapshot);
                self.clamp_selection(engine);
                self.status = format!("已重做 → {count} 个物体");
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
        self.selected = Some(index);
        self.status = format!("已复制 {name}");
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
        let object = engine.scene.objects.remove(index);
        if let Some(handle) = object.body {
            engine.world.remove_body(handle);
        }
        self.selected = if engine.scene.objects.is_empty() {
            None
        } else {
            Some(index.min(engine.scene.objects.len() - 1))
        };
        self.status = format!("已删除 {}", object.name);
    }

    /// 清空场景（地面也不留）。
    pub fn clear(&mut self, engine: &mut Engine) {
        engine.scene.clear();
        engine.world.clear();
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
