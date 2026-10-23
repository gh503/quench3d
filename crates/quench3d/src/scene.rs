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

use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
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

/// 一个 CPU 粒子发射器的参数。
///
/// 粒子是**公告板**：每个粒子是一个面向相机的四边形（`MeshData::quad`），
/// 颜色/透明度按寿命淡出。它们不是刚体，也不参与物理；这里只做视觉。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParticleDesc {
    /// 粒子数（会 clamp 到 `1..=4096`，避免误写成十万个物体）。
    pub count: usize,
    /// 发射器中心（世界坐标）。
    pub position: Vec3,
    /// 初始位置在中心周围的球形半径（米）。
    pub radius: f32,
    /// 粒子初速度（米/秒）。
    pub velocity: Vec3,
    /// 初速度的随机扰动幅度（米/秒）。
    pub spread: f32,
    /// 寿命（秒，必须 > 0）。
    pub lifetime: f32,
    /// 四边形边长（米，必须 > 0）。
    pub size: f32,
    /// 重力加速度（米/秒²）。
    pub gravity: Vec3,
    /// 颜色（线性 RGBA；alpha 再乘寿命淡出）。
    pub color: Vec4,
}

impl Default for ParticleDesc {
    fn default() -> Self {
        Self {
            count: 64,
            position: Vec3::new(0.0, 1.0, 0.0),
            radius: 0.1,
            velocity: Vec3::new(0.0, 1.2, 0.0),
            spread: 0.4,
            lifetime: 2.0,
            size: 0.12,
            gravity: Vec3::new(0.0, -1.0, 0.0),
            color: Vec4::new(1.0, 0.65, 0.2, 0.8),
        }
    }
}

/// 一个 CPU 粒子发射器的运行时状态。
pub struct ParticleSystem {
    /// 每个粒子对应的场景物体下标。
    pub objects: Vec<usize>,
    /// 发射器中心。
    pub position: Vec3,
    /// 初始位置球形半径。
    pub radius: f32,
    /// 初速度。
    pub velocity: Vec3,
    /// 速度扰动。
    pub spread: f32,
    /// 寿命（秒）。
    pub lifetime: f32,
    /// 四边形边长。
    pub size: f32,
    /// 重力。
    pub gravity: Vec3,
    /// 基础颜色（alpha 在运行时乘淡出）。
    pub color: Vec4,
    /// 每颗粒子的年龄与速度。
    state: Vec<ParticleState>,
    /// 确定性伪随机种子。
    seed: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct ParticleState {
    age: f32,
    velocity: Vec3,
}

/// 线性同余随机数（只用于视觉扰动；不需要密码学质量）。
fn particle_rand(seed: &mut u64) -> f32 {
    *seed = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    ((*seed >> 40) as f32) / ((1u32 << 24) as f32)
}

/// 半径 `radius` 球内的均匀随机点。
fn particle_point(seed: &mut u64, radius: f32) -> Vec3 {
    let mut p = Vec3::new(
        particle_rand(seed) * 2.0 - 1.0,
        particle_rand(seed) * 2.0 - 1.0,
        particle_rand(seed) * 2.0 - 1.0,
    );
    let len = p.length();
    if len > 1.0 {
        p /= len;
    }
    p * (radius * particle_rand(seed).cbrt())
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

/// 状态机用来判断过渡的运行时参数（浮点；布尔/触发器用 0/1 表达）。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimatorParameter {
    /// 参数名（场景文件里的 `parameters[].name`）。
    pub name: String,
    /// 当前值；由宿主每帧写（角色速度、是否着地……）。
    pub value: f32,
    /// 场景文件里的缺省值；「重置」用它。
    pub default: f32,
}

/// 过渡条件的比较方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimatorOp {
    /// `value > expected`。
    Greater,
    /// `value >= expected`。
    GreaterOrEqual,
    /// `value < expected`。
    Less,
    /// `value <= expected`。
    LessOrEqual,
    /// `|value - expected| < 1e-6`。
    Equal,
    /// 与 `Equal` 相反。
    NotEqual,
}

impl AnimatorOp {
    /// 这个比较是否成立。
    pub fn matches(self, value: f32, expected: f32) -> bool {
        match self {
            Self::Greater => value > expected,
            Self::GreaterOrEqual => value >= expected,
            Self::Less => value < expected,
            Self::LessOrEqual => value <= expected,
            Self::Equal => (value - expected).abs() < 1e-6,
            Self::NotEqual => (value - expected).abs() >= 1e-6,
        }
    }
}

/// 状态机过渡的一个参数条件。
#[derive(Clone, Debug, PartialEq)]
pub struct AnimatorCondition {
    /// 参数名；必须能在 `parameters` 里找到。
    pub parameter: String,
    /// 比较方式。
    pub op: AnimatorOp,
    /// 比较的右值。
    pub value: f32,
}

/// 状态机的一条过渡。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimatorTransition {
    /// 从哪个状态名出发。
    pub from: String,
    /// 到哪个状态名。
    pub to: String,
    /// 在当前状态停留多少秒后开始过渡（`on_finished = true` 时忽略）。
    pub after: f32,
    /// `true` = 当前 clip **播完**（非循环播放器 `finished`）时立即过渡，而不是等 `after`。
    ///
    /// 这让"跑步→停下"这类一次性动作不需要猜 clip 时长；若当前状态仍循环，
    /// 它永远不会结束，这条过渡也就不会触发。
    pub on_finished: bool,
    /// 淡入秒数；0 = 立即切换。
    pub fade: f32,
    /// 参数条件；全部满足才允许过渡。空 = 只按时间/播完判断（旧行为）。
    pub conditions: Vec<AnimatorCondition>,
    /// 可选的事件名：宿主发这个事件时**立即**过渡（忽略 `after`/`on_finished`）。
    pub on_event: Option<String>,
}

/// 状态机输入缓冲的容量（条）。足够覆盖一帧内的连招/移动输入，又不会无限堆积。
pub const ANIMATOR_EVENT_CAPACITY: usize = 8;

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
    /// 运行时参数（按场景顺序；没有参数时为空）。
    pub parameters: Vec<ModelAnimatorParameter>,
    /// 已收到、等帧首处理的事件（有界 FIFO，最多 [`ANIMATOR_EVENT_CAPACITY`] 条）。
    ///
    /// 每帧只消费队首一条；队列满时丢最旧的，保证最新输入不会因为
    /// 旧事件堆积而永远轮不到。
    pub event_buffer: VecDeque<String>,
}

impl ModelAnimatorRuntime {
    /// 把一条事件压进输入缓冲；空名忽略。
    ///
    /// 缓冲区满时丢**最旧**的一条：输入缓冲服务的是"玩家刚按的键"，
    /// 旧事件不应该把新事件挤掉。返回 true 表示已入队。
    pub fn queue_event(&mut self, name: impl Into<String>) -> bool {
        let name = name.into();
        if name.trim().is_empty() {
            return false;
        }
        while self.event_buffer.len() >= ANIMATOR_EVENT_CAPACITY {
            self.event_buffer.pop_front();
        }
        self.event_buffer.push_back(name);
        true
    }

    /// 当前缓冲里还有多少条未处理事件。
    pub fn queued_events(&self) -> usize {
        self.event_buffer.len()
    }
}

/// 一条两骨 IK 链（模型局部空间）。
///
/// `root → mid → tip` 按 glTF **节点名字**引用；求解只改这两根骨头的朝向，
/// 骨骼长度保持不变。`target` 是模型局部空间的位置（不是世界空间，也不受
/// `models[].position/rotation/scale` 影响）。`pole` 是弯曲方向的提示向量，
/// 缺省沿用当前膝盖/肘部的弯曲方向。
#[derive(Clone, Debug, PartialEq)]
pub struct IkChain {
    /// 大腿 / 上臂节点名。
    pub root: String,
    /// 小腿 / 前臂节点名。
    pub mid: String,
    /// 脚 / 手节点名。
    pub tip: String,
    /// 末端目标位置（模型局部空间）。
    pub target: Vec3,
    /// 弯曲方向提示（模型局部空间的方向向量，不需要归一化）。
    pub pole: Option<Vec3>,
    /// IK/FK 混合权重（0~1）：0 = 完全保持 FK 姿态，1 = 完全解到目标；中间值
    /// 对两次关节旋转做 slerp，骨骼长度不变，但末端不再保证精确到达。
    pub weight: f32,
}

/// 一条**动画事件标记**：播放头经过 `time` 时触发 `name`。
///
/// `clip` 为空表示只对主 clip 生效；写名字时只对同名 clip 生效（主/副 clip 都行）。
/// 时间是 clip 局部时间（秒），循环播放用 `rem_euclid(duration)` 归一化。
#[derive(Clone, Debug, PartialEq)]
pub struct SceneAnimationEvent {
    /// 事件名（给宿主逻辑用，不要求唯一）。
    pub name: String,
    /// 触发时间（秒，>= 0）。
    pub time: f32,
    /// 可选的 clip 名；`None` = 主 clip。
    pub clip: Option<String>,
}

/// 已经触发的一条动画事件（由 [`Scene::take_animation_events`] 取走）。
#[derive(Clone, Debug, PartialEq)]
pub struct FiredAnimationEvent {
    /// 事件名。
    pub name: String,
    /// 触发时所在 clip 的名字。
    pub clip: String,
    /// 标记上写的时间（秒，未取模）。
    pub time: f32,
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
    /// 两骨 IK 链；空 = 不做 IK。
    pub ik: Vec<IkChain>,
    /// 动画事件标记（主/副 clip 用 `clip` 区分）；空 = 不触发。
    pub events: Vec<SceneAnimationEvent>,
}

/// 与 `ModelAnimationPlayer::advance` 相同的倍速兜底：非正/非有限按 1 倍速。
fn sanitize_animation_speed(speed: f32) -> f32 {
    if speed.is_finite() && speed > 0.0 {
        speed
    } else {
        1.0
    }
}

/// 播放头从 `before` 前进 `delta` 秒，返回跨过的标记。
///
/// 区间取**半开** `[start, end)`：上一帧刚好停在标记上不算过，下一帧从该点出发才算；
/// 循环时 `time == duration` 归一到 0，在下一圈开头触发。非循环 clip 的
/// `time == duration` 标记会在到达结尾时补触发一次。
fn animation_events_in_span(
    before: f32,
    delta: f32,
    duration: f32,
    looping: bool,
    clip: &str,
    allow_unscoped: bool,
    markers: &[SceneAnimationEvent],
) -> Vec<FiredAnimationEvent> {
    if markers.is_empty()
        || !delta.is_finite()
        || delta <= 0.0
        || !duration.is_finite()
        || duration <= 0.0
    {
        return Vec::new();
    }
    if !looping && before >= duration {
        // 非循环 clip 播完后播放头夹在结尾；继续推进不应把开头的事件再触发一遍。
        return Vec::new();
    }
    let mut remaining = delta;
    if !remaining.is_finite() || remaining <= 0.0 {
        return Vec::new();
    }
    let mut start = if looping {
        before.rem_euclid(duration)
    } else {
        before.clamp(0.0, duration)
    };
    if start >= duration {
        start = 0.0;
    }
    let mut out = Vec::new();
    // 标记按时间排序，输出才是时间顺序（声明顺序不要求有序）。
    let mut list: Vec<(f32, &SceneAnimationEvent)> = markers
        .iter()
        .filter(|m| match m.clip.as_deref() {
            // 不写 clip 的标记只跟主 clip，不能因为副 clip 也播放就重复触发。
            None => allow_unscoped,
            Some(c) => c == clip,
        })
        .filter(|m| m.time.is_finite() && m.time >= 0.0)
        .map(|m| {
            let t = if looping {
                m.time.rem_euclid(duration)
            } else {
                m.time
            };
            (t, m)
        })
        .collect();
    list.sort_by(|a, b| a.0.total_cmp(&b.0));
    for _ in 0..256 {
        if remaining <= 0.0 || out.len() >= 256 {
            break;
        }
        let end = (start + remaining).min(duration);
        for &(t, marker) in &list {
            let end_hit = !looping && end >= duration && (t - duration).abs() <= f32::EPSILON;
            if t + f32::EPSILON >= start && (t < end || end_hit) {
                out.push(FiredAnimationEvent {
                    name: marker.name.clone(),
                    clip: clip.to_string(),
                    time: marker.time,
                });
                if out.len() >= 256 {
                    break;
                }
            }
        }
        remaining -= (end - start).max(0.0);
        if !looping || end < duration {
            break;
        }
        start = 0.0;
    }
    out
}

/// 取一个动画实例当前该用的节点世界矩阵（单 clip 或双 clip 混合）。
fn animation_worlds(a: &ModelAnimationInstance) -> Option<Vec<Mat4>> {
    let mut worlds = match &a.secondary {
        Some(secondary) => a
            .model
            .blended_node_worlds(&a.player, secondary, a.blend_weight),
        None => a.player.node_worlds(&a.model),
    }?;
    solve_ik_chains(&a.model, &mut worlds, &a.ik);
    Some(worlds)
}

/// 节点名 → 下标；空名字不参与匹配。
fn node_index(model: &crate::assets::LoadedModel, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    model.nodes.iter().position(|n| n.name == name)
}

/// `node` 是不是 `ancestor` 的后代（含自身）。
fn is_descendant(model: &crate::assets::LoadedModel, node: usize, ancestor: usize) -> bool {
    let mut current = Some(node);
    let mut depth = 0;
    while let Some(i) = current {
        if i == ancestor {
            return true;
        }
        depth += 1;
        if depth > model.nodes.len() {
            return false;
        }
        current = model.nodes.get(i).and_then(|n| n.parent);
    }
    false
}

/// 绕 `pivot` 旋转一棵子树（`node` 及其所有后代）的世界矩阵。
fn rotate_subtree(
    model: &crate::assets::LoadedModel,
    worlds: &mut [Mat4],
    node: usize,
    pivot: Vec3,
    rotation: Quat,
) {
    if !rotation.is_finite() {
        return;
    }
    let m = Mat4::from_rotation_translation_scale(pivot, rotation, Vec3::ONE)
        * Mat4::from_rotation_translation_scale(-pivot, Quat::IDENTITY, Vec3::ONE);
    for (i, world) in worlds.iter_mut().enumerate() {
        if is_descendant(model, i, node) {
            *world = m * *world;
        }
    }
}

/// 解一个两骨三角形：返回 `(新的中间关节点, 新的末端点)`。
///
/// 目标超出可达范围时夹到最长/最短距离（不拉伸骨骼）；`pole` 只决定往哪边弯，
/// 不改变目标距离。
fn two_bone_solution(
    a: Vec3,
    b: Vec3,
    c: Vec3,
    target: Vec3,
    pole: Option<Vec3>,
) -> Option<(Vec3, Vec3)> {
    let l1 = (b - a).length();
    let l2 = (c - b).length();
    if !l1.is_finite() || !l2.is_finite() || l1 <= 1e-5 || l2 <= 1e-5 {
        return None;
    }
    let to_target = target - a;
    let distance = to_target.length();
    if !distance.is_finite() || distance <= 1e-5 {
        return None;
    }
    let dir = to_target.normalize_or_zero();
    if dir == Vec3::ZERO {
        return None;
    }
    let reach = (l1 + l2 - 1e-4).max((l1 - l2).abs() + 1e-4);
    let d = distance.clamp((l1 - l2).abs() + 1e-4, reach);
    let cos = ((l1 * l1 + d * d - l2 * l2) / (2.0 * l1 * d)).clamp(-1.0, 1.0);
    let sin = (1.0 - cos * cos).max(0.0).sqrt();

    // 弯曲方向：优先用 pole，其次保持原来的弯曲平面；退化时随便挑一个垂直方向。
    let base_bend = pole.filter(|p| p.length_squared() > 1e-8).unwrap_or(b - a);
    let mut bend = base_bend - dir * base_bend.dot(dir);
    if bend.length_squared() <= 1e-8 {
        let axis = if dir.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
        bend = dir.cross(axis);
    }
    let bend = bend.normalize_or_zero();
    if bend == Vec3::ZERO {
        return None;
    }
    let mid = a + dir * (l1 * cos) + bend * (l1 * sin);
    let tip = a + dir * d;
    Some((mid, tip))
}

/// 依次应用所有 IK 链。链引用缺失/不构成父子关系/退化时整条跳过，不抛错。
fn solve_ik_chains(model: &crate::assets::LoadedModel, worlds: &mut [Mat4], chains: &[IkChain]) {
    for chain in chains {
        let (Some(root), Some(mid), Some(tip)) = (
            node_index(model, &chain.root),
            node_index(model, &chain.mid),
            node_index(model, &chain.tip),
        ) else {
            continue;
        };
        if root == mid
            || mid == tip
            || !is_descendant(model, mid, root)
            || !is_descendant(model, tip, mid)
        {
            continue;
        }
        let weight = if chain.weight.is_finite() {
            chain.weight.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if weight <= 0.0 {
            continue;
        }
        let (Some(&wr), Some(&wm), Some(&wt)) =
            (worlds.get(root), worlds.get(mid), worlds.get(tip))
        else {
            continue;
        };
        let a = wr.translation();
        let b = wm.translation();
        let c = wt.translation();
        let Some((mid_goal, tip_goal)) = two_bone_solution(a, b, c, chain.target, chain.pole)
        else {
            continue;
        };

        let q1 = Quat::from_rotation_arc(
            (b - a).normalize_or_zero(),
            (mid_goal - a).normalize_or_zero(),
        );
        // IK/FK 混合：从单位旋转 slerp 到解出来的旋转；weight=1 时与旧行为逐位一致。
        let q1 = if weight < 1.0 {
            Quat::IDENTITY.slerp(q1, weight)
        } else {
            q1
        };
        rotate_subtree(model, worlds, root, a, q1);

        // 根骨旋转后重新取中间点；再绕它把末端对准目标。
        let b_now = worlds[mid].translation();
        let c_now = worlds[tip].translation();
        let q2 = Quat::from_rotation_arc(
            (c_now - b_now).normalize_or_zero(),
            (tip_goal - b_now).normalize_or_zero(),
        );
        let q2 = if weight < 1.0 {
            Quat::IDENTITY.slerp(q2, weight)
        } else {
            q2
        };
        rotate_subtree(model, worlds, mid, b_now, q2);
    }
}

/// 取一个动画实例当前该用的逐网格刚性矩阵。
fn animation_matrices(a: &ModelAnimationInstance) -> Vec<Mat4> {
    match animation_worlds(a) {
        Some(worlds) => a.model.mesh_matrices_from_worlds(&worlds),
        None => vec![Mat4::IDENTITY; a.model.meshes.len()],
    }
}

/// 本帧可以走 GPU 蒙皮的实例规划。
///
/// 返回 `(调色板, 物体下标 → (起始, 数量))`。收**所有带有效 JOINTS/WEIGHTS 的片段**，
/// 包括同时有 morph 的片段：morph 位移仍在 CPU 的绑定姿态上叠加并上传顶点，
/// 关节矩阵由顶点着色器乘；调色板超过 128 个关节的模型退回 CPU，避免越界。
fn gpu_skin_plan(
    animations: &[ModelAnimationInstance],
    enabled: bool,
) -> (Vec<Mat4>, HashMap<usize, (u32, u32)>) {
    let limit = quench3d_render::MAX_SKIN_JOINTS as usize;
    if !enabled {
        return (Vec::new(), HashMap::new());
    }
    let mut palette = Vec::new();
    let mut plan = HashMap::new();
    for a in animations {
        let Some(worlds) = animation_worlds(a) else {
            continue;
        };
        for (mesh_index, &object_index) in a.objects.iter().enumerate() {
            let Some(mesh) = a.model.meshes.get(mesh_index) else {
                continue;
            };
            if mesh.skin.is_none()
                || mesh.joints.len() != mesh.data.vertices.len()
                || mesh.weights.len() != mesh.data.vertices.len()
            {
                continue;
            }
            let Some(joints) = a.model.skin_matrices(mesh_index, &worlds) else {
                continue;
            };
            if joints.is_empty() || palette.len() + joints.len() > limit {
                continue;
            }
            let base = palette.len() as u32;
            let count = joints.len() as u32;
            palette.extend(joints);
            plan.insert(object_index, (base, count));
        }
    }
    (palette, plan)
}

/// 一个已蒙皮模型的骨骼线段（模型局部空间，父关节 → 子关节）。
///
/// 只画 `skins[].joints` 里的关节，并且只连**最近关节祖先**——中间可能隔着
/// 非关节节点（glTF 的变换分组），跳过它们，骨架才不会断。
fn skeleton_edges(model: &crate::assets::LoadedModel, worlds: &[Mat4]) -> Vec<(Vec3, Vec3)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for skin in &model.skins {
        for &node in &skin.joints {
            let mut parent = model.nodes.get(node).and_then(|n| n.parent);
            while let Some(p) = parent {
                if skin.joints.contains(&p) {
                    if seen.insert((p, node)) {
                        let (Some(a), Some(b)) = (
                            worlds.get(p).map(Mat4::translation),
                            worlds.get(node).map(Mat4::translation),
                        ) else {
                            break;
                        };
                        out.push((a, b));
                    }
                    break;
                }
                parent = model.nodes.get(p).and_then(|n| n.parent);
            }
        }
    }
    out
}

/// 状态机的一步：推进当前状态计时，命中过渡就建立淡入（或立即切换）。
///
/// `dt` 非法时按 0 处理；`fade` 非法时按 0；过渡按场景中的顺序取第一条命中的。
/// 条件是否全部成立；缺参数/非有限值一律不成立（宁可不切，也不乱切）。
fn conditions_hold(anim: &ModelAnimatorRuntime, conditions: &[AnimatorCondition]) -> bool {
    conditions.iter().all(|c| {
        anim.parameters
            .iter()
            .find(|p| p.name == c.parameter)
            .is_some_and(|p| {
                p.value.is_finite() && c.value.is_finite() && c.op.matches(p.value, c.value)
            })
    })
}

fn advance_animator(a: &mut ModelAnimationInstance, dt: f32) {
    // 过渡中不再启动下一条过渡；等 promotion 把新 clip 提为主 clip。
    if a.secondary.is_some() {
        return;
    }
    let step = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
    // 在借 `animator` 之前取，避免同时借 `a.player` 的字段。
    let player_finished = a.player.finished;
    let Some(anim) = &mut a.animator else {
        return;
    };
    let current_name = anim.states.get(anim.current).map(|s| s.name.clone());
    let Some(current_name) = current_name else {
        return;
    };
    // 事件在帧首消费：每帧只取队首一条。没被任何过渡接住就丢弃，
    // 后续事件留在缓冲里等下一帧（过渡期间不处理，见上面的 early return）。
    let event = anim.event_buffer.pop_front();
    // 过渡在**帧首**判定：本帧开始时已经停留够 after 才切。这样目标 clip
    // 不会被多推进一整帧——它本帧只从 0 开始走这一帧的时间。
    //
    // 事件过渡优先于同状态的时间过渡：先在所有事件过渡里找，找不到再退回
    // 普通时间/播完过渡。这样「攻击键」不会被一个正好到点的 idle→walk 抢走。
    let event_transition = event.as_deref().and_then(|name| {
        anim.transitions.iter().find(|t| {
            t.from == current_name
                && t.on_event.as_deref() == Some(name)
                && conditions_hold(anim, &t.conditions)
        })
    });
    let transition = event_transition.or_else(|| {
        anim.transitions.iter().find(|t| {
            if t.from != current_name || t.on_event.is_some() {
                return false;
            }
            // 条件驱动的自环会在条件持续满足时每帧重启；只允许时间/播完自环（旧行为）。
            if !t.conditions.is_empty() && t.to == current_name {
                return false;
            }
            if !conditions_hold(anim, &t.conditions) {
                return false;
            }
            if t.on_finished {
                // 帧首判定：上一帧 `player.advance` 已经把它置为 finished。
                player_finished
            } else {
                t.after.is_finite() && t.after >= 0.0 && anim.elapsed >= t.after
            }
        })
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

/// 一个手工 LOD 层级：用哪些物体、相机距离到多远时用这一级。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelLodLevel {
    /// 这一级显示的场景物体下标（同一模型的全部节点）。
    pub objects: Vec<usize>,
    /// 相机距离 `<= max_distance` 时使用这一级；越远越粗。
    pub max_distance: f32,
}

/// 一个手工 LOD 组：第 0 级是基础模型，之后是按距离替换的粗模。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelLodGroup {
    /// 从细到粗的层级；至少 2 级，`max_distance` 递增。
    pub levels: Vec<ModelLodLevel>,
    /// 当前显示的层级下标。
    pub active: usize,
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
    /// 是否用顶点着色器做蒙皮（缺省开）。关掉退回 CPU 蒙皮，用于对照/排障。
    ///
    /// 纯蒙皮与 morph+skin 都走 GPU 蒙皮；morph 位移是 CPU 叠加后上传，
    /// 关节矩阵仍只乘一次（在顶点着色器里）。
    pub gpu_skinning: bool,
    /// 手工 LOD 组；每帧按相机距离切可见层级。
    pub lods: Vec<ModelLodGroup>,
    /// CPU 粒子发射器（视觉层；不是刚体，也不参与物理）。
    pub particles: Vec<ParticleSystem>,
    /// 本帧触发、等待宿主导出的动画事件（`take_animation_events` 清空）。
    fired_animation_events: Vec<FiredAnimationEvent>,
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
            gpu_skinning: true,
            lods: Vec::new(),
            particles: Vec::new(),
            fired_animation_events: Vec::new(),
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

    /// 添加一个 CPU 粒子发射器；每个粒子是一个独立的公告板场景物体。
    ///
    /// 返回发射器下标（不是物体下标）。粒子物体不会出现在场景树/导出里
    /// （见 [`Scene::is_particle_object`]），由 [`Scene::update_particles`] 每帧驱动。
    pub fn add_particle_system(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        desc: ParticleDesc,
    ) -> usize {
        let count = desc.count.clamp(1, 4096);
        let lifetime = if desc.lifetime.is_finite() && desc.lifetime > 0.0 {
            desc.lifetime
        } else {
            1.0
        };
        let size = if desc.size.is_finite() && desc.size > 0.0 {
            desc.size
        } else {
            0.1
        };
        let radius = if desc.radius.is_finite() {
            desc.radius.max(0.0)
        } else {
            0.0
        };
        let spread = if desc.spread.is_finite() {
            desc.spread.max(0.0)
        } else {
            0.0
        };
        let system = self.particles.len();
        let seed = 0x9E37_79B9_7F4A_7C15u64 ^ (system as u64 + 1);
        let mut system_state = ParticleSystem {
            objects: Vec::with_capacity(count),
            position: desc.position,
            radius,
            velocity: desc.velocity,
            spread,
            lifetime,
            size,
            gravity: desc.gravity,
            color: desc.color,
            state: Vec::with_capacity(count),
            seed,
        };
        for i in 0..count {
            let mut offset = particle_point(&mut system_state.seed, radius);
            offset += desc.position;
            let index = self.add_named(
                mesh,
                material,
                Transform::new(offset, Quat::IDENTITY, Vec3::splat(size)),
                &format!("$particles/{system}/{i}"),
            );
            self.objects[index].color = desc.color;
            self.objects[index].visible = true;
            system_state.objects.push(index);
            system_state.state.push(ParticleState {
                age: particle_rand(&mut system_state.seed) * lifetime,
                velocity: desc.velocity + particle_point(&mut system_state.seed, spread),
            });
        }
        self.particles.push(system_state);
        system
    }

    /// 这个物体是不是粒子发射器生成的公告板（决定场景树/拾取/导出是否忽略它）。
    pub fn is_particle_object(&self, index: usize) -> bool {
        // 代理下标按加入顺序递增（删除只会保持相对顺序），所以能二分；
        // 场景树/拾取每帧对每个物体都要问一次，4096 个粒子时线性扫会明显卡。
        self.particles
            .iter()
            .any(|system| system.objects.binary_search(&index).is_ok())
    }

    /// 按帧推进粒子：老化/重生、重力、速度积分、面向相机、寿命淡出。
    ///
    /// `dt = 0` 时只做朝向/颜色同步（暂停时粒子不继续飞，但相机绕过去仍然面向相机）。
    pub fn update_particles(&mut self, camera: Vec3, dt: f32) {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        for system in &mut self.particles {
            for i in 0..system.objects.len() {
                let object_index = system.objects[i];
                let Some(state) = system.state.get_mut(i) else {
                    continue;
                };
                let mut respawned = false;
                if dt > 0.0 {
                    state.age += dt;
                    if state.age >= system.lifetime {
                        state.age = 0.0;
                        state.velocity =
                            system.velocity + particle_point(&mut system.seed, system.spread);
                        if let Some(object) = self.objects.get_mut(object_index) {
                            object.transform.position =
                                system.position + particle_point(&mut system.seed, system.radius);
                        }
                        // 重生的这一帧不再积分：否则新粒子会凭空多飞一帧的 dt。
                        respawned = true;
                    }
                }
                let Some(object) = self.objects.get_mut(object_index) else {
                    continue;
                };
                if dt > 0.0 && !respawned {
                    object.transform.position += state.velocity * dt;
                    state.velocity += system.gravity * dt;
                }
                let to_camera = camera - object.transform.position;
                if to_camera.length_squared() > 1e-8 {
                    object.transform.rotation =
                        Quat::from_rotation_arc(Vec3::Z, to_camera.normalize());
                }
                object.transform.scale = Vec3::splat(system.size);
                let fade = (1.0 - (state.age / system.lifetime).clamp(0.0, 1.0)).powi(2);
                object.color = Vec4::new(
                    system.color.x,
                    system.color.y,
                    system.color.z,
                    system.color.w * fade,
                );
                object.visible = fade > 0.001;
            }
        }
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
        self.particles.clear();
        self.fired_animation_events.clear();
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
            ik: Vec::new(),
            events: Vec::new(),
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
            ik: Vec::new(),
            events: Vec::new(),
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
            ik: Vec::new(),
            events: Vec::new(),
            animator: Some(ModelAnimatorRuntime {
                states,
                transitions,
                current: initial,
                elapsed: 0.0,
                pending: None,
                parameters: Vec::new(),
                event_buffer: VecDeque::new(),
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

    /// 设置状态机参数；参数不存在或值非有限时返回 `false`。
    ///
    /// 这是"输入"入口：宿主每帧把角色速度、是否着地等写进来，过渡条件在帧首判定。
    /// 只改运行时值，不写场景文件（场景里的 `default` 不会变）。
    pub fn set_animator_parameter(
        &mut self,
        animation_index: usize,
        name: &str,
        value: f32,
    ) -> bool {
        if !value.is_finite() {
            return false;
        }
        let Some(anim) = self
            .animations
            .get_mut(animation_index)
            .and_then(|a| a.animator.as_mut())
        else {
            return false;
        };
        let Some(param) = anim.parameters.iter_mut().find(|p| p.name == name) else {
            return false;
        };
        param.value = value;
        true
    }

    /// 注册一个手工 LOD 组：`levels[0]` 是基础模型，之后是按距离替换的粗模。
    ///
    /// 返回组下标；层级少于 2、空物体列表、阈值非有限/负值/不递增时返回 `None`。
    /// 注册后只有第 0 级可见，其余层级隐藏；后续由 [`Scene::update_lods`] 按相机距离切。
    pub fn add_model_lod(&mut self, levels: Vec<ModelLodLevel>) -> Option<usize> {
        if levels.len() < 2 {
            return None;
        }
        let mut prev = f32::NEG_INFINITY;
        for level in &levels {
            let finite_or_inf =
                level.max_distance.is_finite() || level.max_distance == f32::INFINITY;
            if level.objects.is_empty()
                || !finite_or_inf
                || level.max_distance < 0.0
                || level.max_distance <= prev
            {
                return None;
            }
            prev = level.max_distance;
        }
        for (li, level) in levels.iter().enumerate() {
            for &object in &level.objects {
                if let Some(obj) = self.objects.get_mut(object) {
                    obj.visible = li == 0;
                }
            }
        }
        let group = ModelLodGroup { levels, active: 0 };
        self.lods.push(group);
        Some(self.lods.len() - 1)
    }

    /// 按相机位置更新所有 LOD 组；返回发生切换的组数。
    ///
    /// 选择规则：距离落在当前层级的 `[上一层阈值, 本层阈值]` 内就保持；离开这个
    /// 区间 **10% 的滞回带**才切，避免相机在阈值附近抖动导致来回弹。
    /// 组内物体的 `visible` 由 LOD 接管（会覆盖用户之前的隐藏状态）。
    pub fn update_lods(&mut self, camera_position: Vec3) -> usize {
        if self.lods.is_empty() {
            return 0;
        }
        let origins: Vec<Option<Vec3>> = self
            .lods
            .iter()
            .map(|group| {
                let mut sum = Vec3::ZERO;
                let mut count = 0usize;
                for object in group.levels.first()?.objects.iter() {
                    if self.objects.get(*object).is_some() {
                        sum += self.world_transform(*object).position;
                        count += 1;
                    }
                }
                (count > 0).then_some(sum / count as f32)
            })
            .collect();
        let mut changed = 0;
        for (group, origin) in self.lods.iter_mut().zip(origins) {
            let Some(origin) = origin else {
                continue;
            };
            let distance = (camera_position - origin).length();
            let active = group.active.min(group.levels.len() - 1);
            let upper = group.levels[active].max_distance;
            let lower = if active > 0 {
                group.levels[active - 1].max_distance
            } else {
                0.0
            };
            let next = if distance <= upper * 1.1 && distance >= lower * 0.9 {
                active
            } else {
                group
                    .levels
                    .iter()
                    .position(|l| distance <= l.max_distance)
                    .unwrap_or(group.levels.len() - 1)
            };
            if next != group.active {
                group.active = next;
                changed += 1;
            }
            for (li, level) in group.levels.iter().enumerate() {
                for object in &level.objects {
                    if let Some(obj) = self.objects.get_mut(*object) {
                        obj.visible = li == group.active;
                    }
                }
            }
        }
        changed
    }

    /// 发一个离散事件：状态机里有过渡监听这个名字时排队，下一帧帧首处理。
    ///
    /// 返回 `false` 表示整个状态机里没有任何过渡监听这个事件（不会静默堆积）。
    /// 同帧多次调用按 FIFO 入队（最多 8 条，满时丢最旧）；每帧帧首消费一条，
    /// 过渡事件优先于同状态的时间过渡。
    pub fn fire_animator_event(&mut self, animation_index: usize, name: &str) -> bool {
        if name.trim().is_empty() {
            return false;
        }
        let Some(anim) = self
            .animations
            .get_mut(animation_index)
            .and_then(|a| a.animator.as_mut())
        else {
            return false;
        };
        // 只要状态机里**有**任何过渡监听这个名字就接收：输入缓冲的意义就是
        // 在当前动作还没结束前先按下下一招（`from` 可能是未来的状态）。
        // 本次帧首没有匹配过渡的事件会被丢弃，不会无限期留存。
        if !anim
            .transitions
            .iter()
            .any(|t| t.on_event.as_deref() == Some(name))
        {
            return false;
        }
        anim.queue_event(name)
    }

    /// 这个状态机里声明过的事件名（去重、保持过渡顺序；给编辑器画按钮）。
    pub fn animator_event_names(&self, animation_index: usize) -> Vec<String> {
        let Some(anim) = self
            .animations
            .get(animation_index)
            .and_then(|a| a.animator.as_ref())
        else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for t in &anim.transitions {
            if let Some(name) = &t.on_event {
                if !out.contains(name) {
                    out.push(name.clone());
                }
            }
        }
        out
    }

    /// 把某个状态机的全部参数重置为场景里的 `default`；返回重置个数。
    pub fn reset_animator_parameters(&mut self, animation_index: usize) -> usize {
        let Some(anim) = self
            .animations
            .get_mut(animation_index)
            .and_then(|a| a.animator.as_mut())
        else {
            return 0;
        };
        for p in &mut anim.parameters {
            p.value = p.default;
        }
        anim.parameters.len()
    }

    /// 覆盖某个动画实例的两骨 IK 链（空 = 关闭）。
    ///
    /// 求解发生在**每帧采样之后、蒙皮之前**：CPU 蒙皮、GPU 调色板、刚性片段和
    /// 骨骼叠加层看到的是同一份关节世界矩阵。
    pub fn set_model_ik(&mut self, animation_index: usize, chains: Vec<IkChain>) -> bool {
        let Some(a) = self.animations.get_mut(animation_index) else {
            return false;
        };
        a.ik = chains;
        true
    }

    /// 某个动画实例当前的 IK 链（编辑器展示/测试用）。
    pub fn model_ik(&self, animation_index: usize) -> Option<&[IkChain]> {
        self.animations
            .get(animation_index)
            .map(|a| a.ik.as_slice())
    }

    /// 给一个动画实例挂上事件标记（替换旧的）；下标不存在返回 false。
    pub fn set_model_events(&mut self, index: usize, events: Vec<SceneAnimationEvent>) -> bool {
        match self.animations.get_mut(index) {
            Some(a) => {
                a.events = events;
                true
            }
            None => false,
        }
    }

    /// 某个动画实例的事件标记（编辑器展示用）。
    pub fn model_events(&self, index: usize) -> Option<&[SceneAnimationEvent]> {
        self.animations.get(index).map(|a| a.events.as_slice())
    }

    /// 取走本帧触发的事件（队列清空）。
    ///
    /// 宿主每帧调用一次；不取走会一直累积（最多 1024 条，超出丢最旧的）。
    pub fn take_animation_events(&mut self) -> Vec<FiredAnimationEvent> {
        std::mem::take(&mut self.fired_animation_events)
    }

    /// 丢弃尚未取走的动画事件（例如重新载入场景）。
    pub fn clear_animation_events(&mut self) {
        self.fired_animation_events.clear();
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
            fired_animation_events,
            ..
        } = self;
        let mut updated = 0;
        for a in animations.iter_mut() {
            let primary_before = a.player.time;
            let secondary_before = a.secondary.as_ref().map(|s| s.time);
            advance_animator(a, dt);
            // 事件标记只对单/双 clip 播放生效；状态机切换 clip 的瞬间没有明确的
            // "播放头跨过"语义，校验器会拒绝 animator + events 的组合。
            let primary_events = if a.animator.is_none() {
                animation_events_in_span(
                    primary_before,
                    dt * sanitize_animation_speed(a.player.speed),
                    a.player.duration,
                    a.player.looping,
                    &a.player.animation,
                    true,
                    &a.events,
                )
            } else {
                Vec::new()
            };
            a.player.advance(dt);
            let mut secondary_events = Vec::new();
            if let Some(secondary) = &mut a.secondary {
                let before = secondary_before.unwrap_or(0.0);
                let fired = if a.animator.is_none() {
                    animation_events_in_span(
                        before,
                        dt * sanitize_animation_speed(secondary.speed),
                        secondary.duration,
                        secondary.looping,
                        &secondary.animation,
                        false,
                        &a.events,
                    )
                } else {
                    Vec::new()
                };
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
                // 副 clip 完全被压到 0 时，它的标记不应触发。
                if a.blend_weight > 0.0 {
                    secondary_events = fired;
                }
            }
            promote_animator(a);
            fired_animation_events.extend(primary_events);
            fired_animation_events.extend(secondary_events);
            let mats = animation_matrices(a);
            updated += apply_matrices(objects, &a.objects, &a.base, &mats);
        }
        // 极端 dt 下事件可能超过上限：保留最新的 1024 条，避免队列无限增长。
        if fired_animation_events.len() > 1024 {
            let excess = fired_animation_events.len() - 1024;
            fired_animation_events.drain(..excess);
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
        let (_, gpu_skinned) = gpu_skin_plan(animations, self.gpu_skinning);
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
                let gpu_skin = gpu_skinned.contains_key(&object_index);
                // GPU 蒙皮时只上传 **morph 后的绑定姿态**：关节矩阵仍在顶点着色器里乘。
                // 顺序必须与 CPU 路径一致（先 morph 再 skin），所以这里不能用
                // `deformed_mesh`——那会把关节也乘进去，GPU 再乘一次。
                if gpu_skin && mesh.morph_targets.is_empty() {
                    continue;
                }
                let Some(mut data) = (if gpu_skin {
                    a.model.morphed_mesh(mesh_index, weights)
                } else {
                    a.model.deformed_mesh(mesh_index, &worlds, weights)
                }) else {
                    continue;
                };
                // `morphed_mesh` 从 `mesh.data` 克隆，而 JOINTS_0/WEIGHTS_0 存在
                // `mesh.joints/weights` 里；GPU 蒙皮必须把它们原样带回去，否则
                // 每帧上传会把关节清零，蒙皮直接塌成绑定姿态。
                if gpu_skin {
                    if mesh.joints.len() == data.vertices.len()
                        && mesh.weights.len() == data.vertices.len()
                    {
                        for (i, vertex) in data.vertices.iter_mut().enumerate() {
                            vertex.joints = mesh.joints[i];
                            vertex.weights = mesh.weights[i];
                        }
                    } else {
                        continue;
                    }
                }
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

    /// 当前所有播放中模型的骨骼线段（世界空间），给调试叠加层用。
    ///
    /// 没有动画/蒙皮时返回空；同一根骨头被多个 skin 引用时只画一次。
    pub fn model_skeleton_lines(&self) -> Vec<(Vec3, Vec3)> {
        let mut out = Vec::new();
        for a in &self.animations {
            let Some(&object_index) = a.objects.first() else {
                continue;
            };
            let Some(worlds) = animation_worlds(a) else {
                continue;
            };
            let base = self.world_transform(object_index);
            out.extend(
                skeleton_edges(&a.model, &worlds)
                    .into_iter()
                    .map(|(a, b)| (base.transform_point(a), base.transform_point(b))),
            );
        }
        out
    }

    /// 提交全部可见物体到渲染器。
    pub fn submit(&self, renderer: &mut Renderer) {
        let (palette, skin_plan) = gpu_skin_plan(&self.animations, self.gpu_skinning);
        renderer.set_skin_palette(&palette);
        for (i, obj) in self.objects.iter().enumerate() {
            if obj.visible {
                let mut instance = obj.instance(self.world_transform(i));
                if let Some(&(base, count)) = skin_plan.get(&i) {
                    instance = instance.with_skin(base, count);
                }
                renderer.draw(obj.mesh, obj.material, instance);
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
        // 粒子代理是普通场景物体，删掉前面的物体同样会让它们整体前移；
        // 不修就会把粒子系统指到别人的物体上（甚至越界）。
        for system in &mut self.particles {
            let mut i = 0;
            while i < system.objects.len() {
                if system.objects[i] == index {
                    system.objects.remove(i);
                    if i < system.state.len() {
                        system.state.remove(i);
                    }
                } else {
                    if system.objects[i] > index {
                        system.objects[i] -= 1;
                    }
                    i += 1;
                }
            }
        }
        self.particles.retain(|system| !system.objects.is_empty());
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
        LoadedNode, LoadedSkin,
    };
    use quench3d_math::{Mat4, Quat};
    use std::sync::Arc;

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

    fn marker(time: f32, name: &str) -> SceneAnimationEvent {
        SceneAnimationEvent {
            name: name.into(),
            time,
            clip: None,
        }
    }

    /// 事件区间是半开 `[start, end)`：停在标记上不重复触发，循环回到开头会再触发。
    #[test]
    fn animation_events_fire_once_per_crossing_and_loop() {
        let markers = vec![marker(0.25, "a"), marker(0.75, "b")];
        let span = |before: f32, dt: f32| {
            animation_events_in_span(before, dt, 1.0, true, "转", true, &markers)
                .into_iter()
                .map(|e| e.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(span(0.0, 0.3), vec!["a"]);
        assert!(span(0.3, 0.2).is_empty(), "停在 0.5，不应再触发 a");
        assert_eq!(span(0.5, 0.3), vec!["b"]);
        assert!(span(0.8, 0.1).is_empty(), "到 0.9，未跨 1.0");
        // 从 0.9 走 0.2 绕回 0.1：0.25 还没到；再走 0.2 才跨 0.25。
        assert!(span(0.9, 0.2).is_empty());
        assert_eq!(span(0.1, 0.2), vec!["a"]);
        // 一帧跨两圈：每个标记各触发两次，顺序按时间。
        assert_eq!(
            span(0.0, 2.2),
            vec!["a", "b", "a", "b"],
            "两圈 + 0.2 应触发两轮"
        );
    }

    #[test]
    fn animation_events_respect_clip_and_end_marker() {
        let markers = vec![
            marker(0.5, "主"),
            SceneAnimationEvent {
                name: "副".into(),
                time: 0.5,
                clip: Some("转".into()),
            },
            SceneAnimationEvent {
                name: "别的 clip".into(),
                time: 0.5,
                clip: Some("不存在的".into()),
            },
        ];
        let fired = animation_events_in_span(0.0, 0.6, 1.0, true, "转", true, &markers);
        assert_eq!(
            fired.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["主", "副"],
            "同名 clip 或无 clip 才触发"
        );
        // 非循环的 end 标记在到达结尾时补触发一次。
        let end =
            animation_events_in_span(0.0, 2.0, 1.0, false, "转", true, &[marker(1.0, "结束")]);
        assert_eq!(end.len(), 1);
        assert_eq!(end[0].name, "结束");
        // 副 clip 不接收“不写 clip”的标记；只有显式写副 clip 名的才触发。
        let secondary = animation_events_in_span(
            0.0,
            0.6,
            1.0,
            true,
            "run",
            false,
            &[
                marker(0.5, "主"),
                SceneAnimationEvent {
                    name: "跑步".into(),
                    time: 0.5,
                    clip: Some("run".into()),
                },
            ],
        );
        assert_eq!(
            secondary
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["跑步"]
        );
    }

    #[test]
    fn scene_queues_animation_events_and_drains_them() {
        let model = std::sync::Arc::new(pulley_model());
        let mut scene = Scene::new();
        let o = scene.add_named(1, 0, Transform::default(), "滑轮");
        let player = model.player("转").expect("有动画");
        let index = scene.play_model_animation(
            Arc::clone(&model),
            vec![o],
            vec![Transform::default()],
            player,
        );
        assert!(scene.set_model_events(index, vec![marker(0.25, "a")]));
        assert_eq!(scene.model_events(index).map(|e| e.len()), Some(1));
        assert!(!scene.set_model_events(99, Vec::new()));

        // 0.3 秒跨过 0.25：触发一次，取走后队列为空。
        scene.advance_animations(0.3);
        let fired = scene.take_animation_events();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].name, "a");
        assert_eq!(fired[0].clip, "转");
        assert!(scene.take_animation_events().is_empty());

        // 不跨标记的帧不触发；dt=0（暂停）不触发。
        scene.advance_animations(0.3);
        assert!(scene.take_animation_events().is_empty());
        scene.advance_animations(0.0);
        assert!(scene.take_animation_events().is_empty());

        // 从 0.6 绕回 0.1（不跨 0.25），再走到 0.4 才跨。
        scene.advance_animations(0.5);
        assert!(
            scene.take_animation_events().is_empty(),
            "0.6 → 0.1，没跨 0.25"
        );
        scene.advance_animations(0.3);
        assert_eq!(scene.take_animation_events().len(), 1);
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

    #[test]
    fn skeleton_edges_connect_joints_and_skip_non_joint_nodes() {
        let model = LoadedModel {
            name: "rig".into(),
            meshes: Vec::new(),
            materials: Vec::new(),
            images: Vec::new(),
            nodes: vec![
                LoadedNode {
                    name: "root".into(),
                    ..Default::default()
                },
                LoadedNode {
                    name: "group".into(),
                    parent: Some(0),
                    ..Default::default()
                },
                LoadedNode {
                    name: "mid".into(),
                    parent: Some(1),
                    translation: Vec3::new(1.0, 0.0, 0.0),
                    ..Default::default()
                },
                LoadedNode {
                    name: "tip".into(),
                    parent: Some(2),
                    translation: Vec3::new(1.0, 0.0, 0.0),
                    ..Default::default()
                },
            ],
            animations: Vec::new(),
            // `group` 不是关节：root→mid 必须直接连起来，骨架不能断。
            skins: vec![LoadedSkin {
                joints: vec![0, 2, 3],
                inverse_bind: vec![Mat4::IDENTITY; 3],
            }],
        };
        let worlds = model.bind_pose();
        let edges = skeleton_edges(&model, &worlds);
        assert_eq!(edges.len(), 2, "root→mid、mid→tip 两条边");
        assert_eq!(edges[0].0, Vec3::ZERO);
        assert_eq!(edges[0].1, Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(edges[1].0, Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(edges[1].1, Vec3::new(2.0, 0.0, 0.0));
    }

    fn three_bone_model() -> LoadedModel {
        LoadedModel {
            name: "rig".into(),
            meshes: Vec::new(),
            materials: Vec::new(),
            images: Vec::new(),
            nodes: vec![
                LoadedNode {
                    name: "root".into(),
                    ..Default::default()
                },
                LoadedNode {
                    name: "mid".into(),
                    parent: Some(0),
                    translation: Vec3::new(0.0, 1.0, 0.0),
                    ..Default::default()
                },
                LoadedNode {
                    name: "tip".into(),
                    parent: Some(1),
                    translation: Vec3::new(0.0, 1.0, 0.0),
                    ..Default::default()
                },
            ],
            animations: vec![LoadedAnimation {
                name: "idle".into(),
                ..Default::default()
            }],
            skins: vec![LoadedSkin {
                joints: vec![0, 1, 2],
                inverse_bind: vec![Mat4::IDENTITY; 3],
            }],
        }
    }

    fn ik_chain(target: Vec3, pole: Option<Vec3>) -> IkChain {
        IkChain {
            root: "root".into(),
            mid: "mid".into(),
            tip: "tip".into(),
            target,
            pole,
            weight: 1.0,
        }
    }

    #[test]
    fn two_bone_ik_reaches_target_and_keeps_bone_lengths() {
        let model = three_bone_model();
        let mut worlds = model.bind_pose();
        let chain = ik_chain(Vec3::new(0.0, 1.5, 0.5), Some(Vec3::Z));
        solve_ik_chains(&model, &mut worlds, &[chain]);
        let a = worlds[0].translation();
        let b = worlds[1].translation();
        let c = worlds[2].translation();
        assert!((a - Vec3::ZERO).length() < 1e-4, "根关节不该被移动");
        assert!(
            (c - Vec3::new(0.0, 1.5, 0.5)).length() < 1e-3,
            "末端应到达目标，实际 {c:?}"
        );
        assert!(((b - a).length() - 1.0).abs() < 1e-3, "上骨长度不能变");
        assert!(((c - b).length() - 1.0).abs() < 1e-3, "下骨长度不能变");
        assert!(b.z > 0.1, "pole=+Z 时膝盖应往 +Z 弯，实际 {b:?}");
    }

    #[test]
    fn two_bone_ik_clamps_unreachable_target_without_stretching() {
        let model = three_bone_model();
        let mut worlds = model.bind_pose();
        solve_ik_chains(
            &model,
            &mut worlds,
            &[ik_chain(Vec3::new(0.0, 0.0, 10.0), None)],
        );
        let a = worlds[0].translation();
        let b = worlds[1].translation();
        let c = worlds[2].translation();
        assert!(
            (c.length() - 2.0).abs() < 1e-3,
            "最长只能伸到 2，实际 {c:?}"
        );
        assert!(((b - a).length() - 1.0).abs() < 1e-3);
        assert!(((c - b).length() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn ik_weight_blends_between_fk_and_full_solution() {
        let model = three_bone_model();
        let target = Vec3::new(0.0, 1.5, 0.5);
        let mut full = model.bind_pose();
        solve_ik_chains(&model, &mut full, &[ik_chain(target, Some(Vec3::Z))]);
        assert!((full[2].translation() - target).length() < 1e-3);

        let bind_tip = model.bind_pose()[2].translation();
        let mut half = model.bind_pose();
        let mut chain = ik_chain(target, Some(Vec3::Z));
        chain.weight = 0.5;
        solve_ik_chains(&model, &mut half, &[chain.clone()]);
        let half_tip = half[2].translation();
        assert!(
            (half_tip - bind_tip).length() > 1e-3,
            "0.5 权重必须离开 FK 姿态"
        );
        assert!(
            (half_tip - target).length() > 1e-3,
            "0.5 权重不能完全到达目标"
        );
        assert!(
            (half_tip - target).length() > (full[2].translation() - target).length(),
            "0.5 权重应比全 IK 更接近 FK"
        );
        // 骨骼长度仍是 1：混合的是旋转，不是位置。
        let a = half[0].translation();
        let b = half[1].translation();
        let c = half[2].translation();
        assert!(((b - a).length() - 1.0).abs() < 1e-3);
        assert!(((c - b).length() - 1.0).abs() < 1e-3);

        // 0 = 完全 FK；非法权重按 1 处理。
        let mut none = model.bind_pose();
        let mut zero = ik_chain(target, Some(Vec3::Z));
        zero.weight = 0.0;
        solve_ik_chains(&model, &mut none, &[zero]);
        assert_eq!(none, model.bind_pose());
        let mut clamped = model.bind_pose();
        let mut over = ik_chain(target, Some(Vec3::Z));
        over.weight = 2.0;
        solve_ik_chains(&model, &mut clamped, &[over]);
        assert!((clamped[2].translation() - target).length() < 1e-3);
    }

    #[test]
    fn two_bone_ik_skips_missing_or_broken_chains() {
        let model = three_bone_model();
        let mut worlds = model.bind_pose();
        let before = worlds.clone();
        solve_ik_chains(
            &model,
            &mut worlds,
            &[IkChain {
                root: "root".into(),
                mid: "tip".into(), // tip 不是 root 的直接后代顺序
                tip: "mid".into(),
                target: Vec3::new(1.0, 1.0, 1.0),
                pole: None,
                weight: 1.0,
            }],
        );
        assert_eq!(worlds, before, "坏链整条跳过，不能留下半截旋转");
    }

    #[test]
    fn set_model_ik_drives_the_skeleton_lines() {
        let model = Arc::new(three_bone_model());
        let mut scene = Scene::new();
        let base = Transform::default();
        let object = scene.add_named(1, 1, base, "rig");
        let player = model.player("idle").expect("空 clip 也能建播放器");
        let index =
            scene.play_model_animation(Arc::clone(&model), vec![object], vec![base], player);
        assert!(scene.set_model_ik(
            index,
            vec![ik_chain(Vec3::new(0.0, 1.5, 0.5), Some(Vec3::Z))]
        ));
        let lines = scene.model_skeleton_lines();
        assert_eq!(lines.len(), 2);
        assert!(
            (lines[0].1 - lines[1].0).length() < 1e-3,
            "两段骨头应共用中间关节点"
        );
        assert!(
            (lines[1].1 - Vec3::new(0.0, 1.5, 0.5)).length() < 1e-3,
            "骨骼叠加层看到的末端应已 IK 到目标"
        );
    }

    fn condition_model() -> Arc<LoadedModel> {
        Arc::new(LoadedModel {
            name: "animator-params".into(),
            meshes: Vec::new(),
            materials: Vec::new(),
            images: Vec::new(),
            nodes: Vec::new(),
            animations: vec![
                LoadedAnimation {
                    name: "idle".into(),
                    duration: 1.0,
                    ..Default::default()
                },
                LoadedAnimation {
                    name: "run".into(),
                    duration: 1.0,
                    ..Default::default()
                },
                LoadedAnimation {
                    name: "jump".into(),
                    duration: 1.0,
                    ..Default::default()
                },
            ],
            skins: Vec::new(),
        })
    }

    fn condition_state(name: &str, animation: &str) -> ModelAnimatorState {
        ModelAnimatorState {
            name: name.into(),
            animation: animation.into(),
            speed: 1.0,
            looping: true,
            start: 0.0,
        }
    }

    fn condition_transition(
        from: &str,
        to: &str,
        after: f32,
        parameter: &str,
        op: AnimatorOp,
        value: f32,
    ) -> ModelAnimatorTransition {
        ModelAnimatorTransition {
            from: from.into(),
            to: to.into(),
            after,
            on_finished: false,
            fade: 0.0,
            conditions: vec![AnimatorCondition {
                parameter: parameter.into(),
                op,
                value,
            }],
            on_event: None,
        }
    }

    #[test]
    fn animator_conditions_switch_state_and_respect_the_delay() {
        let model = condition_model();
        let states = vec![
            condition_state("idle", "idle"),
            condition_state("run", "run"),
        ];
        let transitions = vec![
            condition_transition("idle", "run", 0.0, "speed", AnimatorOp::Greater, 0.1),
            condition_transition("run", "idle", 0.0, "speed", AnimatorOp::Less, 0.1),
        ];
        let mut scene = Scene::new();
        let idx = scene
            .play_model_animator(model, Vec::new(), Vec::new(), states, transitions, 0)
            .expect("两个 clip 都在");
        scene.animations[idx].animator.as_mut().unwrap().parameters =
            vec![ModelAnimatorParameter {
                name: "speed".into(),
                value: 0.0,
                default: 0.0,
            }];

        // 条件不满足：不切。
        scene.advance_animations(0.0);
        assert_eq!(scene.animations[idx].player.animation, "idle");
        assert_eq!(scene.animations[idx].animator.as_ref().unwrap().current, 0);

        // 写入输入参数：下一次 advance（dt=0）立即切到 run。
        assert!(scene.set_animator_parameter(idx, "speed", 1.0));
        scene.advance_animations(0.0);
        assert_eq!(scene.animations[idx].player.animation, "run");
        assert_eq!(scene.animations[idx].animator.as_ref().unwrap().current, 1);

        // 反向条件：回到 idle；重置参数也回到默认值 0。
        assert!(scene.set_animator_parameter(idx, "speed", 0.0));
        scene.advance_animations(0.0);
        assert_eq!(scene.animations[idx].player.animation, "idle");
        assert_eq!(scene.reset_animator_parameters(idx), 1);
        let p = &scene.animations[idx].animator.as_ref().unwrap().parameters[0];
        assert_eq!(p.value, p.default);
    }

    #[test]
    fn animator_conditions_wait_for_the_transition_delay() {
        let model = condition_model();
        let states = vec![
            condition_state("idle", "idle"),
            condition_state("run", "run"),
        ];
        let transitions = vec![condition_transition(
            "idle",
            "run",
            2.0,
            "speed",
            AnimatorOp::Greater,
            0.1,
        )];
        let mut scene = Scene::new();
        let idx = scene
            .play_model_animator(model, Vec::new(), Vec::new(), states, transitions, 0)
            .expect("两个 clip 都在");
        scene.animations[idx].animator.as_mut().unwrap().parameters =
            vec![ModelAnimatorParameter {
                name: "speed".into(),
                value: 1.0,
                default: 0.0,
            }];
        scene.advance_animations(1.0);
        assert_eq!(scene.animations[idx].player.animation, "idle", "还差 1 秒");
        scene.advance_animations(1.0);
        assert_eq!(
            scene.animations[idx].player.animation, "idle",
            "帧首判定：到点后下一帧才切"
        );
        scene.advance_animations(0.0);
        assert_eq!(scene.animations[idx].player.animation, "run", "满 2 秒后切");
    }

    #[test]
    fn animator_event_transitions_win_over_due_time_transitions() {
        let model = condition_model();
        let states = vec![
            condition_state("idle", "idle"),
            condition_state("walk", "run"),
            condition_state("jump", "jump"),
        ];
        let transitions = vec![
            // 时间过渡写在前面，且 after=0 本帧就到期；事件仍必须赢。
            ModelAnimatorTransition {
                from: "idle".into(),
                to: "walk".into(),
                after: 0.0,
                on_finished: false,
                fade: 0.0,
                conditions: Vec::new(),
                on_event: None,
            },
            ModelAnimatorTransition {
                from: "idle".into(),
                to: "jump".into(),
                after: 999.0,
                on_finished: false,
                fade: 0.0,
                conditions: Vec::new(),
                on_event: Some("跳跃".into()),
            },
        ];
        let mut scene = Scene::new();
        let idx = scene
            .play_model_animator(model, Vec::new(), Vec::new(), states, transitions, 0)
            .expect("三个 clip 都在");
        assert_eq!(scene.animator_event_names(idx), vec!["跳跃".to_string()]);
        assert!(!scene.fire_animator_event(idx, "不存在"));
        assert!(scene.fire_animator_event(idx, "跳跃"));
        scene.advance_animations(0.0);
        assert_eq!(scene.animations[idx].player.animation, "jump");
        assert!(
            scene.animations[idx]
                .animator
                .as_ref()
                .unwrap()
                .event_buffer
                .is_empty(),
            "事件必须被消费一次"
        );
    }

    #[test]
    fn animator_event_conditions_are_checked_before_switching() {
        let model = condition_model();
        let states = vec![
            condition_state("idle", "idle"),
            condition_state("jump", "jump"),
        ];
        let transitions = vec![ModelAnimatorTransition {
            from: "idle".into(),
            to: "jump".into(),
            after: 0.0,
            on_finished: false,
            fade: 0.0,
            conditions: vec![AnimatorCondition {
                parameter: "speed".into(),
                op: AnimatorOp::Greater,
                value: 0.1,
            }],
            on_event: Some("跳跃".into()),
        }];
        let mut scene = Scene::new();
        let idx = scene
            .play_model_animator(model, Vec::new(), Vec::new(), states, transitions, 0)
            .expect("两个 clip 都在");
        scene.animations[idx].animator.as_mut().unwrap().parameters =
            vec![ModelAnimatorParameter {
                name: "speed".into(),
                value: 0.0,
                default: 0.0,
            }];

        // 事件能排队，但条件不满足 → 不切，事件这一帧丢弃。
        assert!(scene.fire_animator_event(idx, "跳跃"));
        scene.advance_animations(0.0);
        assert_eq!(scene.animations[idx].player.animation, "idle");
        assert!(scene.animations[idx]
            .animator
            .as_ref()
            .unwrap()
            .event_buffer
            .is_empty());

        // 条件满足后同一个事件才切。
        assert!(scene.set_animator_parameter(idx, "speed", 1.0));
        assert!(scene.fire_animator_event(idx, "跳跃"));
        scene.advance_animations(0.0);
        assert_eq!(scene.animations[idx].player.animation, "jump");
    }

    #[test]
    fn animator_event_buffer_is_fifo_bounded_and_one_per_frame() {
        let model = condition_model();
        let states = vec![
            condition_state("idle", "idle"),
            condition_state("jump", "jump"),
        ];
        let transitions = vec![
            ModelAnimatorTransition {
                from: "idle".into(),
                to: "jump".into(),
                after: 999.0,
                on_finished: false,
                fade: 0.0,
                conditions: Vec::new(),
                on_event: Some("a".into()),
            },
            ModelAnimatorTransition {
                from: "jump".into(),
                to: "idle".into(),
                after: 999.0,
                on_finished: false,
                fade: 0.0,
                conditions: Vec::new(),
                on_event: Some("b".into()),
            },
        ];
        let mut scene = Scene::new();
        let idx = scene
            .play_model_animator(model, Vec::new(), Vec::new(), states, transitions, 0)
            .expect("clip 都在");
        assert!(scene.fire_animator_event(idx, "a"));
        assert!(scene.fire_animator_event(idx, "b"));
        assert_eq!(
            scene.animations[idx]
                .animator
                .as_ref()
                .unwrap()
                .queued_events(),
            2,
            "同帧两条事件都要留在 FIFO 里"
        );
        // 每帧只消费队首一条：a 切到 jump，b 还在；下一帧 b 切回 idle。
        scene.advance_animations(0.0);
        assert_eq!(scene.animations[idx].player.animation, "jump");
        assert_eq!(
            scene.animations[idx]
                .animator
                .as_ref()
                .unwrap()
                .queued_events(),
            1
        );
        scene.advance_animations(0.0);
        assert_eq!(scene.animations[idx].player.animation, "idle");
        assert_eq!(
            scene.animations[idx]
                .animator
                .as_ref()
                .unwrap()
                .queued_events(),
            0
        );

        // 容量上限 8：满时丢最旧，保留最新 8 条。
        let animator = scene.animations[idx].animator.as_mut().unwrap();
        for i in 0..12 {
            assert!(animator.queue_event(format!("e{i}")));
        }
        let queued: Vec<_> = animator.event_buffer.iter().cloned().collect();
        assert_eq!(
            queued,
            vec!["e4", "e5", "e6", "e7", "e8", "e9", "e10", "e11"]
        );
        assert!(!animator.queue_event("   "), "空名不入队");
    }

    #[test]
    fn animator_parameter_setter_rejects_unknown_and_nonfinite() {
        let model = condition_model();
        let states = vec![condition_state("idle", "idle")];
        let mut scene = Scene::new();
        let idx = scene
            .play_model_animator(model, Vec::new(), Vec::new(), states, Vec::new(), 0)
            .expect("clip 在");
        scene.animations[idx].animator.as_mut().unwrap().parameters =
            vec![ModelAnimatorParameter {
                name: "speed".into(),
                value: 0.0,
                default: 0.0,
            }];
        assert!(!scene.set_animator_parameter(idx, "missing", 1.0));
        assert!(!scene.set_animator_parameter(idx, "speed", f32::NAN));
        assert!(scene.set_animator_parameter(idx, "speed", 2.5));
        assert_eq!(
            scene.animations[idx].animator.as_ref().unwrap().parameters[0].value,
            2.5
        );
    }

    #[test]
    fn model_skeleton_lines_follow_the_model_world_transform() {
        let model = Arc::new(LoadedModel {
            name: "rig".into(),
            meshes: Vec::new(),
            materials: Vec::new(),
            images: Vec::new(),
            nodes: vec![
                LoadedNode {
                    name: "root".into(),
                    ..Default::default()
                },
                LoadedNode {
                    name: "mid".into(),
                    parent: Some(0),
                    translation: Vec3::new(1.0, 0.0, 0.0),
                    ..Default::default()
                },
            ],
            animations: vec![LoadedAnimation {
                name: "idle".into(),
                ..Default::default()
            }],
            skins: vec![LoadedSkin {
                joints: vec![0, 1],
                inverse_bind: vec![Mat4::IDENTITY; 2],
            }],
        });
        let mut scene = Scene::new();
        let base = Transform::new(Vec3::new(10.0, 2.0, 0.0), Quat::IDENTITY, Vec3::ONE);
        let object = scene.add_named(1, 1, base, "rig");
        let player = model.player("idle").expect("空 clip 也能建播放器");
        scene.play_model_animation(Arc::clone(&model), vec![object], vec![base], player);
        let lines = scene.model_skeleton_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, Vec3::new(10.0, 2.0, 0.0));
        assert_eq!(lines[0].1, Vec3::new(11.0, 2.0, 0.0));
    }

    #[test]
    fn lods_switch_visibility_by_camera_distance_with_hysteresis() {
        let mut scene = Scene::new();
        let near = scene.add_named(0, 0, Transform::IDENTITY, "near");
        let mid = scene.add_named(1, 0, Transform::IDENTITY, "mid");
        let far = scene.add_named(2, 0, Transform::IDENTITY, "far");
        let group = scene
            .add_model_lod(vec![
                ModelLodLevel {
                    objects: vec![near],
                    max_distance: 10.0,
                },
                ModelLodLevel {
                    objects: vec![mid],
                    max_distance: 20.0,
                },
                ModelLodLevel {
                    objects: vec![far],
                    max_distance: f32::INFINITY,
                },
            ])
            .expect("三级 LOD");
        assert_eq!(group, 0);
        // 注册后只有基础层可见。
        assert!(scene.objects[near].visible);
        assert!(!scene.objects[mid].visible);
        assert!(!scene.objects[far].visible);

        // 5m：仍是最细。
        assert_eq!(scene.update_lods(Vec3::new(5.0, 0.0, 0.0)), 0);
        assert_eq!(scene.lods[0].active, 0);
        // 15m：中模；25m：粗模。
        assert_eq!(scene.update_lods(Vec3::new(15.0, 0.0, 0.0)), 1);
        assert_eq!(scene.lods[0].active, 1);
        assert!(scene.objects[mid].visible && !scene.objects[near].visible);
        assert_eq!(scene.update_lods(Vec3::new(25.0, 0.0, 0.0)), 1);
        assert_eq!(scene.lods[0].active, 2);
        assert!(scene.objects[far].visible && !scene.objects[mid].visible);

        // 19m 落进 20m 的 10% 滞回带：保持粗模，避免阈值附近抖动。
        scene.update_lods(Vec3::new(19.0, 0.0, 0.0));
        assert_eq!(scene.lods[0].active, 2, "滞回带内不切");
        // 17m 离开滞回带：回到中模。
        scene.update_lods(Vec3::new(17.0, 0.0, 0.0));
        assert_eq!(scene.lods[0].active, 1);
        assert!(scene.objects[mid].visible);
    }

    #[test]
    fn lods_reject_invalid_levels_and_noop_without_groups() {
        let mut scene = Scene::new();
        assert_eq!(scene.update_lods(Vec3::ZERO), 0);
        assert!(
            scene
                .add_model_lod(vec![ModelLodLevel {
                    objects: vec![0],
                    max_distance: 10.0,
                }])
                .is_none(),
            "至少两级"
        );
        assert!(
            scene
                .add_model_lod(vec![
                    ModelLodLevel {
                        objects: vec![0],
                        max_distance: 10.0,
                    },
                    ModelLodLevel {
                        objects: Vec::new(),
                        max_distance: 20.0,
                    },
                ])
                .is_none(),
            "空物体列表"
        );
        assert!(
            scene
                .add_model_lod(vec![
                    ModelLodLevel {
                        objects: vec![0],
                        max_distance: 10.0,
                    },
                    ModelLodLevel {
                        objects: vec![1],
                        max_distance: 10.0,
                    },
                ])
                .is_none(),
            "距离不递增"
        );
    }
}

#[cfg(test)]
mod particle_tests {
    use super::*;

    fn emitter() -> ParticleDesc {
        ParticleDesc {
            count: 8,
            position: Vec3::new(1.0, 2.0, 3.0),
            radius: 0.0,
            velocity: Vec3::new(0.0, 1.0, 0.0),
            spread: 0.0,
            lifetime: 1.0,
            size: 0.25,
            gravity: Vec3::new(0.0, -2.0, 0.0),
            color: Vec4::new(0.2, 0.4, 0.8, 0.6),
        }
    }

    #[test]
    fn particle_system_creates_hidden_from_outliner_proxy_objects() {
        let mut scene = Scene::new();
        assert_eq!(scene.add_particle_system(0, 0, emitter()), 0);
        assert_eq!(scene.particles[0].objects.len(), 8);
        for &index in &scene.particles[0].objects {
            assert!(scene.is_particle_object(index));
            assert_eq!(scene.objects[index].transform.scale, Vec3::splat(0.25));
        }
        assert!(!scene.is_particle_object(0) || scene.particles[0].objects.contains(&0));
    }

    #[test]
    fn particles_move_fly_toward_camera_fade_and_respawn_on_the_clock() {
        let mut scene = Scene::new();
        scene.add_particle_system(0, 0, emitter());
        let index = scene.particles[0].objects[0];
        let base = scene.objects[index].transform.position;
        // 初生年龄是随机的；这条测试要从 0 岁开始看淡出，先钉住年龄。
        scene.particles[0].state[0].age = 0.0;

        // 相机在 -Z 方向：粒子四边形（局部 +Z）应当转向相机。
        let camera = Vec3::new(1.0, 2.0, -5.0);
        scene.update_particles(camera, 0.25);
        let object = &scene.objects[index];
        assert!(
            (object.transform.position - base).length() > 0.1,
            "有初速度的粒子必须移动"
        );
        let normal = object.transform.rotation * Vec3::Z;
        let expected = (camera - object.transform.position).normalize();
        assert!(normal.dot(expected) > 0.99, "公告板必须面向相机");
        assert!(
            object.color.w < 0.6 && object.color.w > 0.0,
            "寿命淡出必须降低 alpha：{}",
            object.color.w
        );

        // 跑过整个寿命：位置回到发射器附近，alpha 重新回到接近基础值。
        scene.update_particles(camera, 1.0);
        let object = &scene.objects[index];
        assert!(
            (object.transform.position - base).length() < 0.75,
            "过寿命后必须重生在发射器附近：{:?}",
            object.transform.position
        );
        assert!(object.color.w > 0.4, "重生后 alpha 应当恢复");
    }

    #[test]
    fn particle_count_and_invalid_fields_are_clamped() {
        let mut scene = Scene::new();
        let mut desc = emitter();
        desc.count = 0;
        desc.lifetime = -3.0;
        desc.size = 0.0;
        scene.add_particle_system(0, 0, desc);
        assert_eq!(scene.particles[0].objects.len(), 1, "count 至少为 1");
        assert_eq!(scene.particles[0].lifetime, 1.0, "非法寿命回退");
        assert_eq!(scene.particles[0].size, 0.1, "非法尺寸回退");
    }
    #[test]
    fn removing_an_object_before_the_proxies_keeps_their_indices_valid() {
        let mut scene = Scene::new();
        scene.add_named(0, 0, Transform::IDENTITY, "先放的物体");
        scene.add_particle_system(0, 0, emitter());
        let old_first = scene.particles[0].objects[0];
        assert_eq!(old_first, 1, "代理应当排在已有物体后面");
        scene.remove(0);
        assert_eq!(
            scene.particles[0].objects[0], 0,
            "删除前面的物体后代理下标必须前移"
        );
        scene.update_particles(Vec3::new(0.0, 0.0, 5.0), 0.1);
        assert!(
            scene.objects[0].visible,
            "仍然能正常驱动，不能指到越界/错物体"
        );
    }
}
