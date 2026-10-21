//! 场景：把"网格 + 材质 + 变换 + 可选物理刚体"组织成一个可提交给渲染器的列表。
//!
//! 这是引擎里最贴近"搭场景"的抽象：加一个物体、挂一个刚体、
//! 每帧自动从物理世界同步位姿，然后一次性提交绘制。
//!
//! 物体可以有**父物体**（[`SceneObject::parent`]），于是 `transform` 是**局部**变换，
//! 世界变换由 [`Scene::world_transform`] 沿父链现算 —— 拖动父物体时子物体自动跟着走。
//! 三条边界写死在这里：子物体只继承位置与朝向、**不继承缩放**（`rigid_only` 解释了为什么）；
//! 带刚体的物体不能有父物体（位姿归物理管）；删除物体要调 [`Scene::remove`]
//! （它负责把子孙提到祖父、保持它们的世界位姿、并把后面的父下标前移）。

use std::collections::HashMap;
use std::sync::Arc;

use quench3d_math::{Mat4, Quat, Transform, Vec2, Vec3, Vec4};
use quench3d_physics::{BodyHandle, PhysicsWorld};
use quench3d_render::{InstanceDesc, Lighting, MaterialDesc, MaterialId, MeshId, Renderer};

/// 场景中的一个实例。
#[derive(Clone, Debug)]
pub struct SceneObject {
    /// 名称（调试与查找用）。
    pub name: String,
    /// 网格句柄。
    pub mesh: MeshId,
    /// 材质句柄。
    pub material: MaterialId,
    /// **局部**变换：有父物体时是相对父物体的，没有父物体时就是世界变换。
    ///
    /// 世界变换用 [`Scene::world_transform`] 现算（沿父链把局部变换拼上去），
    /// 所以移动父物体时子物体会自动跟着走，不需要任何"同步"步骤。
    pub transform: Transform,
    /// 父物体下标（`None` = 根）。
    ///
    /// 用下标而不是名字：场景里的物体本来就是按列表存取的，名字可能重复；
    /// 名字到下标的那层解析留在场景文件（`parent` 字段）与编辑器里。
    ///
    /// 子物体继承父物体的**位置与朝向**，但**不继承缩放**（原因见 `rigid_only`）。
    pub parent: Option<usize>,
    /// 这个物体是"某个复合形状物体的第几个附加部件"（`None` = 普通物体）。
    ///
    /// 复合形状在渲染上要拆成多个绘制物体（每个部件一个网格），但它们**不是**场景里
    /// 独立的物体：场景树、导出、选择都当归主物体的一部分。这个字段就是那条"归属线"——
    /// 少了它，编辑器把场景存回去会把附加部件写成一堆独立物体（静默的数据繁殖）。
    pub owner_part: Option<usize>,
    /// 关联的物理刚体（`None` 表示纯静态显示）。
    ///
    /// **带刚体的物体必须是根**（[`Scene::set_parent`] 会拒绝，场景校验器也会报错）：
    /// 刚体的位姿由物理决定，再挂一个父物体只会让父物体的运动被静默忽略。
    pub body: Option<BodyHandle>,
    /// 是否可见。
    pub visible: bool,
    /// 颜色倍乘（线性 RGBA）。
    pub color: Vec4,
    /// 金属度覆盖。
    pub metallic: Option<f32>,
    /// 粗糙度覆盖。
    pub roughness: Option<f32>,
    /// 自发光强度。
    pub emissive_strength: f32,
    /// 粒子流体的可视绑定（`None` = 普通物体）。
    ///
    /// 有它时 [`Scene::sync_from_physics`] 直接从 `world.fluids()` 读粒子位置，
    /// 不需要把粒子伪装成刚体；物体本身不参与物理。
    pub fluid_particle: Option<FluidParticleVisual>,
    /// 粒子流体的**水面**可视绑定（`None` = 不是水面）。
    ///
    /// 每帧把水面抬到该流体粒子的最高点；它只是一张半透明平面，不是真正的
    /// 自由表面网格/屏幕空间折射（M4 的边界）。
    pub fluid_surface: Option<FluidSurfaceVisual>,
}

/// 添加粒子物体时的参数（避免 8 个位置参数）。
#[derive(Clone, Copy, Debug)]
pub struct FluidParticleSpec {
    /// 流体下标。
    pub fluid: usize,
    /// 粒子下标。
    pub particle: usize,
    /// 可视半径（米）。
    pub radius: f32,
    /// 初始位置。
    pub position: Vec3,
}

/// 添加水面物体时的参数。
#[derive(Clone, Copy, Debug)]
pub struct FluidSurfaceSpec {
    /// 流体下标。
    pub fluid: usize,
    /// 水面半尺寸 `(x, z)`。
    pub half_size: Vec2,
    /// 水面中心（只用 XZ；Y 由流体最高粒子决定）。
    pub center: Vec3,
    /// 初始高度。
    pub level: f32,
}

/// 一个粒子可视绑定：第 `fluid` 个流体的第 `particle` 颗粒子。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidParticleVisual {
    /// 流体下标（`world.fluids()` 的顺序）。
    pub fluid: usize,
    /// 粒子下标（与 `Fluid::particles()` 同序）。
    pub particle: usize,
    /// 可视半径（米）；物理半径可以更小，渲染时允许略微重叠。
    pub radius: f32,
}

/// 粒子流体的水面可视绑定。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidSurfaceVisual {
    /// 流体下标（`world.fluids()` 的顺序）。
    pub fluid: usize,
    /// 水面半尺寸 `(x, z)`（米）。
    pub half_size: Vec2,
}

impl SceneObject {
    /// 生成绘制描述（`world` 是它的**世界**变换，见 [`Scene::world_transform`]）。
    pub fn instance(&self, world: Transform) -> InstanceDesc {
        let mut d = InstanceDesc::new(world).with_color(self.color);
        d.emissive_strength = self.emissive_strength;
        if self.metallic.is_some() || self.roughness.is_some() {
            d = d.with_pbr(self.metallic.unwrap_or(0.0), self.roughness.unwrap_or(0.5));
        }
        d
    }
}

/// 第二条 clip 的混合参数：目标权重 + 一次性淡入时长（秒）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlendOptions {
    /// 淡入结束后第二条 clip 的权重（0..=1）。
    pub weight: f32,
    /// 从 0 升到 `weight` 的秒数；0 = 立即生效。
    pub fade: f32,
}

impl BlendOptions {
    /// 立即到 `weight`（`fade = 0`）。
    pub fn new(weight: f32) -> Self {
        Self { weight, fade: 0.0 }
    }

    /// 在 `fade` 秒内从 0 平滑升到 `weight`。
    pub fn with_fade(weight: f32, fade: f32) -> Self {
        Self { weight, fade }
    }
}

/// 状态机里的一个状态：一个可循环/单次的 clip。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimatorState {
    /// 状态名（场景文件里的 `name`，只用于过渡匹配）。
    pub name: String,
    /// 资产里的动画名。
    pub animation: String,
    /// 播放倍速（必须 > 0）。
    pub speed: f32,
    /// 是否循环。
    pub looping: bool,
    /// 进入状态时的起始时间（秒）。
    pub start: f32,
}

/// 状态机的一条过渡。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimatorTransition {
    /// 从哪个状态名出发。
    pub from: String,
    /// 到哪个状态名。
    pub to: String,
    /// 在当前状态停留多少秒后开始过渡。
    pub after: f32,
    /// 淡入秒数；0 = 立即切换。
    pub fade: f32,
}

/// 运行中的状态机：当前状态、已停留时间、正在淡入的目标状态。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimatorRuntime {
    /// 所有状态（顺序与场景一致）。
    pub states: Vec<ModelAnimatorState>,
    /// 所有过渡（按场景顺序匹配，先命中先用）。
    pub transitions: Vec<ModelAnimatorTransition>,
    /// 当前状态下标。
    pub current: usize,
    /// 当前状态已停留的时间（秒）。
    pub elapsed: f32,
    /// 正在淡入的目标状态下标；`None` = 不在过渡中。
    pub pending: Option<usize>,
}

/// 场景里一个"正在播放 glTF 动画"的模型实例。
///
/// 一个模型会展开成多个场景物体（每个网格片段一个），动画要把每个片段自己的矩阵
/// 乘到它**出生时**的变换上。`model` 用 `Arc` 共享：模型可能很大（顶点/贴图），
/// 而播放器只读它。
pub struct ModelAnimationInstance {
    /// 模型（只读；与 `objects` 同一份切片的来源）。
    pub model: Arc<crate::assets::LoadedModel>,
    /// 片段 → 场景物体下标（与 `base` **同序**，下标就是 `model.meshes` 下标）。
    pub objects: Vec<usize>,
    /// 片段 → 出生变换（动画的基准，**不是**上一帧结果）。
    pub base: Vec<Transform>,
    /// 播放状态（时间 / 倍速 / 循环）。
    pub player: crate::assets::ModelAnimationPlayer,
    /// 可选的**第二条**动画（交叉淡化）；`None` = 单 clip。
    pub secondary: Option<crate::assets::ModelAnimationPlayer>,
    /// 第二条动画的**当前**权重（0 = 只用第一条，1 = 只用第二条）。
    pub blend_weight: f32,
    /// 淡入结束时的目标权重（缺省 = `blend_weight`，即不淡入）。
    pub blend_target: f32,
    /// 淡入时长（秒）；0 = 立即到 `blend_target`。
    pub blend_fade: f32,
    /// 淡入已累计的时间（秒）。
    pub blend_elapsed: f32,
    /// 场景覆盖的 morph 权重；`None` = 用资产的默认值。
    pub morph_weights: Option<Vec<f32>>,
    /// 可选的状态机；`None` = 单/双 clip 播放。
    pub animator: Option<ModelAnimatorRuntime>,
}

/// 取一个动画实例当前该用的节点世界矩阵（单 clip 或双 clip 混合）。
fn animation_worlds(a: &ModelAnimationInstance) -> Option<Vec<Mat4>> {
    match &a.secondary {
        Some(secondary) => a
            .model
            .blended_node_worlds(&a.player, secondary, a.blend_weight),
        None => a.player.node_worlds(&a.model),
    }
}

/// 取一个动画实例当前该用的逐网格刚性矩阵。
fn animation_matrices(a: &ModelAnimationInstance) -> Vec<Mat4> {
    match animation_worlds(a) {
        Some(worlds) => a.model.mesh_matrices_from_worlds(&worlds),
        None => vec![Mat4::IDENTITY; a.model.meshes.len()],
    }
}

/// 状态机的一步：推进当前状态计时，命中过渡就建立淡入（或立即切换）。
///
/// `dt` 非法时按 0 处理；`fade` 非法时按 0；过渡按场景中的顺序取第一条命中的。
fn advance_animator(a: &mut ModelAnimationInstance, dt: f32) {
    // 过渡中不再启动下一条过渡；等 promotion 把新 clip 提为主 clip。
    if a.secondary.is_some() {
        return;
    }
    let step = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
    let Some(anim) = &mut a.animator else {
        return;
    };
    let current_name = anim.states.get(anim.current).map(|s| s.name.clone());
    let Some(current_name) = current_name else {
        return;
    };
    // 过渡在**帧首**判定：本帧开始时已经停留够 after 才切。这样目标 clip
    // 不会被多推进一整帧——它本帧只从 0 开始走这一帧的时间。
    let transition = anim.transitions.iter().find(|t| {
        t.from == current_name && t.after.is_finite() && t.after >= 0.0 && anim.elapsed >= t.after
    });
    let Some(transition) = transition else {
        anim.elapsed += step;
        return;
    };
    let target = anim.states.iter().position(|s| s.name == transition.to);
    let Some(target) = target else {
        return;
    };
    let fade = if transition.fade.is_finite() {
        transition.fade.max(0.0)
    } else {
        0.0
    };
    let state = anim.states[target].clone();
    let Some(mut player) = a.model.player(&state.animation) else {
        return;
    };
    player.speed = state.speed;
    player.looping = state.looping;
    player.time = state.start;
    player.advance(0.0);
    if fade <= 0.0 {
        a.player = player;
        if let Some(anim) = &mut a.animator {
            anim.current = target;
            anim.elapsed = 0.0;
            anim.pending = None;
        }
        a.blend_weight = 0.0;
        a.blend_target = 0.0;
        a.blend_fade = 0.0;
        a.blend_elapsed = 0.0;
    } else {
        a.secondary = Some(player);
        a.blend_weight = 0.0;
        a.blend_target = 1.0;
        a.blend_fade = fade;
        a.blend_elapsed = 0.0;
        if let Some(anim) = &mut a.animator {
            anim.pending = Some(target);
        }
    }
}

/// 淡入结束后把 `secondary` 提为主 clip，并让状态机重新计时。
fn promote_animator(a: &mut ModelAnimationInstance) {
    let Some(target) = a.animator.as_ref().and_then(|x| x.pending) else {
        return;
    };
    if a.secondary.is_none() {
        return;
    }
    if a.blend_fade > 0.0 && a.blend_elapsed + 1e-6 < a.blend_fade {
        return;
    }
    if let Some(player) = a.secondary.take() {
        a.player = player;
    }
    a.blend_weight = 0.0;
    a.blend_target = 0.0;
    a.blend_fade = 0.0;
    a.blend_elapsed = 0.0;
    if let Some(anim) = &mut a.animator {
        anim.current = target;
        anim.elapsed = 0.0;
        anim.pending = None;
    }
}

/// 场景。
pub struct Scene {
    /// 全部物体。
    pub objects: Vec<SceneObject>,
    /// 光照与天空环境。
    pub lighting: Lighting,
    /// 物理步进后是否自动把刚体位姿同步到物体。
    pub auto_sync_physics: bool,
    /// 正在播放的模型动画（每帧 [`Scene::advance_animations`] 推进）。
    ///
    /// 空 = 静态场景：`advance_animations` 一次循环都不进，逐字节零开销。
    pub animations: Vec<ModelAnimationInstance>,
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene {
    /// 空场景。
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
            lighting: Lighting::default(),
            auto_sync_physics: true,
            animations: Vec::new(),
        }
    }

    /// 添加一个物体。
    pub fn add(&mut self, mesh: MeshId, material: MaterialId, transform: Transform) -> usize {
        self.add_named(mesh, material, transform, "object")
    }

    /// 添加并命名。
    pub fn add_named(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        transform: Transform,
        name: &str,
    ) -> usize {
        let index = self.objects.len();
        self.objects.push(SceneObject {
            name: name.to_string(),
            mesh,
            material,
            transform,
            parent: None,
            owner_part: None,
            body: None,
            visible: true,
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
            metallic: None,
            roughness: None,
            emissive_strength: 1.0,
            fluid_particle: None,
            fluid_surface: None,
        });
        index
    }

    /// 添加并绑定物理刚体。
    pub fn add_with_body(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        body: BodyHandle,
        world: &PhysicsWorld,
        name: &str,
    ) -> usize {
        let transform = world.body(body).map(|b| b.transform()).unwrap_or_default();
        let index = self.add_named(mesh, material, transform, name);
        self.objects[index].body = Some(body);
        index
    }

    /// 添加一颗粒子的可视物体（不参与物理，由 `sync_from_physics` 跟随流体）。
    pub fn add_fluid_particle(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        spec: FluidParticleSpec,
        name: &str,
    ) -> usize {
        let radius = spec.radius.max(1e-4);
        let transform = Transform::new(spec.position, Quat::IDENTITY, Vec3::splat(radius * 2.0));
        let index = self.add_named(mesh, material, transform, name);
        self.objects[index].fluid_particle = Some(FluidParticleVisual {
            fluid: spec.fluid,
            particle: spec.particle,
            radius,
        });
        index
    }

    /// 添加一个跟随流体最高点的水面（半透明平面）。
    pub fn add_fluid_surface(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        spec: FluidSurfaceSpec,
        name: &str,
    ) -> usize {
        let transform = Transform::new(
            Vec3::new(spec.center.x, spec.level, spec.center.z),
            Quat::IDENTITY,
            Vec3::new(spec.half_size.x * 2.0, 1.0, spec.half_size.y * 2.0),
        );
        let index = self.add_named(mesh, material, transform, name);
        self.objects[index].fluid_surface = Some(FluidSurfaceVisual {
            fluid: spec.fluid,
            half_size: spec.half_size,
        });
        index
    }

    /// 按名称查找。
    pub fn find(&self, name: &str) -> Option<usize> {
        self.objects.iter().position(|o| o.name == name)
    }

    /// 按名称获取可变引用。
    pub fn get_mut(&mut self, name: &str) -> Option<&mut SceneObject> {
        self.objects.iter_mut().find(|o| o.name == name)
    }

    /// 按刚体句柄查找物体下标。
    pub fn find_by_body(&self, body: BodyHandle) -> Option<usize> {
        self.objects.iter().position(|o| o.body == Some(body))
    }

    /// 清空。
    ///
    /// 动画登记也一起清：不清的话，重建场景后旧的"片段 → 物体下标"会指向新场景里
    /// **别的**物体（典型症状：重新载入场景后，某块地板开始自己走）。
    pub fn clear(&mut self) {
        self.objects.clear();
        self.animations.clear();
    }

    /// 物体数量。
    /// 把某个模型动画播放器**当前帧**应用到它占用的物体上；返回真正更新的物体数。
    ///
    /// * `objects` 与 `model.meshes` 同序（就是 `add_model`/`spawn` 返回的下标）；
    /// * `base` 是这些物体**创建时**的基础变换 —— 动画结果是 `base × 额外矩阵`，
    ///   **不是**在上一帧结果上继续乘（那样会漂）；
    /// * 越界下标跳过（不 panic）；没有动画的片段矩阵是单位矩阵 → 物体保持原样。
    ///
    /// 这一步是纯 CPU 的：`MeshId`/`MaterialId` 只是句柄，不需要 GPU 上下文，
    /// 所以"动画到底动了什么"可以在没有显卡的测试里断言。
    pub fn apply_animation(
        &mut self,
        model: &crate::assets::LoadedModel,
        objects: &[usize],
        base: &[Transform],
        player: &crate::assets::ModelAnimationPlayer,
    ) -> usize {
        let mats = player.mesh_matrices(model);
        apply_matrices(&mut self.objects, objects, base, &mats)
    }

    /// 登记一个会播放的模型动画，返回登记序号。
    ///
    /// **名字不存在时不要调它**（`model.player(name)` 返回 `None`）：把几何摆到
    /// "绑定姿态以外的某处"或静默不播都不该由这里猜。调用方拿到 `None` 时应当在
    /// 报告里说明（`simulate` / `render` 的 `models[].animation.found = false`）。
    pub fn play_model_animation(
        &mut self,
        model: Arc<crate::assets::LoadedModel>,
        objects: Vec<usize>,
        base: Vec<Transform>,
        player: crate::assets::ModelAnimationPlayer,
    ) -> usize {
        let index = self.animations.len();
        self.animations.push(ModelAnimationInstance {
            model,
            objects,
            base,
            player,
            secondary: None,
            blend_weight: 0.0,
            blend_target: 0.0,
            blend_fade: 0.0,
            blend_elapsed: 0.0,
            morph_weights: None,
            animator: None,
        });
        index
    }

    /// 登记**两条**动画并交叉淡化：`weight` 是第二条的权重（0..=1）。
    ///
    /// 与 [`Self::play_model_animation`] 的区别只在采样：两条 clip 各自推进自己的
    /// 时间，再对局部 TRS 插值（旋转 slerp），最后只做一次父子累乘。蒙皮网格用混合后的
    /// 节点世界矩阵重算顶点；没有第二条时仍走原来的单 clip 路径。
    ///
    /// 名字不存在时**不要**调它；调用方应先确认两条 clip 都在资产里，否则模型会
    /// 保持绑定姿态（与单 clip 的契约一致）。
    pub fn play_model_animation_blend(
        &mut self,
        model: Arc<crate::assets::LoadedModel>,
        objects: Vec<usize>,
        base: Vec<Transform>,
        primary: crate::assets::ModelAnimationPlayer,
        secondary: crate::assets::ModelAnimationPlayer,
        weight: f32,
    ) -> usize {
        self.play_model_animation_blend_with_fade(
            model,
            objects,
            base,
            primary,
            secondary,
            BlendOptions::new(weight),
        )
    }

    /// 与 [`Self::play_model_animation_blend`] 相同，但第二条的权重在
    /// `options.fade` 秒内从 0 **平滑**升到 `options.weight`（smoothstep）。
    /// `fade = 0` 就是立即生效。
    ///
    /// 语义是"从主 clip 淡入到混合姿态"：第一帧完全是第一条，`fade` 秒后到达
    /// `weight`。适合行走→奔跑这类一次性过渡；不做循环淡入、也不做权重曲线编辑。
    pub fn play_model_animation_blend_with_fade(
        &mut self,
        model: Arc<crate::assets::LoadedModel>,
        objects: Vec<usize>,
        base: Vec<Transform>,
        primary: crate::assets::ModelAnimationPlayer,
        secondary: crate::assets::ModelAnimationPlayer,
        options: BlendOptions,
    ) -> usize {
        let target = if options.weight.is_finite() {
            options.weight.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let fade = if options.fade.is_finite() {
            options.fade.max(0.0)
        } else {
            0.0
        };
        let index = self.animations.len();
        self.animations.push(ModelAnimationInstance {
            model,
            objects,
            base,
            player: primary,
            secondary: Some(secondary),
            blend_weight: if fade > 0.0 { 0.0 } else { target },
            blend_target: target,
            blend_fade: fade,
            blend_elapsed: 0.0,
            morph_weights: None,
            animator: None,
        });
        index
    }

    /// 用状态机播放模型动画；返回动画实例下标，任何状态引用的 clip 不存在时返回 `None`。
    ///
    /// 状态机只按**停留时间**过渡：当前状态停留 `transition.after` 秒后开始淡入目标状态，
    /// `fade = 0` 立即切换；命中多条时按 `transitions` 顺序取第一条。过渡期间旧的
    /// `player` 仍是主 clip，新的在 `secondary` 里 0→1 淡入；淡入完成后把新 clip
    /// 提为主 clip，状态机重新计时。这是纯 CPU 的时间逻辑，不依赖渲染器。
    pub fn play_model_animator(
        &mut self,
        model: Arc<crate::assets::LoadedModel>,
        objects: Vec<usize>,
        base: Vec<Transform>,
        states: Vec<ModelAnimatorState>,
        transitions: Vec<ModelAnimatorTransition>,
        initial: usize,
    ) -> Option<usize> {
        if states.is_empty() || initial >= states.len() {
            return None;
        }
        // 先确认每个状态的 clip 都存在：状态机不能等切过去才发现名字写错。
        for state in &states {
            model.player(&state.animation)?;
        }
        let first = &states[initial];
        let mut player = model.player(&first.animation)?;
        player.speed = first.speed;
        player.looping = first.looping;
        player.time = first.start;
        player.advance(0.0);
        let index = self.animations.len();
        self.animations.push(ModelAnimationInstance {
            model,
            objects,
            base,
            player,
            secondary: None,
            blend_weight: 0.0,
            blend_target: 0.0,
            blend_fade: 0.0,
            blend_elapsed: 0.0,
            morph_weights: None,
            animator: Some(ModelAnimatorRuntime {
                states,
                transitions,
                current: initial,
                elapsed: 0.0,
                pending: None,
            }),
        });
        Some(index)
    }

    /// 覆盖某个动画实例的 morph 权重（下标来自 `play_model_animation*`）。
    ///
    /// 只影响几何；权重为 `None` 时仍用资产默认值。没有 morph target 的模型
    /// 调用它没有副作用。返回 `false` 表示下标不存在。
    pub fn set_model_morph_weights(
        &mut self,
        animation_index: usize,
        weights: Option<Vec<f32>>,
    ) -> bool {
        let Some(a) = self.animations.get_mut(animation_index) else {
            return false;
        };
        a.morph_weights = weights;
        true
    }

    /// 推进所有模型动画 `dt` 秒并把当前帧落到物体上；返回更新的物体数。
    ///
    /// * 调用点：窗口帧循环与无头逐帧循环都**每帧一次**（暂停时不调）；
    ///   单帧渲染调 `dt = 0.0`，等于"按播放器当前时间摆一次姿态"（时间初值是 `start`）。
    /// * 顺序在 `sync_from_physics` **之后**：物理先定位，动画再叠加；模型展开的物体
    ///   没有刚体（模型整体只有可选的静态三角网格碰撞体），两者不会互相覆盖。
    /// * 没有登记任何动画时直接返回 0，不进循环（静态场景零开销）。
    pub fn advance_animations(&mut self, dt: f32) -> usize {
        if self.animations.is_empty() {
            return 0;
        }
        // 拆成两个字段借用：动画表（读模型/播放器）与物体表（写变换）互不重叠。
        let Scene {
            objects,
            animations,
            ..
        } = self;
        let mut updated = 0;
        for a in animations.iter_mut() {
            advance_animator(a, dt);
            a.player.advance(dt);
            if let Some(secondary) = &mut a.secondary {
                secondary.advance(dt);
                if a.blend_fade > 0.0 {
                    let step = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
                    a.blend_elapsed = (a.blend_elapsed + step).min(a.blend_fade);
                    let u = (a.blend_elapsed / a.blend_fade).clamp(0.0, 1.0);
                    // smoothstep：两端一阶导为 0，过渡不会在起止处"顿一下"。
                    a.blend_weight = a.blend_target * (u * u * (3.0 - 2.0 * u));
                } else {
                    a.blend_weight = a.blend_target;
                }
            }
            promote_animator(a);
            let mats = animation_matrices(a);
            updated += apply_matrices(objects, &a.objects, &a.base, &mats);
        }
        updated
    }

    /// **按当前播放时间**重新摆位姿，不推进时间。
    ///
    /// 编辑器的拖动条会直接改 `player.time`，然后调用它让视口当场更新；
    /// 状态机的 `elapsed` / 过渡都不动（拖动不改状态机语义）。
    pub fn refresh_animations(&mut self) -> usize {
        if self.animations.is_empty() {
            return 0;
        }
        let Scene {
            objects,
            animations,
            ..
        } = self;
        let mut updated = 0;
        for a in animations.iter() {
            let mats = animation_matrices(a);
            updated += apply_matrices(objects, &a.objects, &a.base, &mats);
        }
        updated
    }

    /// 按当前时间刷新位姿并**把蒙皮/morph 顶点写回 GPU**。
    ///
    /// 供编辑器在暂停时拖动时间轴；正常帧循环仍应使用
    /// [`Scene::advance_animations_with_renderer`]。
    pub fn refresh_animations_with_renderer(
        &mut self,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
    ) -> usize {
        let updated = self.refresh_animations();
        self.upload_animated_vertices(renderer, ctx);
        updated
    }

    /// 推进动画并**把蒙皮顶点写回 GPU**；窗口/无头渲染都应该用这个入口。
    ///
    /// 变换与顶点更新分开：`advance_animations` 只摆物体的刚性位姿（绑定姿态时是
    /// 单位矩阵），这里再按关节矩阵重算蒙皮顶点。没有蒙皮网格时第二步是空循环，
    /// 与旧路径的可见行为逐位一致。`renderer.update_mesh_vertices` 保留同一个
    /// `MeshId`，所以编辑器选择、材质绑定、包围盒引用都不需要换句柄。
    pub fn advance_animations_with_renderer(
        &mut self,
        dt: f32,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
    ) -> usize {
        let updated = self.advance_animations(dt);
        self.upload_animated_vertices(renderer, ctx);
        updated
    }

    /// 把当前 `player.time` / morph 权重算出的变形顶点写回 GPU。
    fn upload_animated_vertices(
        &mut self,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
    ) {
        if !self
            .animations
            .iter()
            .any(|a| a.model.has_skinning() || a.model.has_morph_targets())
        {
            return;
        }
        let Scene {
            objects,
            animations,
            ..
        } = self;
        for a in animations.iter() {
            let Some(worlds) = animation_worlds(a) else {
                continue;
            };
            // morph 动画按节点给权重；场景里手工写的 `morph_weights` 优先级更高，
            // 用于覆盖资产动画（此时动画的 morph 轨道被忽略）。双 clip 混合时按
            // `blend_weight` 逐 target 插值两条 clip 的权重（见
            // `LoadedModel::blended_morph_weights_at`）。
            let sampled = match &a.secondary {
                Some(secondary) => a.model.blended_morph_weights_at(
                    &a.player.animation,
                    &secondary.animation,
                    a.blend_weight,
                    a.player.time,
                    secondary.time,
                ),
                None => a.model.morph_weights_at(&a.player.animation, a.player.time),
            };
            for (mesh_index, &object_index) in a.objects.iter().enumerate() {
                let Some(mesh) = a.model.meshes.get(mesh_index) else {
                    continue;
                };
                // 只有蒙皮/morph 片段需要每帧重传；普通刚性片段仍靠实例矩阵。
                if mesh.skin.is_none() && mesh.morph_targets.is_empty() {
                    continue;
                }
                let weights: Option<&[f32]> = if let Some(manual) = &a.morph_weights {
                    Some(manual.as_slice())
                } else {
                    mesh.node.and_then(|node| {
                        sampled
                            .as_ref()?
                            .iter()
                            .find(|(n, _)| *n == node)
                            .map(|(_, w)| w.as_slice())
                    })
                };
                let Some(data) = a.model.deformed_mesh(mesh_index, &worlds, weights) else {
                    continue;
                };
                let Some(object) = objects.get_mut(object_index) else {
                    continue;
                };
                renderer.update_mesh_vertices(ctx, object.mesh, &data.vertices);
            }
        }
    }

    /// 物体数量。
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// 从物理世界同步所有已绑定物体的位姿（保留缩放）。
    pub fn sync_from_physics(&mut self, world: &PhysicsWorld) {
        for obj in &mut self.objects {
            // 带刚体的物体必然是根（`set_parent` 拒绝给带刚体的物体挂父物体），
            // 所以它的局部变换就是世界变换，直接写没问题。
            debug_assert!(
                obj.body.is_none() || obj.parent.is_none(),
                "带刚体的物体 \"{}\" 不该有父物体",
                obj.name
            );
            if obj.parent.is_some() {
                continue;
            }
            if let Some(visual) = obj.fluid_surface {
                if let Some(f) = world.fluids().get(visual.fluid) {
                    let level = f
                        .particles()
                        .iter()
                        .map(|p| p.y)
                        .fold(f32::NEG_INFINITY, f32::max);
                    if level.is_finite() {
                        obj.transform.position.y = level;
                    }
                }
                continue;
            }
            if let Some(visual) = obj.fluid_particle {
                if let Some(fluid) = world.fluids().get(visual.fluid) {
                    if let Some(p) = fluid.particles().get(visual.particle) {
                        obj.transform.position = *p;
                        obj.transform.rotation = Quat::IDENTITY;
                        obj.transform.scale = Vec3::splat(visual.radius * 2.0);
                        // 速度越快越偏青白：静止的水是深蓝，水花有一点亮边。
                        let speed = fluid
                            .velocities()
                            .get(visual.particle)
                            .map(|v| v.length())
                            .unwrap_or(0.0);
                        let tint = (speed * 0.05).clamp(0.0, 0.35);
                        obj.color = Vec4::new(0.10 + tint, 0.34 + tint * 0.6, 0.72, 1.0);
                    }
                }
                continue;
            }
            if let Some(handle) = obj.body {
                if let Some(body) = world.body(handle) {
                    let scale = obj.transform.scale;
                    obj.transform = body.transform();
                    obj.transform.scale = scale;
                }
            }
        }
    }

    /// 把物体位姿写回物理世界（移动静态物体或做"抓取"时有用）。
    pub fn sync_to_physics(&self, world: &mut PhysicsWorld) {
        for (i, obj) in self.objects.iter().enumerate() {
            if let Some(handle) = obj.body {
                // 用世界变换：带刚体的物体是根，这里等价于 `obj.transform`，
                // 但写成世界变换后，"以后允许给刚体挂父物体"不会变成一个静默的错。
                if let Some(body) = world.body_mut(handle) {
                    body.set_transform(self.world_transform(i));
                }
            }
        }
    }

    /// 提交全部可见物体到渲染器。
    pub fn submit(&self, renderer: &mut Renderer) {
        for (i, obj) in self.objects.iter().enumerate() {
            if obj.visible {
                renderer.draw(
                    obj.mesh,
                    obj.material,
                    obj.instance(self.world_transform(i)),
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // 层级（父子）
    // -----------------------------------------------------------------------

    /// 物体的**世界**变换：沿父链把局部变换逐级拼上去。
    ///
    /// **只继承位置与朝向，不继承缩放**（见 `rigid_only`）。父链断掉（下标越界）或
    /// 成环时立刻停下返回当前结果 —— 宁给一个能用的值，也不要在这里死循环
    /// （界面每帧都会调它）。
    pub fn world_transform(&self, index: usize) -> Transform {
        let Some(obj) = self.objects.get(index) else {
            return Transform::IDENTITY;
        };
        let mut world = obj.transform;
        let mut cursor = obj.parent;
        let mut steps = 0usize;
        while let Some(p) = cursor {
            let Some(parent) = self.objects.get(p) else {
                break; // 断链：按当前结果返回（校验器会先一步报错）
            };
            world = rigid_only(&parent.transform).mul_transform(&world);
            cursor = parent.parent;
            steps += 1;
            if steps > self.objects.len() {
                break; // 成环：不转圈
            }
        }
        world
    }

    /// 把世界变换写回物体（层级里存的是局部变换，这里做一次换算）。
    ///
    /// 返回 `false` 表示下标越界，什么都没改。
    pub fn set_world_transform(&mut self, index: usize, world: Transform) -> bool {
        let Some(obj) = self.objects.get(index) else {
            return false;
        };
        let local = match obj.parent {
            None => world,
            Some(p) => rigid_only(&self.world_transform(p)).child_of(&world),
        };
        self.objects[index].transform = local;
        true
    }

    /// 设置父物体（`None` = 变成根）；成功时**不改动**物体的世界位姿之外的东西。
    ///
    /// 注意：调用方通常想"保持世界位姿不变"，那需要自己先取 [`Scene::world_transform`]、
    /// 设完父物体后调 [`Scene::set_world_transform`] 写回去。
    ///
    /// 拒绝三种情况：自身、成环、以及"给带刚体的物体挂父物体"
    /// （刚体位姿由物理决定，父物体只会被静默忽略 —— 这种"看着像生效了"的错误最难查）。
    pub fn set_parent(&mut self, child: usize, parent: Option<usize>) -> Result<(), String> {
        if child >= self.objects.len() {
            return Err(format!("物体下标 {child} 越界"));
        }
        if let Some(p) = parent {
            if p >= self.objects.len() {
                return Err(format!("父物体下标 {p} 越界"));
            }
            if p == child {
                return Err("物体不能以自己为父".to_string());
            }
            if self.is_ancestor(child, p) {
                return Err(format!(
                    "父子关系会成环：\"{}\" 已经是 \"{}\" 的祖先",
                    self.objects[child].name, self.objects[p].name
                ));
            }
            if self.objects[child].body.is_some() {
                return Err(format!(
                    "\"{}\" 带刚体，位姿由物理决定，不能挂到父物体下",
                    self.objects[child].name
                ));
            }
        }
        self.objects[child].parent = parent;
        Ok(())
    }

    /// `ancestor` 是不是 `node` 的祖先（含间接；不含自己）。
    pub fn is_ancestor(&self, ancestor: usize, node: usize) -> bool {
        let mut cursor = self.objects.get(node).and_then(|o| o.parent);
        let mut steps = 0usize;
        while let Some(p) = cursor {
            if p == ancestor {
                return true;
            }
            cursor = self.objects.get(p).and_then(|o| o.parent);
            steps += 1;
            if steps > self.objects.len() {
                break;
            }
        }
        false
    }

    /// 直接子物体（按场景顺序）。
    pub fn children_of(&self, parent: usize) -> Vec<usize> {
        self.objects
            .iter()
            .enumerate()
            .filter(|(_, o)| o.parent == Some(parent))
            .map(|(i, _)| i)
            .collect()
    }

    /// 删除一个物体：返回被删的物体。
    ///
    /// 直接子物体**提到它的父级**（删父不连带删子），并且**保持世界位姿不变** ——
    /// 只挪父下标的话，子物体会"跳"到祖父坐标系里去（父物体有位移时尤其明显）。
    /// 其余物体排在删除位置之后的，父下标也会跟着整体前移 —— 这一步必须由场景自己做，
    /// 调用方手工 `objects.remove(i)` 会让所有排在后面的父子关系全部错位。
    pub fn remove(&mut self, index: usize) -> Option<SceneObject> {
        if index >= self.objects.len() {
            return None;
        }
        // 删之前先记下子物体的世界变换（删完父链就断了，算不出来了）。
        let promoted: Vec<(usize, Transform)> = self
            .children_of(index)
            .into_iter()
            .map(|child| (child, self.world_transform(child)))
            .collect();

        let removed = self.objects.remove(index);
        let grandparent = removed.parent.map(|g| if g > index { g - 1 } else { g });
        for obj in &mut self.objects {
            match obj.parent {
                Some(p) if p == index => obj.parent = grandparent,
                Some(p) if p > index => obj.parent = Some(p - 1),
                _ => {}
            }
        }
        // 再把世界变换写回去（`set_world_transform` 会按**新的**父物体换算成局部量）。
        for (child, world) in promoted {
            let shifted = if child > index { child - 1 } else { child };
            self.set_world_transform(shifted, world);
        }
        Some(removed)
    }

    /// 场景包围盒（用于自动对焦相机与自适应阴影范围）。
    /// 一组物体的合并包围盒（用于对准相机或做范围查询）。
    pub fn bounds_of(&self, renderer: &Renderer, indices: &[usize]) -> Option<quench3d_math::Aabb> {
        let mut bounds = quench3d_math::Aabb::EMPTY;
        for &i in indices {
            let Some(obj) = self.objects.get(i) else {
                continue;
            };
            let (_, _, local) = renderer.mesh_info(obj.mesh)?;
            bounds.merge(&local.transformed(&self.world_transform(i).matrix()));
        }
        if bounds.is_valid() {
            Some(bounds)
        } else {
            None
        }
    }

    /// 整个场景（包含所有可见物体）的包围盒。
    pub fn bounds(&self, renderer: &Renderer) -> Option<quench3d_math::Aabb> {
        let mut bounds = quench3d_math::Aabb::EMPTY;
        for (i, obj) in self.objects.iter().enumerate() {
            if let Some((_, _, local)) = renderer.mesh_info(obj.mesh) {
                bounds.merge(&local.transformed(&self.world_transform(i).matrix()));
            }
        }
        if bounds.is_valid() {
            Some(bounds)
        } else {
            None
        }
    }
}

/// 取一个变换的"刚性部分"：位置 + 朝向，**缩放按 1 算**。
///
/// 场景层级刻意**不继承缩放**。这个引擎里物体的 `scale` 就是图元的尺寸
/// （场景文件的 `shape.size` / `radius` 会被折算成它）：如果按标准 TRS
/// 乘法继承，把一棵树挂到 `size: [20, 0.2, 20]` 的地面下，树会立刻被压成一块饼 ——
/// 而"挂到地板上"恰恰是最常见的用法。
///
/// 所以 `scale` 的含义是"**这个物体**自己多大"，不是"这一组多大"。
/// 想整体缩放一组物体，得逐个改（编辑器里也是这个语义）。
/// 组合与反解都走这一个函数，保证"世界→局部→世界"是精确往返。
fn rigid_only(t: &Transform) -> Transform {
    Transform::new(t.position, t.rotation, Vec3::ONE)
}

/// 便捷构造器：用一组预设网格/材质快速搭建场景。
///
/// 单位圆锥网格（底面半径 0.5、高 1，**中心在原点**：底面 y=-0.5、顶点 y=+0.5）。
///
/// `MeshData::cone` 的约定是「底面在 y=0、顶点在 y=height」，而场景里的图元一律
/// **中心在原点**（这样 `parts[].rotation` 绕的才是几何中心）。这里平移一次，与物理
/// `Shape::Cone { half_height: 0.5, radius: 0.5 }`（底面 -h、顶点 +h）对齐；
/// 两者的 AABB 由 `tests/unit_primitives.rs` 逐轴对账。
pub fn unit_cone_mesh() -> quench3d_render::MeshData {
    quench3d_render::MeshData::cone(0.5, 1.0, 28)
        .transformed(&Mat4::from_translation(quench3d_math::vec3(0.0, -0.5, 0.0)))
}

/// 内部缓存了常用图元与纯色材质，避免重复上传。
pub struct AssetCache {
    meshes: HashMap<String, MeshId>,
    materials: HashMap<String, MaterialId>,
    /// 单位立方体（边长为 1，中心在原点）。
    pub unit_cube: MeshId,
    /// 单位球（半径 0.5）。
    pub unit_sphere: MeshId,
    /// 单位圆柱（半径 0.5，高 1）。
    pub unit_cylinder: MeshId,
    /// 单位胶囊（半径 0.5，总高 2）。
    pub unit_capsule: MeshId,
    /// 单位圆锥（底面半径 0.5、高 1，**以质心为原点**：底面 y=-0.5、顶点 y=+0.5）。
    pub unit_cone: MeshId,
    /// 单位平面（XZ 平面，边长 1）。
    pub unit_plane: MeshId,
}

impl AssetCache {
    /// 用常用图元初始化。
    pub fn new(renderer: &mut Renderer, ctx: &quench3d_render::GpuContext) -> Self {
        let unit_cube = renderer.add_mesh_named(ctx, &quench3d_render::MeshData::cube(1.0), "cube");
        let unit_sphere = renderer.add_mesh_named(
            ctx,
            &quench3d_render::MeshData::sphere(0.5, 32, 20),
            "sphere",
        );
        let unit_cylinder = renderer.add_mesh_named(
            ctx,
            &quench3d_render::MeshData::cylinder(0.5, 0.5, 28),
            "cylinder",
        );
        let unit_capsule = renderer.add_mesh_named(
            ctx,
            &quench3d_render::MeshData::capsule(0.5, 0.5, 28, 20),
            "capsule",
        );
        let unit_cone = renderer.add_mesh_named(ctx, &unit_cone_mesh(), "cone");
        let unit_plane =
            renderer.add_mesh_named(ctx, &quench3d_render::MeshData::plane(1.0, 1), "plane");
        Self {
            meshes: HashMap::new(),
            materials: HashMap::new(),
            unit_cube,
            unit_sphere,
            unit_cylinder,
            unit_capsule,
            unit_cone,
            unit_plane,
        }
    }

    /// 上传一个命名网格（同名复用）。
    pub fn mesh(
        &mut self,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
        name: &str,
        data: &quench3d_render::MeshData,
    ) -> MeshId {
        if let Some(id) = self.meshes.get(name) {
            return *id;
        }
        let id = renderer.add_mesh_named(ctx, data, name);
        self.meshes.insert(name.to_string(), id);
        id
    }

    /// 创建纯色 PBR 材质（相同参数复用）。
    ///
    /// `color` 按 **sRGB**（也就是「屏幕上看得到的颜色」）解释，
    /// 例如 `Vec4::new(0.85, 0.25, 0.20, 1.0)` 是砖红色。
    pub fn solid_material(
        &mut self,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
        color: Vec4,
        metallic: f32,
        roughness: f32,
    ) -> MaterialId {
        let linear = Vec4::new(
            quench3d_render::color::srgb_to_linear(color.x),
            quench3d_render::color::srgb_to_linear(color.y),
            quench3d_render::color::srgb_to_linear(color.z),
            color.w,
        );
        self.solid_material_linear(renderer, ctx, linear, metallic, roughness)
    }

    /// 创建纯色 PBR 材质，`color` 为**线性**空间数值。
    pub fn solid_material_linear(
        &mut self,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
        color: Vec4,
        metallic: f32,
        roughness: f32,
    ) -> MaterialId {
        let key = format!(
            "solid:{:.4},{:.4},{:.4},{:.4}|{:.2}|{:.2}",
            color.x, color.y, color.z, color.w, metallic, roughness
        );
        if let Some(id) = self.materials.get(&key) {
            return *id;
        }
        let desc = MaterialDesc::new(color).pbr(metallic, roughness);
        let id = renderer.add_material_named(ctx, desc, &key);
        self.materials.insert(key, id);
        id
    }

    /// 注册一个自定义材质（带贴图等）。
    pub fn material(
        &mut self,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
        name: &str,
        desc: MaterialDesc,
    ) -> MaterialId {
        if let Some(id) = self.materials.get(name) {
            return *id;
        }
        let id = renderer.add_material_named(ctx, desc, name);
        self.materials.insert(name.to_string(), id);
        id
    }

    /// 缓存统计。
    pub fn cached_counts(&self) -> (usize, usize) {
        (self.meshes.len(), self.materials.len())
    }
}

/// 由物体缩放推断常用物理形状的辅助函数。
pub fn box_half_extents(scale: Vec3) -> Vec3 {
    scale.abs() * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use quench3d_math::Quat;

    /// 纯逻辑测试：层级只碰 `Scene` 自己，不需要 GPU（网格/材质句柄就是 `u32`）。
    /// 编辑器那边也有一份带真实装载的集成测试，但那些在没显卡的机器上会跳过，
    /// 这一份保证"没显卡也要能验证层级"。
    fn scene_with_chain() -> Scene {
        let mut scene = Scene::new();
        let m = 1u32;
        // 父：位置 (10,0,0)、绕 Y 转 90°、**非均匀**缩放 (2,0.5,3)
        let p = scene.add_named(
            m,
            m,
            Transform::new(
                Vec3::new(10.0, 0.0, 0.0),
                Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                Vec3::new(2.0, 0.5, 3.0),
            ),
            "父",
        );
        // 子：局部位置 (0,1,0)、自己的缩放 (1,2,1)
        let c = scene.add_named(
            m,
            m,
            Transform::new(
                Vec3::new(0.0, 1.0, 0.0),
                Quat::IDENTITY,
                Vec3::new(1.0, 2.0, 1.0),
            ),
            "子",
        );
        scene.set_parent(c, Some(p)).expect("挂父物体应当成功");
        assert_eq!(scene.find("父"), Some(p));
        scene
    }

    #[test]
    fn world_transform_inherits_position_and_rotation_but_not_scale() {
        let scene = scene_with_chain();
        let child = scene.find("子").unwrap();
        let world = scene.world_transform(child);
        // "父"绕 Y 转 90°：局部的 (0,1,0) 在世界里还是 (0,1,0)（Y 轴旋转不动 Y 轴）。
        assert!(
            (world.position - Vec3::new(10.0, 1.0, 0.0)).length() < 1e-5,
            "{:?}",
            world.position
        );
        // 缩放是子物体自己的：父物体的 (2,0.5,3) **不**乘进来。
        assert_eq!(world.scale, Vec3::new(1.0, 2.0, 1.0));
        // 朝向被子物体继承（父物体绕 Y 转了 90°）。比四元数而不是欧拉角：
        // 欧拉角反解在轴附近有 1e-5 量级的抖动，写 `== 90.0` 会在别的平台上红
        // （macOS 上就真红过一次：90.00001 != 90.0）。
        let expected = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        assert!(
            world.rotation.dot(expected).abs() > 0.9999,
            "子物体没有继承父物体的朝向：{:?}",
            world.rotation
        );
        // 根物体的世界变换就是局部变换。
        let parent = scene.find("父").unwrap();
        assert_eq!(
            scene.world_transform(parent).scale,
            Vec3::new(2.0, 0.5, 3.0)
        );
    }

    #[test]
    fn moving_the_parent_moves_the_child() {
        let mut scene = scene_with_chain();
        let parent = scene.find("父").unwrap();
        let child = scene.find("子").unwrap();
        let before_local = scene.objects[child].transform;
        let mut world = scene.world_transform(parent);
        world.position += Vec3::new(0.0, 3.0, 0.0);
        assert!(scene.set_world_transform(parent, world));
        assert_eq!(
            scene.world_transform(child).position,
            Vec3::new(10.0, 4.0, 0.0)
        );
        assert_eq!(
            scene.objects[child].transform.position,
            before_local.position
        );
    }

    #[test]
    fn world_and_local_conversions_are_exact_inverses() {
        let mut scene = scene_with_chain();
        let child = scene.find("子").unwrap();
        let target = Transform::new(
            Vec3::new(-3.0, 2.5, 7.0),
            Quat::from_rotation_x(0.4),
            Vec3::new(0.5, 0.5, 0.5),
        );
        assert!(scene.set_world_transform(child, target));
        // 写回去的世界变换与给的一致（中间的局部量不需要人来算）。
        let back = scene.world_transform(child);
        assert!((back.position - target.position).length() < 1e-5);
        assert!((back.scale - target.scale).length() < 1e-5);
    }

    #[test]
    fn set_parent_rejects_cycles_and_bodies() {
        let mut scene = scene_with_chain();
        let parent = scene.find("父").unwrap();
        let child = scene.find("子").unwrap();
        // 自己
        assert!(scene.set_parent(parent, Some(parent)).is_err());
        // 成环：把父挂到自己的子下面
        assert!(scene.set_parent(parent, Some(child)).is_err());
        assert_eq!(scene.objects[parent].parent, None, "失败不能改坏层级");
        // 越界
        assert!(scene.set_parent(99, Some(parent)).is_err());
        assert!(scene.set_parent(child, Some(99)).is_err());
        // 带刚体的物体（用一个假句柄）不能挂父物体 —— 位姿归物理管
        let body = scene.add(1u32, 1u32, Transform::IDENTITY);
        scene.objects[body].body = Some(quench3d_physics::BodyHandle::new(0, 0));
        assert!(scene.set_parent(body, Some(parent)).is_err());
        // children_of / is_ancestor
        assert_eq!(scene.children_of(parent), vec![child]);
        assert!(scene.is_ancestor(parent, child));
        assert!(!scene.is_ancestor(child, parent));
    }

    #[test]
    fn remove_promotes_children_without_moving_them() {
        let mut scene = scene_with_chain();
        let parent = scene.find("父").unwrap();
        let child = scene.find("子").unwrap();
        let before = scene.world_transform(child);
        // 后面再加一个物体，验证下标前移
        let tail = scene.add(1, 1, Transform::IDENTITY);
        scene.set_parent(tail, Some(parent)).expect("挂父物体");

        let removed = scene.remove(parent).expect("父应当存在");
        assert_eq!(removed.name, "父");
        // 子物体成了根物体（父物体自己就是根），但世界位置没动。
        let child_now = scene.find("子").unwrap();
        assert_eq!(scene.objects[child_now].parent, None);
        assert!(
            (scene.world_transform(child_now).position - before.position).length() < 1e-5,
            "{:?} vs {:?}",
            scene.world_transform(child_now).position,
            before.position
        );
        // 排在后面的物体下标前移一位（`add` 的缺省名就是 "object"）。
        let tail_now = scene
            .find("object")
            .unwrap_or_else(|| panic!("尾物体应当还在，实际有 {}", scene.len()));
        assert!(tail_now < tail);
        assert_eq!(scene.len(), 2);
    }

    #[test]
    fn fluid_visuals_follow_the_particle_cloud() {
        use quench3d_physics::FluidDesc;

        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_fluid(FluidDesc {
            center: Vec3::new(0.0, 2.0, 0.0),
            half_extents: Vec3::splat(0.3),
            spacing: 0.1,
            radius: 0.05,
            ..FluidDesc::default()
        });
        for _ in 0..5 {
            world.step(1.0 / 60.0);
        }
        let fluid = &world.fluids()[0];
        let expected = fluid.particles()[0];
        let top = fluid
            .particles()
            .iter()
            .map(|p| p.y)
            .fold(f32::MIN, f32::max);

        let mut scene = Scene::new();
        let p = scene.add_named(1u32, 1u32, Transform::IDENTITY, "粒子");
        scene.objects[p].fluid_particle = Some(FluidParticleVisual {
            fluid: 0,
            particle: 0,
            radius: 0.07,
        });
        let s = scene.add_named(1u32, 1u32, Transform::IDENTITY, "水面");
        scene.objects[s].fluid_surface = Some(FluidSurfaceVisual {
            fluid: 0,
            half_size: Vec2::new(0.3, 0.3),
        });

        scene.sync_from_physics(&world);
        assert_eq!(scene.objects[p].transform.position, expected);
        assert_eq!(scene.objects[p].transform.scale, Vec3::splat(0.14));
        assert!((scene.objects[s].transform.position.y - top).abs() < 1e-6);
    }
}

/// 把"每个片段该用的额外矩阵"落到对应物体上；返回真正更新的物体数。
///
/// `objects` 是场景物体表，`object_indices` 与 `mats` / `base` 同序（下标 = 模型片段下标）。
/// 越界物体下标跳过（不 panic）；`base` 比 `objects` 短时用物体当前变换兜底。
fn apply_matrices(
    objects: &mut [SceneObject],
    object_indices: &[usize],
    base: &[Transform],
    mats: &[Mat4],
) -> usize {
    let mut updated = 0;
    for (mesh_index, &object) in object_indices.iter().enumerate() {
        let Some(obj) = objects.get_mut(object) else {
            continue;
        };
        let m = mats.get(mesh_index).copied().unwrap_or(Mat4::IDENTITY);
        let base = base.get(mesh_index).copied().unwrap_or(obj.transform);
        obj.transform = compose(base, &m);
        updated += 1;
    }
    updated
}

/// `基础变换 × 额外矩阵`：先把基础变换变成矩阵、乘上动画矩阵、再分解回 `Transform`。
///
/// 额外矩阵来自"动画后的节点世界 × 绑定姿态世界⁻¹"，是仿射变换（旋转 + 平移，
/// 可能带缩放，但**不该有剪切**）。`Transform::from_matrix` 已经处理了分解
/// （含零缩放保护），手写一遍只会多一处出错点。
fn compose(base: Transform, m: &Mat4) -> Transform {
    Transform::from_matrix(&(base.matrix() * *m))
}

#[cfg(test)]
mod anim_tests {
    use super::*;
    use crate::assets::{
        AnimatedProperty, AnimationTrack, Interpolation, LoadedAnimation, LoadedMesh, LoadedModel,
        LoadedNode,
    };
    use quench3d_math::{Mat4, Quat};

    /// 造一个 2 网格 / 2 节点的模型：节点 1 是"滑轮"，绑定姿态在 x=+2，
    /// 动画 1 秒内把它从 x=2 移到 x=4。
    fn pulley_model() -> LoadedModel {
        let bind = Mat4::from_rotation_translation_scale(
            Vec3::new(2.0, 0.0, 0.0),
            Quat::IDENTITY,
            Vec3::ONE,
        );
        LoadedModel {
            nodes: vec![
                LoadedNode::default(),
                LoadedNode {
                    name: "滑轮".into(),
                    parent: Some(0),
                    translation: Vec3::new(2.0, 0.0, 0.0),
                    ..Default::default()
                },
            ],
            meshes: vec![
                LoadedMesh {
                    node: Some(0),
                    bind_inverse: Mat4::IDENTITY,
                    ..Default::default()
                },
                LoadedMesh {
                    node: Some(1),
                    bind_inverse: bind.inverse(),
                    ..Default::default()
                },
            ],
            animations: vec![LoadedAnimation {
                name: "转".into(),
                duration: 1.0,
                tracks: vec![AnimationTrack {
                    node: 1,
                    property: AnimatedProperty::Translation,
                    interpolation: Interpolation::Linear,
                    times: vec![0.0, 1.0],
                    values: vec![[2.0, 0.0, 0.0, 0.0], [4.0, 0.0, 0.0, 0.0]],
                    in_tangents: Vec::new(),
                    out_tangents: Vec::new(),
                }],
                morph_tracks: Vec::new(),
            }],
            ..Default::default()
        }
    }

    /// 动画必须**只**动它覆盖的节点，而且以"基础变换"为基准（不累积漂移）。
    #[test]
    fn apply_animation_moves_only_animated_meshes_from_their_base() {
        let model = pulley_model();
        let mut scene = Scene::new();
        // 两个物体：0 = 静止的地面网格，1 = 滑轮
        let o0 = scene.add_named(
            0,
            0,
            Transform::new(Vec3::ZERO, Quat::IDENTITY, Vec3::ONE),
            "地面",
        );
        let base1 = Transform::new(Vec3::new(0.0, 5.0, 0.0), Quat::IDENTITY, Vec3::ONE);
        let o1 = scene.add_named(1, 0, base1, "滑轮");
        let objects = vec![o0, o1];
        let base = vec![Transform::new(Vec3::ZERO, Quat::IDENTITY, Vec3::ONE), base1];

        let mut player = model.player("转").expect("该有播放器");
        // t = 0：还是绑定姿态 → 两个物体都不动
        let n = scene.apply_animation(&model, &objects, &base, &player);
        assert_eq!(n, 2);
        assert!((scene.objects[o1].transform.position.x - 0.0).abs() < 1e-5);
        assert!((scene.objects[o1].transform.position.y - 5.0).abs() < 1e-5);

        // t = 0.5：滑轮从 2 移到 3（+1），基础位置 y=5 保持
        player.advance(0.5);
        scene.apply_animation(&model, &objects, &base, &player);
        assert!(
            (scene.objects[o1].transform.position.x - 1.0).abs() < 1e-4,
            "滑轮该右移 1（2→3），实际 {}",
            scene.objects[o1].transform.position.x
        );
        assert!(
            (scene.objects[o1].transform.position.y - 5.0).abs() < 1e-4,
            "基础位置不能丢"
        );
        // 静止物体一分不动
        assert!(scene.objects[o0].transform.position.length() < 1e-6);

        // t = 1.0：再走半秒 → 位移 2（不是在 1 的基础上累加）
        // 注意：播放器缺省是**循环**，t=1.0 会绕回 0（位移 0）。这里关掉循环，
        // 顺便钉住"循环 vs 不循环"在场景层的可见差别。
        player.looping = false;
        player.advance(0.5);
        scene.apply_animation(&model, &objects, &base, &player);
        assert!(
            (scene.objects[o1].transform.position.x - 2.0).abs() < 1e-4,
            "t=1 时该是 +2（不累积），实际 {}",
            scene.objects[o1].transform.position.x
        );

        // 越界下标跳过、不 panic
        let n = scene.apply_animation(&model, &[9999], &[], &player);
        assert_eq!(n, 0);
    }

    /// 没有动画（或动画名对不上）时，`apply_animation` 必须是**严格无操作**。
    ///
    /// 这条守的是静态路径：模型一旦"带着一个播不了的动画名"（资产改名、编辑器存回旧名字），
    /// 绝不能把几何挪到别处 —— 那会表现成"模型突然飞到原点"，而没有任何诊断。
    #[test]
    fn apply_animation_is_a_no_op_without_a_usable_clip() {
        let mut model = pulley_model();
        let mut scene = Scene::new();
        let base = Transform::new(Vec3::new(1.0, 2.0, 3.0), Quat::IDENTITY, Vec3::ONE);
        let o = scene.add_named(0, 0, base, "滑轮");
        let objects = [o];
        let bases = [base];

        // ① 模型里根本没有任何动画：玩家手里那条名字永远匹配不上
        model.animations.clear();
        let mut player = crate::assets::ModelAnimationPlayer {
            animation: "转".into(),
            duration: 1.0,
            time: 0.3,
            speed: 1.0,
            looping: true,
            finished: false,
        };
        let n = scene.apply_animation(&model, &objects, &bases, &player);
        assert_eq!(n, 1, "下标合法就该算更新（只是矩阵是单位矩阵）");
        assert_eq!(scene.objects[o].transform.position, base.position);
        assert_eq!(scene.objects[o].transform.rotation, base.rotation);

        // ② 动画在、但玩家要的名字不存在（资产改名/旧场景）：同样不许动
        let model2 = pulley_model();
        player.animation = "已经改名了".into();
        player.time = 0.7;
        let n = scene.apply_animation(&model2, &objects, &bases, &player);
        assert_eq!(n, 1);
        assert_eq!(scene.objects[o].transform.position, base.position);

        // ③ `base` 比 `objects` 短：缺的那份用物体当前变换当基准，也不能 panic/NaN
        let n = scene.apply_animation(&model2, &objects, &[], &model2.player("转").unwrap());
        assert_eq!(n, 1);
        assert!(scene.objects[o].transform.position.is_finite());
    }

    /// 登记过的动画由 `advance_animations` 自己推时间：每帧一次，几何跟着走；
    /// 没有登记的模型一分不动；`clear` 之后旧登记不能继续改动新场景。
    #[test]
    fn advance_animations_plays_registered_models_only() {
        let model = std::sync::Arc::new(pulley_model());
        let mut scene = Scene::new();
        // 片段 0 是静止的父节点、片段 1 才是滑轮；两个都要有对应物体。
        let root = scene.add_named(0, 0, Transform::IDENTITY, "根");
        let base = Transform::new(Vec3::new(0.0, 5.0, 0.0), Quat::IDENTITY, Vec3::ONE);
        let o = scene.add_named(1, 0, base, "滑轮");
        // 同一场景里的另一个物体：不属于这个模型，它必须一分不动。
        let still = scene.add_named(2, 0, Transform::IDENTITY, "背景");

        let player = model.player("转").expect("该有播放器");
        scene.play_model_animation(
            model.clone(),
            vec![root, o],
            vec![Transform::IDENTITY, base],
            player,
        );

        // dt = 0：按当前时间（start = 0）摆一次 —— 仍是绑定姿态。
        assert_eq!(scene.advance_animations(0.0), 2);
        assert!((scene.objects[o].transform.position.x).abs() < 1e-5);
        assert!((scene.objects[o].transform.position.y - 5.0).abs() < 1e-5);

        // 0.25 秒：滑轮 2 → 2.5，即 +0.5；背景不动。
        scene.advance_animations(0.25);
        assert!(
            (scene.objects[o].transform.position.x - 0.5).abs() < 1e-4,
            "半秒该右移 0.5，实际 {}",
            scene.objects[o].transform.position.x
        );
        assert!((scene.objects[o].transform.position.y - 5.0).abs() < 1e-4);
        assert_eq!(scene.objects[still].transform.position, Vec3::ZERO);

        // 再走 0.25 秒 → 累计 0.5 秒 = +1.0（不是在上一次结果上叠加成 +1.5）。
        scene.advance_animations(0.25);
        assert!(
            (scene.objects[o].transform.position.x - 1.0).abs() < 1e-4,
            "累计半秒该是 +1.0，实际 {}",
            scene.objects[o].transform.position.x
        );

        // 重载场景时 `clear` 必须把登记一起清掉：否则旧下标会去动新场景的别的物体。
        scene.clear();
        assert!(scene.animations.is_empty(), "clear 要清掉动画登记");
        assert_eq!(scene.advance_animations(1.0), 0);
        assert!(scene.objects.is_empty());
    }
}
