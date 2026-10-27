//! 物理世界：刚体容器 + 固定步长流水线。
//!
//! 每一步（子步）的顺序：
//! 1. 速度积分（重力 / 外力 / 阻尼 / 速度上限）；
//! 2. 宽相（空间哈希）→ 窄相（解析 + GJK/EPA）→ 接触唤醒；
//! 3. 求解器准备（有效质量、恢复目标、热启动）；
//! 4. 速度迭代（接触 + 关节）+ 松弛 + 伪速度位置迭代；
//! 5. 位置积分（速度 + 伪速度，四元数重新归一化）；
//! 6. 睡眠判定。
//!
//! `step` 内部使用累加器，按固定步长推进，单次调用最多 8 个子步；
//! 所有容器遍历顺序固定，因此相同输入必然得到相同输出。

use std::sync::OnceLock;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use crate::small_vec::SmallVec;

use quench3d_math::*;

use crate::body::{BodyHandle, BodyType, RigidBody};
use crate::broadphase::{Broadphase, Proxy};

use crate::contact::{ContactManifold, ContactPair, ContactPoint};
use crate::fluid::{self, Fluid, FluidDesc, FluidStats};
use crate::gjk;
use crate::island::{IslandStats, Islands};
use crate::joints::{
    hinge_angle, Joint, JointEntry, JointHandle, JointImpulses, JointSolver, JointTelemetry,
};
use crate::narrowphase;
#[cfg(not(target_arch = "wasm32"))]
use crate::parallel::ParallelLayout;
use crate::query::{self, QueryFilter, ShapeDistance};
use crate::shape::Shape;
use crate::solver::{self, BodyView, ContactCache, ContactSolver, ManifoldEntry, SolverConfig};
use crate::water::{self, WaterVolume};

/// 接触点达到这个数才启用「预算角速度响应」的求解快速路径。
///
/// 小场景（包括所有黄金摘要回归）继续逐次做叉积/3x3 乘法，结果与历史**逐位相同**；
/// 大场景把 `inv_i_world(r × dir)` 预乘一次，速度/位置/松弛迭代只乘标量，本机
/// 2000 体深堆从约 16.7 降到 14.7~15.3 ms/步。代价是角速度末位可能差 1 ulp，
/// 因此不把它设为无条件路径，也不改小场景的默认行为。
const PRECOMPUTE_CONTACT_RESPONSE_MIN: usize = 2048;

/// 三角网局部 AABB 的均匀网格。三角形按**中心**放入唯一格子，查询时把刚体
/// AABB 按最大三角形半径膨胀——只要三角形 AABB 真的碰到刚体 AABB，它的中心
/// 一定落在膨胀后的查询范围内，所以不会漏检。大网格精确 CCD 不再全扫。
struct TriGrid {
    min: Vec3,
    cell: f32,
    dims: [usize; 3],
    heads: Vec<i32>,
    next: Vec<i32>,
    max_radius: f32,
}

impl TriGrid {
    fn build(bounds: &[Aabb]) -> Option<Self> {
        if bounds.len() < 256 {
            return None;
        }
        let mut min = Vec3::INFINITY;
        let mut max = Vec3::NEG_INFINITY;
        let mut max_radius = 0.0f32;
        for b in bounds {
            if !b.is_valid() {
                continue;
            }
            min = Vec3::new(min.x.min(b.min.x), min.y.min(b.min.y), min.z.min(b.min.z));
            max = Vec3::new(max.x.max(b.max.x), max.y.max(b.max.y), max.z.max(b.max.z));
            max_radius = max_radius.max(b.half_extents().length());
        }
        let extent = max - min;
        let cell = extent.max_element() / 127.0;
        if !(cell.is_finite() && cell > 1e-9) {
            return None;
        }
        let dim = |v: f32| ((v / cell).floor() as usize + 1).clamp(1, 128);
        let dims = [dim(extent.x), dim(extent.y), dim(extent.z)];
        let count = dims[0]
            .checked_mul(dims[1])
            .and_then(|n| n.checked_mul(dims[2]))?;
        let mut heads = vec![-1i32; count];
        let mut next = vec![-1i32; bounds.len()];
        for (k, b) in bounds.iter().enumerate() {
            if !b.is_valid() {
                continue;
            }
            let c = b.center() - min;
            let ix = ((c.x / cell).floor() as isize).clamp(0, dims[0] as isize - 1) as usize;
            let iy = ((c.y / cell).floor() as isize).clamp(0, dims[1] as isize - 1) as usize;
            let iz = ((c.z / cell).floor() as isize).clamp(0, dims[2] as isize - 1) as usize;
            let index = ix + dims[0] * (iy + dims[1] * iz);
            next[k] = heads[index];
            heads[index] = k as i32;
        }
        Some(Self {
            min,
            cell,
            dims,
            heads,
            next,
            max_radius,
        })
    }

    fn candidates(&self, aabb: &Aabb, out: &mut Vec<u32>) {
        out.clear();
        if !aabb.is_valid() {
            return;
        }
        let r = self.max_radius;
        let cell = self.cell;
        let ix0 = ((aabb.min.x - r - self.min.x) / cell)
            .floor()
            .clamp(0.0, (self.dims[0] - 1) as f32) as usize;
        let iy0 = ((aabb.min.y - r - self.min.y) / cell)
            .floor()
            .clamp(0.0, (self.dims[1] - 1) as f32) as usize;
        let iz0 = ((aabb.min.z - r - self.min.z) / cell)
            .floor()
            .clamp(0.0, (self.dims[2] - 1) as f32) as usize;
        let ix1 = ((aabb.max.x + r - self.min.x) / cell)
            .floor()
            .clamp(0.0, (self.dims[0] - 1) as f32) as usize;
        let iy1 = ((aabb.max.y + r - self.min.y) / cell)
            .floor()
            .clamp(0.0, (self.dims[1] - 1) as f32) as usize;
        let iz1 = ((aabb.max.z + r - self.min.z) / cell)
            .floor()
            .clamp(0.0, (self.dims[2] - 1) as f32) as usize;
        for iz in iz0..=iz1 {
            for iy in iy0..=iy1 {
                let row = self.dims[0] * (iy + self.dims[1] * iz);
                for ix in ix0..=ix1 {
                    let mut k = self.heads[row + ix];
                    while k >= 0 {
                        out.push(k as u32);
                        k = self.next[k as usize];
                    }
                }
            }
        }
    }
}

/// 纯 FK 支持的关节类型。
#[derive(Clone, Copy, PartialEq, Eq)]
enum FkKind {
    Hinge,
    Slider,
    /// 仅传播祖先的刚性变换，不施加独立目标。
    Fixed,
}

/// 构建纯 FK 树时的一条原始关节（A/B 尚未按根定向）。
#[derive(Clone, Copy)]
struct FkRaw {
    kind: FkKind,
    a: usize,
    anchor_a: Vec3,
    anchor_b: Vec3,
    axis_a: Vec3,
    axis_b: Vec3,
    target: f32,
    limits: Option<(f32, f32)>,
}

/// 纯 FK 模式下的一条铰链边（已经定向为 parent → child）。
#[derive(Clone, Copy)]
struct FkEdge {
    kind: FkKind,
    anchor_parent: Vec3,
    anchor_child: Vec3,
    axis_parent: Vec3,
    axis_child: Vec3,
    target: f32,
    limits: Option<(f32, f32)>,
    /// parent 是原关节的 A 端时为 +1，B 端时为 -1（目标角取反）。
    sign: f32,
}

/// 把 `root` 及其所有后代整体绕 `pivot` 旋转 `dq`；纯 FK 不产生速度。
fn fk_rotate_subtree(
    world: &mut PhysicsWorld,
    root: usize,
    pivot: Vec3,
    dq: Quat,
    children: &[Vec<usize>],
) {
    let mut stack = vec![root];
    while let Some(index) = stack.pop() {
        if let Some(body) = world
            .bodies
            .get_mut(index)
            .and_then(|slot| slot.body.as_mut())
        {
            if body.body_type != BodyType::Static {
                body.transform.position = pivot + dq.mul_vec3(body.transform.position - pivot);
                body.transform.rotation = (dq * body.transform.rotation).normalize_or_identity();
                body.linear_velocity = Vec3::ZERO;
                body.angular_velocity = Vec3::ZERO;
                body.wake_up();
            }
        }
        stack.extend(children[index].iter().copied());
    }
}

/// 把 `root` 及其所有后代整体平移 `offset`；纯 FK 不产生速度。
fn fk_translate_subtree(
    world: &mut PhysicsWorld,
    root: usize,
    offset: Vec3,
    children: &[Vec<usize>],
) {
    let mut stack = vec![root];
    while let Some(index) = stack.pop() {
        if let Some(body) = world
            .bodies
            .get_mut(index)
            .and_then(|slot| slot.body.as_mut())
        {
            if body.body_type != BodyType::Static {
                body.transform.position += offset;
                body.linear_velocity = Vec3::ZERO;
                body.angular_velocity = Vec3::ZERO;
                body.wake_up();
            }
        }
        stack.extend(children[index].iter().copied());
    }
}

/// 对 FK 树做一次先序驱动：先驱动 child 子树到目标，再处理 child 的后代。
fn fk_apply(
    world: &mut PhysicsWorld,
    parent: usize,
    children: &[Vec<usize>],
    edges: &[Option<FkEdge>],
) {
    for &child in &children[parent] {
        let Some(edge) = edges[child] else {
            continue;
        };
        let Some(parent_body) = world.bodies[parent].body.as_ref() else {
            continue;
        };
        let Some(child_body) = world.bodies[child].body.as_ref() else {
            continue;
        };
        if child_body.body_type == BodyType::Static {
            fk_apply(world, child, children, edges);
            continue;
        }
        let axis_world = parent_body
            .transform
            .rotation
            .mul_vec3(edge.axis_parent)
            .normalize_or_zero();
        if axis_world.length_squared() < 0.5 {
            fk_apply(world, child, children, edges);
            continue;
        }
        let current = match edge.kind {
            FkKind::Fixed => {
                fk_apply(world, child, children, edges);
                continue;
            }
            FkKind::Hinge => hinge_angle(
                edge.axis_parent,
                edge.axis_child,
                &BodyView::from_body(parent_body),
                &BodyView::from_body(child_body),
                axis_world,
            ),
            FkKind::Slider => {
                let wa = parent_body.transform.position
                    + parent_body.transform.rotation.mul_vec3(edge.anchor_parent);
                let wb = child_body.transform.position
                    + child_body.transform.rotation.mul_vec3(edge.anchor_child);
                (wb - wa).dot(axis_world)
            }
        };
        let target = if let Some((lo, hi)) = edge.limits {
            edge.target.clamp(lo.min(hi), lo.max(hi))
        } else {
            edge.target
        };
        let mut delta = edge.sign * target - current;
        if edge.kind == FkKind::Hinge {
            // 目标/当前跨 ±π 时走短弧，避免为了 1° 转 359°。
            delta = (delta + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
        }
        if delta.abs() > 1e-7 {
            match edge.kind {
                FkKind::Fixed => {}
                FkKind::Hinge => {
                    let pivot = parent_body.transform.position
                        + parent_body.transform.rotation.mul_vec3(edge.anchor_parent);
                    let dq = Quat::from_axis_angle(axis_world, delta);
                    fk_rotate_subtree(world, child, pivot, dq, children);
                }
                FkKind::Slider => {
                    fk_translate_subtree(world, child, axis_world * delta, children);
                }
            }
        }
        fk_apply(world, child, children, edges);
    }
}

/// 两个 AABB 的间隙距离：相交为 0，否则是欧氏下界。三角网精确 CCD 用它
/// 做保守预筛，避免对每个三角形都跑 GJK。
fn aabb_gap(a: &Aabb, b: &Aabb) -> f32 {
    let dx = (a.min.x - b.max.x).max(b.min.x - a.max.x).max(0.0);
    let dy = (a.min.y - b.max.y).max(b.min.y - a.max.y).max(0.0);
    let dz = (a.min.z - b.max.z).max(b.min.z - a.max.z).max(0.0);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// 默认固定步长（1/60 秒）。
pub const DEFAULT_FIXED_DT: f32 = 1.0 / 60.0;
/// 单次 `step` 最多推进的子步数。
pub const MAX_SUBSTEPS: usize = 8;
/// 单次 `step` 接受的最大时间增量（防止卡顿后的「时间爆炸」）。
pub const MAX_FRAME_DT: f32 = 0.25;

/// 事件队列的默认上限。
///
/// 存在的理由：事件是**给宿主看的**，宿主忘了取（或跑了一万步不取）时，
/// 无上限的队列会变成内存泄漏。到顶之后新事件被丢掉，但 [`PhysicsWorld::events_dropped`]
/// 会记下来——"静默丢事件"比"内存涨到 OOM"好，但也要能被发现。
pub const DEFAULT_EVENT_CAPACITY: usize = 4096;

/// 复合形状**每对刚体**最多保留的接触点数（按穿透深度取最深的几个）。
pub const MAX_COMPOUND_POINTS: usize = 8;

/// 射线命中结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RaycastHit {
    /// 命中的刚体。
    pub handle: BodyHandle,
    /// 世界命中点。
    pub point: Vec3,
    /// 世界法线（朝向射线来向）。
    pub normal: Vec3,
    /// 命中距离。
    pub distance: f32,
}

impl crate::solver::BodyStore for Vec<BodySlot> {
    fn slot_mut(&mut self, index: usize) -> Option<&mut RigidBody> {
        self.get_mut(index).and_then(|s| s.body.as_mut())
    }

    fn slot_count(&self) -> usize {
        self.len()
    }
}

/// 刚体槽位。
#[derive(Clone, Debug)]
pub(crate) struct BodySlot {
    generation: u32,
    pub(crate) body: Option<RigidBody>,
}

/// 关节槽位。
#[derive(Clone, Debug)]
pub(crate) struct JointSlot {
    generation: u32,
    pub(crate) joint: Option<Joint>,
    pub(crate) impulses: JointImpulses,
    /// 断裂力阈值（N）；本步关节力超过它就断。
    pub(crate) break_force: Option<f32>,
    /// 断裂力矩阈值（N·m）。
    pub(crate) break_torque: Option<f32>,
    /// 关节两端是否**互相碰撞**（缺省 `false`，与 Bullet/PhysX 一致）。
    pub(crate) collide_connected: bool,
    /// 点约束的径向自由间隙（米，0 = 理想点约束）。
    ///
    /// 只作用于 fixed/ball/hinge 的点约束：两锚点距离 ≤ clearance 时完全自由，
    /// 超过后才只约束**径向分离**速度；旋转/角度约束不受影响。缺省 0 走原路径。
    pub(crate) radial_clearance: f32,
}

/// 宿主装入的单调时钟（秒）。
///
/// 为什么要可插拔：`std::time::Instant::now()` 在 `wasm32-unknown-unknown` 上
/// **直接 panic**（"time not implemented on this platform"）。物理 crate 又刻意
/// 保持零依赖，所以不自己绑 `performance.now()`，而是由宿主（`quench3d` 门面）
/// 在启动时把时钟装进来。
///
/// * 原生：默认就用 `Instant`，不装也能工作；
/// * wasm：**不装不会 panic**，只是阶段计时全是 0（`has_time_source()` 返回 false）。
static TIME_SOURCE: OnceLock<fn() -> f64> = OnceLock::new();

/// 装入一个单调时钟（返回**秒**，只要求单调不减）。只能成功装入一次。
///
/// 返回是否装入成功（重复调用返回 `false`）。物理结果与计时无关：
/// 装不装时钟、什么时候装，模拟结果都逐位相同。
pub fn set_time_source(source: fn() -> f64) -> bool {
    TIME_SOURCE.set(source).is_ok()
}

/// 当前是否有可用的时钟。wasm 上没装就是 `false`（此时阶段计时全是 0）。
pub fn has_time_source() -> bool {
    TIME_SOURCE.get().is_some() || cfg!(not(target_arch = "wasm32"))
}

/// 读一次时钟（秒）。wasm 上没装时钟时返回 0.0，**绝不 panic**。
#[inline]
fn now_seconds() -> f64 {
    if let Some(source) = TIME_SOURCE.get() {
        return source();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        static BASE: OnceLock<Instant> = OnceLock::new();
        BASE.get_or_init(Instant::now).elapsed().as_secs_f64()
    }
    #[cfg(target_arch = "wasm32")]
    {
        0.0
    }
}

/// 物理世界。
/// 每步各阶段的累计耗时（微秒）。
///
/// 默认**不采集**：`PhysicsWorld` 里只是一个布尔开关，关着的时候连一次
/// `Instant::now()` 都不会调用，物理结果与计时开关无关（可以逐位对比）。
/// 打开后每步多做 6 次取时钟，用来回答「时间到底花在哪」——
/// 靠改配置去差分是测不准的：改迭代次数会改变模拟状态本身，
/// 关掉位置修正会让箱子互相陷进去、接触流形变大，窄相反而更贵。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhaseTimings {
    /// 采样的步数。
    pub steps: u64,
    /// 速度积分（重力/阻尼）。
    pub integrate_velocity_us: f64,
    /// 宽相（构建代理 + 空间哈希候选对）。
    pub broadphase_us: f64,
    /// 窄相（形状求交 + 唤醒扫描）。
    pub narrowphase_us: f64,
    /// 窄相真正处理过的候选对数（宽相筛完之后）。
    pub candidate_pairs: u64,
    /// 生成的接触流形数。
    pub manifolds: u64,
    /// 组装刚体视图、材质表与关节/接触准备（含热启动）。
    pub prepare_us: f64,
    /// 速度迭代与松弛。
    pub velocity_us: f64,
    /// 位置迭代（分裂冲量）。
    pub position_us: f64,
    /// 速度/伪速度写回 + 位置积分。
    pub writeback_us: f64,
    /// 接触冲量统计、缓存与关节冲量写回。
    pub bookkeeping_us: f64,
    /// 整个子步。
    pub total_us: f64,
    /// 最近一步划分出的 island 数。
    pub islands: u32,
    /// 最近一步最大 island 的刚体数。
    pub largest_island: u32,
    /// 最近一步含两个及以上刚体的 island 数（并行只对这些岛有意义）。
    pub multi_body_islands: u32,
}

impl PhaseTimings {
    /// 每步平均耗时（微秒）按阶段返回，用于打印。
    pub fn per_step(&self) -> [(&'static str, f64); 9] {
        let n = self.steps.max(1) as f64;
        [
            ("速度积分", self.integrate_velocity_us / n),
            ("宽相", self.broadphase_us / n),
            ("窄相", self.narrowphase_us / n),
            ("准备/热启动", self.prepare_us / n),
            ("速度迭代", self.velocity_us / n),
            ("位置迭代", self.position_us / n),
            ("写回+积分", self.writeback_us / n),
            ("记账/缓存", self.bookkeeping_us / n),
            ("整步", self.total_us / n),
        ]
    }
}

/// 传感器事件（一次子步内传感器重叠集合的变化）。
///
/// 只在**变化的那一子步**产生：物体进入触发区报一条 `started: true`，
/// 离开报一条 `started: false`；停在里面的期间不重复报。
/// 两个都是传感器的一对只报一条，`sensor` 取槽位小的那个。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SensorEvent {
    /// 传感器本体。
    pub sensor: BodyHandle,
    /// 进入/离开它的那个刚体。
    pub other: BodyHandle,
    /// `true` = 进入，`false` = 离开。
    pub started: bool,
}

/// 事件种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PhysicsEventKind {
    /// 两个刚体**开始**接触（上一步还没接触）。
    ContactBegin,
    /// 两个刚体**结束**接触。
    ContactEnd,
    /// 某刚体进入传感器。
    SensorEnter,
    /// 某刚体离开传感器。
    SensorExit,
    /// 刚体进入睡眠。
    Sleep,
    /// 刚体醒来。
    Wake,
    /// 关节被拉断。
    JointBroken,
}

impl PhysicsEventKind {
    /// 报告与测试里用的稳定名字（也是场景 JSON 里的字符串）。
    pub fn name(self) -> &'static str {
        match self {
            Self::ContactBegin => "contact_begin",
            Self::ContactEnd => "contact_end",
            Self::SensorEnter => "sensor_enter",
            Self::SensorExit => "sensor_exit",
            Self::Sleep => "sleep",
            Self::Wake => "wake",
            Self::JointBroken => "joint_broken",
        }
    }
}

/// 事件总线里的一条事件。
///
/// 做成**扁平结构**（而不是带负载的枚举）有两个原因：
/// * 队列是 `Vec<PhysicsEvent>`，扁平 + `Copy` 才好复用缓冲、也方便报告直接序列化；
/// * 宿主按 `kind` 分支时字段含义是固定的（接触/传感器用 `a`/`b`，睡眠/唤醒用 `a`，
///   断裂用 `joint` + `force`/`torque`），不需要为每种事件写一遍匹配。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicsEvent {
    /// 发生时的**子步号**（世界里累计的子步计数）。
    pub step: u64,
    /// 种类。
    pub kind: PhysicsEventKind,
    /// 主体（接触/传感器的第一个刚体、睡眠/唤醒的那个刚体、断裂时关节的 A 端）。
    pub a: BodyHandle,
    /// 第二个刚体（单主体事件为 `None`）。
    pub b: Option<BodyHandle>,
    /// 断裂的关节（只有 [`PhysicsEventKind::JointBroken`] 有）。
    pub joint: Option<JointHandle>,
    /// 断裂时的力量级（N）。
    pub force: f32,
    /// 断裂时的力矩量级（N·m）。
    pub torque: f32,
}

/// 一个关节的可视化信息（世界坐标）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointVisual {
    /// A 端锚点的世界位置。
    pub a: Vec3,
    /// B 端锚点的世界位置。
    pub b: Vec3,
    /// 铰链/滑块的轴（世界方向）；其它关节为 `None`。
    pub axis: Option<Vec3>,
}

pub struct PhysicsWorld {
    bodies: Vec<BodySlot>,
    free: Vec<u32>,
    joints: Vec<JointSlot>,
    /// 确定性 island 划分（每步重算，缓冲复用）。
    islands: Islands,
    /// island 并行求解的布局与缓冲（native；wasm 走单线程路径）。
    #[cfg(not(target_arch = "wasm32"))]
    parallel: ParallelLayout,
    /// 上一子步是否真的走了并行路径（供基准与诊断观察）。
    parallel_used: bool,
    joint_free: Vec<u32>,
    gravity: Vec3,
    /// 是否生成接触约束（`false` = drive/运动学回放：不跑宽相/窄相/接触求解）。
    collisions_enabled: bool,
    /// 纯 FK：每步先把铰链子树直接旋到目标角（关节求解器仍会跑，但误差为零）。
    kinematic_fk: bool,
    /// 水域（浮力环境）：不写就是空，行为与没有这一节时逐位相同。
    waters: Vec<WaterVolume>,
    /// 粒子流体（CPU PBF）：不写就是空，没有 `fluids` 时物理行为逐位不变。
    fluids: Vec<Fluid>,
    fixed_dt: f32,
    accumulator: f32,
    /// 最近一次子步的接触对（公开只读）。
    contacts: Vec<ContactPair>,
    /// 本子步里所有「涉及传感器」的接触对（已按 (a, b) 排序）。
    sensor_pairs: Vec<(u32, u32)>,
    /// 上一子步的传感器接触对（用来做进入/离开差分）。
    sensor_pairs_prev: Vec<(u32, u32)>,
    /// 本子步的传感器事件（进入/离开）。
    sensor_events: Vec<SensorEvent>,
    /// 事件总线的队列（宿主用 [`PhysicsWorld::drain_events`] 取走）。
    events: Vec<PhysicsEvent>,
    /// 队列上限（默认 [`DEFAULT_EVENT_CAPACITY`]）。
    event_capacity: usize,
    /// 因为队列满而**丢掉**的事件数（> 0 就说明宿主取事件不够勤）。
    events_dropped: u64,
    /// 本子步所有**真实接触**的刚体对（与上一子步差分得到开始/结束）。
    touch_pairs: Vec<(u32, u32)>,
    /// 上一子步的真实接触对。
    touch_pairs_prev: Vec<(u32, u32)>,
    /// 上一子步每个槽位是否在睡（差分得到睡眠/唤醒事件）。
    sleeping_prev: Vec<bool>,
    manifolds: Vec<ManifoldEntry>,
    cache: ContactCache,
    contact_solver: ContactSolver,
    joint_solver: JointSolver,
    broadphase: Broadphase,
    /// 世界内容版本号：任何会改变"查询结果"的操作（增删刚体、可变访问、步进）
    /// 都 +1。查询索引靠它判断自己是不是过期了（见 `query_index`）。
    version: u64,
    proxies: Vec<Proxy>,
    pairs: Vec<(u32, u32)>,
    /// 窄相并行：每对候选的流形（按 `pairs` 下标对齐，跨步复用）。
    narrowphase_hits: Vec<Option<ContactManifold>>,
    /// 窄相并行：哪些候选对通过分组/关节过滤。
    narrowphase_pair_ok: Vec<bool>,
    /// 最近一子步窄相是否真的走了多线程（诊断/基准用）。
    narrowphase_parallel_used: bool,
    wake_list: Vec<u32>,
    /// 每步复用的刚体视图缓冲（避免每步重新分配 O(n) 内存）。
    scratch_views: Vec<BodyView>,
    /// 每步复用的材质缓冲 `(摩擦, 恢复)`。
    scratch_materials: Vec<solver::ContactMaterial>,
    /// 本步**施力之前**的线/角速度（按 slot 下标对齐）。
    ///
    /// 静摩擦/动摩擦的判定用它，而不是"施力之后"的速度：一个静止的箱子被推的
    /// 那一步，施力后的速度已经不低了（Δt × 加速度），拿它判定会永远选动摩擦，
    /// 于是"推得动、但推不住"变成"一推就走、一直滑"。
    scratch_pre_velocities: Vec<(Vec3, Vec3)>,
    /// 断掉的关节（历史记录，见 [`PhysicsWorld::broken_joints`]）。
    broken_joints: Vec<JointHandle>,
    /// 复用缓冲：本步被马达驱动的刚体（睡眠判定用）。
    scratch_actuated: Vec<bool>,
    /// 每步复用的关节条目缓冲。
    scratch_joint_entries: Vec<JointEntry>,
    /// 每步复用的关节冲量缓冲。
    scratch_joint_impulses: Vec<JointImpulses>,
    /// 求解器参数。
    pub config: SolverConfig,
    /// 是否采集阶段耗时（默认关）。
    phase_timing: bool,
    /// 阶段耗时累计。
    phase_timings: PhaseTimings,
    /// 已完成的子步总数。
    pub step_count: u64,
}

impl Default for PhysicsWorld {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
struct RotatingSweep {
    omega: Vec3,
    p0: Vec3,
    rel_motion: Vec3,
    radius: f32,
    rotation_reach: f32,
    dt: f32,
}

impl PhysicsWorld {
    /// 使用默认重力 `(0, -9.81, 0)` 创建空世界。
    /// 打开/关闭阶段耗时采集。
    ///
    /// 关闭时物理循环里不会取时钟，性能与结果都不受影响；打开只是多测时间，
    /// 不参与任何物理计算，所以对同一个场景，开关两种情况下结果逐位相同。
    pub fn set_phase_timing(&mut self, enabled: bool) {
        self.phase_timing = enabled;
    }

    /// 是否在采集阶段耗时。
    pub fn phase_timing(&self) -> bool {
        self.phase_timing
    }

    /// 当前累计的阶段耗时。
    /// 最近一步的 island 划分统计（确定性、每步重算）。
    ///
    /// 用它判断一个场景能不能靠并行求解变快：多体岛越多越适合并行，
    /// 单个大岛（例如一个深堆）并行没有任何收益。
    pub fn island_stats(&self) -> IslandStats {
        self.islands.stats()
    }

    /// 最近一步划分出的 island 数。
    pub fn island_count(&self) -> usize {
        self.islands.len()
    }

    /// 最近一子步是否走了窄相并行路径（native 上候选对足够多才会为真）。
    pub fn uses_parallel_narrowphase(&self) -> bool {
        self.narrowphase_parallel_used
    }

    /// 最近一子步是否走了 island 并行路径（native 上满足条件才会为真）。
    pub fn uses_parallel_solver(&self) -> bool {
        self.parallel_used
    }

    /// 并行求解使用的线程数（未启用时为 0）。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn solver_threads(&self) -> usize {
        if self.parallel_used {
            self.parallel.threads()
        } else {
            0
        }
    }

    /// wasm 上恒为 0（单线程路径）。
    #[cfg(target_arch = "wasm32")]
    pub fn solver_threads(&self) -> usize {
        0
    }

    /// 某个 island 包含的刚体槽位（升序）。
    ///
    /// 配合 [`PhysicsWorld::bodies`] 的枚举顺序可以看清场景的连通结构：
    /// 一个 2000 体的深堆往往是上百个互不相干的「柱子」，而不是一个巨大的岛。
    pub fn island_bodies(&self, island: u32) -> &[u32] {
        self.islands.bodies_of(island)
    }

    /// 某个刚体槽位所属的 island（静态体与睡着的体返回 `None`）。
    pub fn island_of_body(&self, slot: usize) -> Option<u32> {
        self.islands.island_of(slot)
    }

    pub fn phase_timings(&self) -> PhaseTimings {
        self.phase_timings
    }

    /// 清空累计的阶段耗时（打开采集后先清一次，统计更干净）。
    pub fn reset_phase_timings(&mut self) {
        self.phase_timings = PhaseTimings::default();
    }

    pub fn new() -> Self {
        Self::with_gravity(crate::GRAVITY)
    }

    /// 使用自定义重力创建空世界。
    pub fn with_gravity(gravity: Vec3) -> Self {
        let gravity = if gravity.is_finite() {
            gravity
        } else {
            crate::GRAVITY
        };
        Self {
            bodies: Vec::new(),
            free: Vec::new(),
            joints: Vec::new(),
            islands: Islands::new(),
            #[cfg(not(target_arch = "wasm32"))]
            parallel: ParallelLayout::new(),
            parallel_used: false,
            joint_free: Vec::new(),
            gravity,
            collisions_enabled: true,
            kinematic_fk: false,
            waters: Vec::new(),
            fluids: Vec::new(),
            fixed_dt: DEFAULT_FIXED_DT,
            accumulator: 0.0,
            contacts: Vec::new(),
            sensor_pairs: Vec::new(),
            sensor_pairs_prev: Vec::new(),
            sensor_events: Vec::new(),
            events: Vec::new(),
            event_capacity: DEFAULT_EVENT_CAPACITY,
            events_dropped: 0,
            touch_pairs: Vec::new(),
            touch_pairs_prev: Vec::new(),
            sleeping_prev: Vec::new(),
            manifolds: Vec::new(),
            cache: ContactCache::default(),
            contact_solver: ContactSolver::default(),
            joint_solver: JointSolver::default(),
            broadphase: Broadphase::new(),
            version: 0,
            proxies: Vec::new(),
            pairs: Vec::new(),
            narrowphase_hits: Vec::new(),
            narrowphase_pair_ok: Vec::new(),
            narrowphase_parallel_used: false,
            wake_list: Vec::new(),
            scratch_views: Vec::new(),
            scratch_materials: Vec::new(),
            scratch_pre_velocities: Vec::new(),
            broken_joints: Vec::new(),
            scratch_actuated: Vec::new(),
            scratch_joint_entries: Vec::new(),
            scratch_joint_impulses: Vec::new(),
            config: SolverConfig::default(),
            phase_timing: false,
            phase_timings: PhaseTimings::default(),
            step_count: 0,
        }
    }

    // -----------------------------------------------------------------
    // 重力与步长
    // -----------------------------------------------------------------

    /// 设置重力。
    pub fn set_gravity(&mut self, g: Vec3) {
        if g.is_finite() {
            self.gravity = g;
        }
    }

    /// 当前重力。
    pub fn gravity(&self) -> Vec3 {
        self.gravity
    }

    /// 开/关接触生成。关掉后不跑宽相/窄相/接触求解，`contacts()` 为空；
    /// 关节、PD 伺服、积分器仍工作（用于运动学回放）。
    pub fn set_collisions_enabled(&mut self, enabled: bool) {
        self.collisions_enabled = enabled;
        if !enabled {
            self.contacts.clear();
            self.manifolds.clear();
            self.sensor_pairs.clear();
            self.touch_pairs.clear();
        }
        self.version = self.version.wrapping_add(1);
    }

    /// 接触生成是否开启。
    pub fn collisions_enabled(&self) -> bool {
        self.collisions_enabled
    }

    /// 纯 FK 模式：每步开始时把铰链子树直接旋到目标角、滑块子树直接平移到目标位移。
    ///
    /// 它不替代重力/接触开关；调用方通常同时 `set_gravity(ZERO)` 与
    /// `set_collisions_enabled(false)`（`physics.mode:"fk"` 就是这样接的）。
    /// 关闭时行为与旧版本逐位相同。
    pub fn set_kinematic_fk(&mut self, enabled: bool) {
        self.kinematic_fk = enabled;
    }

    /// 纯 FK 是否开启。
    pub fn kinematic_fk(&self) -> bool {
        self.kinematic_fk
    }

    /// 固定步长。
    pub fn fixed_dt(&self) -> f32 {
        self.fixed_dt
    }

    /// 设置固定步长（钳制在 [1/1000, 1/10] 秒）。
    pub fn set_fixed_dt(&mut self, dt: f32) {
        if dt.is_finite() && dt > 0.0 {
            self.fixed_dt = clamp(dt, 1.0 / 1000.0, 0.1);
        }
    }

    /// 尚未消化的时间余量。
    pub fn accumulator(&self) -> f32 {
        self.accumulator
    }

    // -----------------------------------------------------------------
    // 水域（浮力）
    // -----------------------------------------------------------------

    /// 添加一块水域；参数不可用时返回 `None`（不塞进世界）。
    ///
    /// 水域是**环境**：它不参与碰撞、不占刚体槽，只影响动态体的浮力/阻尼。
    /// 参数非法（NaN、非正尺寸、非正密度、负阻尼）时直接拒绝，避免把
    /// "静默不生效"带进报告。
    pub fn add_water(&mut self, water: WaterVolume) -> Option<usize> {
        if !water.is_valid() {
            return None;
        }
        self.waters.push(water);
        Some(self.waters.len() - 1)
    }

    /// 替换第 `index` 块水域；索引越界或参数不可用时返回 `false` 且不改动。
    ///
    /// 编辑器改水流/密度时走这里，而不是暴露 `&mut [WaterVolume]`：非法值
    /// （NaN、负阻尼、NaN 水流）不能绕过 `add_water` 的同一道校验。
    pub fn set_water(&mut self, index: usize, water: WaterVolume) -> bool {
        if !water.is_valid() {
            return false;
        }
        let Some(slot) = self.waters.get_mut(index) else {
            return false;
        };
        *slot = water;
        true
    }

    /// 删除第 `index` 块水域并返回它；索引越界返回 `None`。
    pub fn remove_water(&mut self, index: usize) -> Option<WaterVolume> {
        if index >= self.waters.len() {
            return None;
        }
        Some(self.waters.remove(index))
    }

    /// 清空所有水域，并把刚体上的浸没占比归零。
    pub fn clear_waters(&mut self) {
        if self.waters.is_empty() {
            return;
        }
        self.waters.clear();
        for slot in &mut self.bodies {
            if let Some(b) = &mut slot.body {
                b.submerged = 0.0;
            }
        }
    }

    /// 当前水域（只读）。
    pub fn waters(&self) -> &[WaterVolume] {
        &self.waters
    }

    /// 水域数量。
    pub fn water_count(&self) -> usize {
        self.waters.len()
    }

    // -----------------------------------------------------------------
    // 粒子流体
    // -----------------------------------------------------------------

    /// 添加一个 CPU PBF 粒子流体（参数非法时返回 `None`，不会静默降级）。
    ///
    /// 粒子在**添加时**按确定性晶格发射；之后每个子步在刚体位置积分之后推进，
    /// 因此流体看到的是本步的刚体位姿。M2 是**单向**的：刚体推流体，流体不推刚体。
    pub fn add_fluid(&mut self, desc: FluidDesc) -> Option<usize> {
        let fluid = Fluid::new(desc)?;
        self.fluids.push(fluid);
        Some(self.fluids.len() - 1)
    }

    /// 清空所有流体。
    pub fn clear_fluids(&mut self) {
        self.fluids.clear();
    }

    /// 当前流体（只读）。
    pub fn fluids(&self) -> &[Fluid] {
        &self.fluids
    }

    /// 可变流体访问（测试/工具注入气泡等；正常场景用 `add_fluid`）。
    pub fn fluids_mut(&mut self) -> &mut [Fluid] {
        &mut self.fluids
    }

    /// 流体数量。
    pub fn fluid_count(&self) -> usize {
        self.fluids.len()
    }

    /// 某个流体的统计（下标越界返回 `None`）。
    pub fn fluid_stats(&self, index: usize) -> Option<FluidStats> {
        self.fluids.get(index).map(Fluid::stats)
    }

    // -----------------------------------------------------------------
    // 刚体
    // -----------------------------------------------------------------

    /// 添加刚体，返回句柄。
    pub fn add_body(&mut self, body: RigidBody) -> BodyHandle {
        self.version += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.bodies[index as usize];
            slot.body = Some(body);
            BodyHandle::new(index, slot.generation)
        } else {
            let index = self.bodies.len() as u32;
            self.bodies.push(BodySlot {
                generation: 1,
                body: Some(body),
            });
            BodyHandle::new(index, 1)
        }
    }

    /// 通过构建器添加刚体。
    pub fn add_builder(&mut self, builder: crate::body::BodyBuilder) -> BodyHandle {
        self.add_body(builder.build())
    }

    /// 移除刚体；成功时返回被移除的刚体。
    pub fn remove_body(&mut self, handle: BodyHandle) -> Option<RigidBody> {
        self.version += 1;
        let slot = self.bodies.get_mut(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        let body = slot.body.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(handle.index);
        // 缓存中的相关条目一并丢弃。
        self.cache.clear();
        Some(body)
    }

    /// 是否存活。
    pub fn contains(&self, handle: BodyHandle) -> bool {
        self.bodies
            .get(handle.index as usize)
            .map(|s| s.generation == handle.generation && s.body.is_some())
            .unwrap_or(false)
    }

    /// 只读访问刚体。
    pub fn body(&self, handle: BodyHandle) -> Option<&RigidBody> {
        let slot = self.bodies.get(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        slot.body.as_ref()
    }

    /// 最近一个固定子步开始前到当前的插值位姿。
    ///
    /// `alpha = 0` 返回上一子步开始前的位姿，`alpha = 1` 返回当前位姿；
    /// 位置线性插值、旋转 `slerp`。只用于渲染，不写回物理状态；缩放保留当前值。
    /// 不做这一步时，固定步长与显示帧率不一致会出现周期性顿挫。
    pub fn body_interpolated_transform(&self, handle: BodyHandle, alpha: f32) -> Option<Transform> {
        let body = self.body(handle)?;
        let alpha = if alpha.is_finite() {
            alpha.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let prev = body.prev_transform;
        let cur = body.transform;
        Some(Transform {
            position: prev.position.lerp(cur.position, alpha),
            rotation: prev.rotation.slerp(cur.rotation, alpha),
            scale: cur.scale,
        })
    }

    /// 唤醒**所有**刚体，返回其中原本睡着的个数。
    ///
    /// 用途：整场景被搬动过（批量设置位姿 / 从快照恢复 / 编辑器里拖了一批物体）之后，
    /// 保证没有物体以"睡着的姿态"停在半空。逐个 `body_mut(h).wake_up()` 也行，
    /// 但那是一次一次借用世界；这里一趟走完，顺带把"醒了几个"报出来（可观测）。
    ///
    /// 注意它**只**负责唤醒：`allow_sleep == false` 的刚体本来就醒着，
    /// 下一次 `step` 也仍然醒着；被唤醒的刚体会重新参与求解。
    pub fn wake_up_all(&mut self) -> usize {
        let mut woken = 0;
        for slot in self.bodies.iter_mut() {
            if let Some(body) = slot.body.as_mut() {
                if body.is_sleeping() {
                    woken += 1;
                }
                body.wake_up();
            }
        }
        woken
    }

    /// 可变访问刚体。
    ///
    /// 会让 [`PhysicsWorld::version`] +1：拿到 `&mut` 之后刚体可能被搬到任何地方，
    /// 已经建好的查询索引（[`PhysicsWorld::build_query_index`]）必须当作过期。
    pub fn body_mut(&mut self, handle: BodyHandle) -> Option<&mut RigidBody> {
        self.version += 1;
        let slot = self.bodies.get_mut(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        slot.body.as_mut()
    }

    /// 存活刚体数量。
    pub fn body_count(&self) -> usize {
        self.bodies.iter().filter(|s| s.body.is_some()).count()
    }

    /// 遍历所有存活刚体（按槽位下标升序，确定）。
    pub fn bodies(&self) -> impl Iterator<Item = (BodyHandle, &RigidBody)> + '_ {
        self.bodies.iter().enumerate().filter_map(|(i, s)| {
            s.body
                .as_ref()
                .map(|b| (BodyHandle::new(i as u32, s.generation), b))
        })
    }

    /// 移除所有刚体与关节。
    pub fn clear(&mut self) {
        self.version += 1;
        self.sensor_pairs.clear();
        self.sensor_pairs_prev.clear();
        self.sensor_events.clear();
        self.events.clear();
        self.touch_pairs.clear();
        self.touch_pairs_prev.clear();
        self.sleeping_prev.clear();
        self.bodies.clear();
        self.joints.clear();
        self.free.clear();
        self.joint_free.clear();
        self.contacts.clear();
        self.manifolds.clear();
        self.cache.clear();
        self.accumulator = 0.0;
    }

    // -----------------------------------------------------------------
    // 关节
    // -----------------------------------------------------------------

    /// 添加关节。
    pub fn add_joint(&mut self, joint: Joint) -> JointHandle {
        if let Some(index) = self.joint_free.pop() {
            let slot = &mut self.joints[index as usize];
            slot.joint = Some(joint);
            slot.impulses = JointImpulses::default();
            // 槽位是复用的：断裂阈值必须清掉，否则新关节会继承上一个关节的阈值
            slot.break_force = None;
            slot.break_torque = None;
            slot.collide_connected = false;
            slot.radial_clearance = 0.0;
            JointHandle {
                index,
                generation: slot.generation,
            }
        } else {
            let index = self.joints.len() as u32;
            self.joints.push(JointSlot {
                generation: 1,
                joint: Some(joint),
                impulses: JointImpulses::default(),
                break_force: None,
                break_torque: None,
                collide_connected: false,
                radial_clearance: 0.0,
            });
            JointHandle {
                index,
                generation: 1,
            }
        }
    }

    /// 移除关节。
    pub fn remove_joint(&mut self, handle: JointHandle) -> bool {
        let slot = match self.joints.get_mut(handle.index as usize) {
            Some(s) if s.generation == handle.generation => s,
            _ => return false,
        };
        if slot.joint.take().is_none() {
            return false;
        }
        slot.generation = slot.generation.wrapping_add(1);
        self.joint_free.push(handle.index);
        true
    }

    /// 只读访问关节。
    pub fn joint(&self, handle: JointHandle) -> Option<&Joint> {
        let slot = self.joints.get(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        slot.joint.as_ref()
    }

    /// 所有**还活着**的关节句柄，按槽位顺序（与场景 `joints[]` 顺序一致）。
    pub fn joint_handles(&self) -> Vec<JointHandle> {
        self.joints
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| {
                slot.joint.as_ref().map(|_| JointHandle {
                    index: i as u32,
                    generation: slot.generation,
                })
            })
            .collect()
    }

    /// 一个关节的运行时遥测（角度 / 目标 / 限位 / 马达力矩）。
    ///
    /// `dt` 是刚刚跑过的子步长：马达力矩 = 累积马达冲量 / dt。没跑过步时冲量为 0，
    /// 因此力矩是 0——报告里要"静力矩"就至少先跑一步。
    pub fn joint_telemetry(&self, handle: JointHandle, dt: f32) -> Option<JointTelemetry> {
        let slot = self
            .joints
            .get(handle.index as usize)
            .filter(|s| s.generation == handle.generation)?;
        let joint = slot.joint.as_ref()?;
        let (ha, hb) = joint.bodies();
        let (a, b) = (self.body(ha)?, self.body(hb)?);
        let (coordinate, target, limits) = match joint {
            Joint::Hinge {
                local_anchor_a: _,
                local_anchor_b: _,
                local_axis_a,
                local_axis_b,
                limits,
                motor,
                ..
            } => {
                let axis = a.rotation() * *local_axis_a;
                let angle = hinge_angle(
                    *local_axis_a,
                    *local_axis_b,
                    &BodyView::from_body(a),
                    &BodyView::from_body(b),
                    axis,
                );
                (angle, motor.and_then(|m| m.target_angle), *limits)
            }
            Joint::Slider {
                local_anchor_a,
                local_anchor_b,
                local_axis_a,
                limits,
                motor,
                ..
            } => {
                let axis = a.rotation() * *local_axis_a;
                let wa = a.position() + a.rotation() * *local_anchor_a;
                let wb = b.position() + b.rotation() * *local_anchor_b;
                (
                    (wb - wa).dot(axis),
                    motor.and_then(|m| m.target_angle),
                    *limits,
                )
            }
            _ => (0.0, None, None),
        };
        let dt = if dt.is_finite() && dt > 1e-9 {
            dt
        } else {
            1e-9
        };
        Some(JointTelemetry {
            handle,
            kind: joint.kind_name(),
            coordinate,
            target,
            limits,
            motor_torque: slot.impulses.motor / dt,
        })
    }

    /// 运行时改一个伺服的目标：铰链是**弧度**，滑块是**米**。返回是否改成功。
    ///
    /// 只改目标，不动增益 / 限幅；关节两端会被唤醒，下一子步就朝新目标动。
    /// 给 `None` / 非有限值返回 false（不静默写 NaN 进世界）。
    pub fn set_joint_target(&mut self, handle: JointHandle, target: f32) -> bool {
        if !target.is_finite() {
            return false;
        }
        let slot = match self.joints.get_mut(handle.index as usize) {
            Some(s) if s.generation == handle.generation => s,
            _ => return false,
        };
        let (ha, hb) = match slot.joint.as_mut() {
            Some(Joint::Hinge {
                motor: Some(m),
                a,
                b,
                ..
            })
            | Some(Joint::Slider {
                motor: Some(m),
                a,
                b,
                ..
            }) => {
                m.target_angle = Some(target);
                m.target_velocity = 0.0;
                (*a, *b)
            }
            _ => return false,
        };
        if let Some(body) = self.bodies.get_mut(ha.index as usize) {
            if let Some(body) = body.body.as_mut() {
                body.wake_up();
            }
        }
        if let Some(body) = self.bodies.get_mut(hb.index as usize) {
            if let Some(body) = body.body.as_mut() {
                body.wake_up();
            }
        }
        true
    }

    /// 读一个铰链伺服的当前目标角（rad）。
    pub fn joint_target(&self, handle: JointHandle) -> Option<f32> {
        let slot = self
            .joints
            .get(handle.index as usize)
            .filter(|s| s.generation == handle.generation)?;
        match slot.joint.as_ref()? {
            Joint::Hinge { motor: Some(m), .. } | Joint::Slider { motor: Some(m), .. } => {
                m.target_angle
            }
            _ => None,
        }
    }

    /// 关节数量。
    pub fn joint_count(&self) -> usize {
        self.joints.iter().filter(|s| s.joint.is_some()).count()
    }

    /// 每个关节两端的**世界**锚点与轴，给编辑器画调试线用。
    ///
    /// 返回的是当前刚体姿态下的值；关节被删除/刚体不存在时跳过，顺序与插入顺序一致。
    pub fn joint_visuals(&self) -> Vec<JointVisual> {
        let mut out = Vec::with_capacity(self.joint_count());
        for slot in &self.joints {
            let Some(joint) = slot.joint.as_ref() else {
                continue;
            };
            let (ha, hb) = joint.bodies();
            let (Some(a), Some(b)) = (self.body(ha), self.body(hb)) else {
                continue;
            };
            let a_anchor = a.position() + a.rotation() * joint.local_anchor_a();
            let b_anchor = b.position() + b.rotation() * joint.local_anchor_b();
            let axis = match joint {
                Joint::Hinge { local_axis_a, .. } | Joint::Slider { local_axis_a, .. } => {
                    Some(a.rotation() * *local_axis_a)
                }
                _ => None,
            };
            out.push(JointVisual {
                a: a_anchor,
                b: b_anchor,
                axis,
            });
        }
        out
    }

    /// 设置关节的断裂阈值（`None` = 那一项不断）。
    ///
    /// `force` 单位 N、`torque` 单位 N·m：每步结束后算一次关节冲量 / dt，
    /// 超过阈值就把关节**移除**（不是禁用），并记进 [`Self::broken_joints`]。
    /// 阈值给非有限值等于不设（静默 NaN 阈值会让关节永远断或永不断）。
    pub fn set_joint_break(
        &mut self,
        handle: JointHandle,
        force: Option<f32>,
        torque: Option<f32>,
    ) -> bool {
        let slot = match self.joints.get_mut(handle.index as usize) {
            Some(s) if s.generation == handle.generation && s.joint.is_some() => s,
            _ => return false,
        };
        slot.break_force = force.filter(|f| f.is_finite() && *f > 0.0);
        slot.break_torque = torque.filter(|t| t.is_finite() && *t > 0.0);
        true
    }

    /// 读回关节的断裂阈值（编辑器导出、报告都要用）。
    pub fn joint_break(&self, handle: JointHandle) -> Option<(Option<f32>, Option<f32>)> {
        let slot = self.joints.get(handle.index as usize)?;
        if slot.generation != handle.generation || slot.joint.is_none() {
            return None;
        }
        Some((slot.break_force, slot.break_torque))
    }

    /// 设置关节两端是否互相碰撞（缺省 `false` = 不碰，和 Bullet / PhysX 一致）。
    ///
    /// 为什么缺省是不碰：铰链/球窝的两端**物理上就重合在一起**（锚点相同），
    /// 让它们参与碰撞等于让门和门轴互相挤——表现为"门转到一半卡住""铰链抖"，
    /// 是新手最常见的坑。想让它们碰（比如"两个球用绳子连着、要能互相弹开"）
    /// 就把这个开关打开。
    pub fn set_joint_collide_connected(&mut self, handle: JointHandle, collide: bool) -> bool {
        match self.joints.get_mut(handle.index as usize) {
            Some(s) if s.generation == handle.generation && s.joint.is_some() => {
                s.collide_connected = collide;
                true
            }
            _ => false,
        }
    }

    /// 读回关节的"两端是否互相碰撞"。
    pub fn joint_collide_connected(&self, handle: JointHandle) -> Option<bool> {
        let slot = self.joints.get(handle.index as usize)?;
        if slot.generation != handle.generation || slot.joint.is_none() {
            return None;
        }
        Some(slot.collide_connected)
    }

    /// 设置点约束的径向自由间隙（米，>=0；非有限/负值按 0 处理）。
    ///
    /// 只对 fixed/ball/hinge 生效；distance/slider 没有点约束，设置了也不参与求解。
    /// 0 = 理想点约束（旧行为）。返回句柄是否有效。
    pub fn set_joint_radial_clearance(&mut self, handle: JointHandle, meters: f32) -> bool {
        let Some(slot) = self
            .joints
            .get_mut(handle.index as usize)
            .filter(|s| s.generation == handle.generation && s.joint.is_some())
        else {
            return false;
        };
        slot.radial_clearance = if meters.is_finite() {
            meters.max(0.0)
        } else {
            0.0
        };
        true
    }

    /// 读取关节径向间隙（米）；句柄无效返回 `None`。
    pub fn joint_radial_clearance(&self, handle: JointHandle) -> Option<f32> {
        let slot = self
            .joints
            .get(handle.index as usize)
            .filter(|s| s.generation == handle.generation && s.joint.is_some())?;
        Some(slot.radial_clearance)
    }

    /// 这一对刚体之间是否有"不互相碰撞"的关节（见
    /// [`Self::set_joint_collide_connected`]）。
    fn joint_blocks_contact(&self, ia: usize, ib: usize) -> bool {
        self.joints.iter().any(|slot| {
            !slot.collide_connected
                && slot.joint.as_ref().is_some_and(|j| {
                    let (ha, hb) = j.bodies();
                    let (x, y) = (ha.index as usize, hb.index as usize);
                    (x == ia && y == ib) || (x == ib && y == ia)
                })
        })
    }

    /// 到目前为止**断掉的关节**（按发生顺序）。
    ///
    /// 断掉的关节句柄已经失效（`joint()` 会返回 `None`），这里留下的是"曾经断过"
    /// 的事实——报告与测试靠它判断"绳子到底断没断"。
    pub fn broken_joints(&self) -> &[JointHandle] {
        &self.broken_joints
    }

    /// 清空断裂记录（报告下一次运行前用）。
    pub fn clear_broken_joints(&mut self) {
        self.broken_joints.clear();
    }

    // -----------------------------------------------------------------
    // 查询
    // -----------------------------------------------------------------

    /// 最近一次子步的接触对。
    pub fn contacts(&self) -> &[ContactPair] {
        &self.contacts
    }

    /// 本子步的传感器事件（进入/离开）。停在里面不动时是空的。
    pub fn sensor_events(&self) -> &[SensorEvent] {
        &self.sensor_events
    }

    /// 事件总线里还没被取走的事件（按发生顺序，确定性）。
    ///
    /// 队列是**跨步累积**的：宿主可以每步取，也可以跑完再取。
    /// 满了就丢新事件并记账（见 [`Self::events_dropped`]）。
    pub fn events(&self) -> &[PhysicsEvent] {
        &self.events
    }

    /// 取走全部事件（把世界的缓冲留给它自己复用，不产生分配）。
    ///
    /// 推荐宿主用一个长期存活的 `Vec` 反复调用它：
    /// ```ignore
    /// let mut buf = Vec::new();
    /// world.drain_events(&mut buf);
    /// for e in &buf { /* ... */ }
    /// ```
    pub fn drain_events(&mut self, out: &mut Vec<PhysicsEvent>) {
        out.append(&mut self.events);
    }

    /// 取走全部事件并返回（内部会换成新缓冲，下一次取会重新分配）。
    pub fn take_events(&mut self) -> Vec<PhysicsEvent> {
        std::mem::take(&mut self.events)
    }

    /// 现场清空事件队列（不影响 [`Self::events_dropped`] 的计数）。
    pub fn clear_events(&mut self) {
        self.events.clear();
    }

    /// 设置队列上限（0 会退化成默认值：一个"永远丢事件"的总线没有意义）。
    pub fn set_event_capacity(&mut self, capacity: usize) {
        self.event_capacity = if capacity == 0 {
            DEFAULT_EVENT_CAPACITY
        } else {
            capacity
        };
        if self.events.len() > self.event_capacity {
            let excess = self.events.len() - self.event_capacity;
            self.events.drain(0..excess);
        }
    }

    /// 队列上限。
    pub fn event_capacity(&self) -> usize {
        self.event_capacity
    }

    /// 因为队列满而丢掉的事件数。
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped
    }

    /// 清掉"丢过事件"的计数（报告下一次运行前用）。
    pub fn reset_events_dropped(&mut self) {
        self.events_dropped = 0;
    }

    /// 本子步里所有涉及传感器的接触对 `(传感器, 对方)`（按槽位排序）。
    pub fn sensor_pairs(&self) -> &[(u32, u32)] {
        &self.sensor_pairs
    }

    /// 向场景发射射线，返回最近命中。
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_distance: f32) -> Option<RaycastHit> {
        self.raycast_filtered(origin, dir, max_distance, |_| true)
    }

    /// 带过滤的射线检测。
    pub fn raycast_filtered<F>(
        &self,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
        filter: F,
    ) -> Option<RaycastHit>
    where
        F: Fn(BodyHandle) -> bool,
    {
        query::raycast_world(self, origin, dir, max_distance, &filter)
    }

    /// 带过滤（[`QueryFilter`]：层掩码 + 是否含传感器）的射线检测。
    ///
    /// 与 [`PhysicsWorld::raycast_filtered`] 的区别：那个收的是闭包，可以按句柄
    /// 任意过滤；这个收的是格式/玩法层里表达得出来的过滤条件。
    pub fn raycast_in(
        &self,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
        filter: QueryFilter,
    ) -> Option<RaycastHit> {
        self.raycast_filtered(origin, dir, max_distance, |h| {
            self.body(h).is_some_and(|b| filter.accepts(b))
        })
    }

    /// 射线检测，返回 `max_distance` 内的**所有**命中（距离升序，平局按槽位下标）。
    pub fn raycast_all(&self, origin: Vec3, dir: Vec3, max_distance: f32) -> Vec<RaycastHit> {
        self.raycast_all_filtered(origin, dir, max_distance, QueryFilter::default())
    }

    /// 带过滤的"所有命中"射线检测。
    pub fn raycast_all_filtered(
        &self,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
        filter: QueryFilter,
    ) -> Vec<RaycastHit> {
        query::raycast_all_filtered(self, origin, dir, max_distance, filter)
    }

    /// 任意形状重叠查询（结果按槽位下标升序）。
    pub fn overlap_shape(&self, shape: &Shape, transform: &Transform) -> Vec<BodyHandle> {
        query::overlap_shape_filtered(self, shape, transform, QueryFilter::default())
    }

    /// 带过滤的任意形状重叠查询。
    pub fn overlap_shape_filtered(
        &self,
        shape: &Shape,
        transform: &Transform,
        filter: QueryFilter,
    ) -> Vec<BodyHandle> {
        query::overlap_shape_filtered(self, shape, transform, filter)
    }

    /// 查询形状与某个刚体的最近距离（相交时为 0）。
    pub fn distance_to(
        &self,
        shape: &Shape,
        transform: &Transform,
        handle: BodyHandle,
    ) -> Option<ShapeDistance> {
        query::distance_to_body_handle(self, shape, transform, handle)
    }

    /// 世界内容版本号（增删刚体 / 可变访问 / 步进都会 +1）。
    pub fn version(&self) -> u64 {
        self.version
    }

    /// 建查询索引（快照）。详见 [`crate::QueryIndex`]。
    pub fn build_query_index(&self) -> crate::QueryIndex {
        crate::QueryIndex::build(self)
    }

    /// 保守推进形状投射。
    pub fn shape_cast(&self, shape: &Shape, from: &Transform, motion: Vec3) -> Option<RaycastHit> {
        query::shape_cast_world(self, shape, from, motion)
    }

    // -----------------------------------------------------------------
    // 步进
    // -----------------------------------------------------------------

    /// 推进仿真：把 `dt` 累加进内部累加器，按固定步长执行若干子步。
    pub fn step(&mut self, dt: f32) {
        self.step_with_accumulator(dt);
    }

    /// 与 [`PhysicsWorld::step`] 等价，返回实际执行的子步数。
    pub fn step_with_accumulator(&mut self, dt: f32) -> usize {
        if !dt.is_finite() || dt <= 0.0 {
            return 0;
        }
        let dt = dt.min(MAX_FRAME_DT);
        if !(self.fixed_dt.is_finite() && self.fixed_dt > 0.0) {
            self.fixed_dt = DEFAULT_FIXED_DT;
        }
        self.accumulator += dt;
        // FK 改位姿也属于物理推进：没有完整固定步时只累加时间。
        // 保持每次外层调用最多应用一次目标，与批量子步入口一致。
        if self.kinematic_fk && self.accumulator >= self.fixed_dt {
            self.apply_kinematic_targets();
        }
        // 子步会改变变换：查询索引到此为止必须重建
        self.version = self.version.wrapping_add(1);
        let mut steps = 0usize;
        while self.accumulator >= self.fixed_dt && steps < MAX_SUBSTEPS {
            let h = self.fixed_dt;
            self.substep(h);
            self.accumulator -= h;
            steps += 1;
        }
        if steps == MAX_SUBSTEPS {
            // 丢弃积压，避免螺旋死亡。
            self.accumulator = 0.0;
        }
        steps
    }

    /// 用**固定数量**的等长子步推进 `dt`，跳过累加器与 [`MAX_SUBSTEPS`] 上限。
    ///
    /// 这是批量仿真（`simulate` / 脚本驱动）用的确定性入口：`dt` 与 `substeps` 相同，
    /// 子步序列就逐位相同，不受帧率或宿主调用节奏影响。`substeps = 0` 视为 1。
    ///
    /// 与 [`Self::step`] 的区别：`step` 以 `fixed_dt` 为步长、按累加器决定跑几步；
    /// 这里**一定**跑 `substeps` 步，每步 `dt / substeps`。物理结果仍由子步长与求解器
    /// 配置决定，不会因为换成这个入口而"变准"。
    pub fn step_substeps(&mut self, dt: f32, substeps: u32) -> usize {
        if !dt.is_finite() || dt <= 0.0 {
            return 0;
        }
        let n = substeps.max(1) as usize;
        if self.kinematic_fk {
            self.apply_kinematic_targets();
        }
        let h = dt.min(MAX_FRAME_DT) / n as f32;
        self.version = self.version.wrapping_add(1);
        for _ in 0..n {
            self.substep(h);
        }
        n
    }

    /// 浮力 + 浸没阻尼：对每个动态体，把各水域的排开体积合成一个力，
    /// 作用在合成形心上（因此有扶正力矩）。
    ///
    /// 睡眠体**不施力**（否则浮力会把它们一直唤醒，永远睡不着）；它们的
    /// 浸没占比仍会重算，报告里看得到。水域为空时整段不执行。
    fn apply_buoyancy(&mut self, dt: f32) {
        if self.waters.is_empty() {
            return;
        }
        let gravity = self.gravity;
        let waters = &self.waters;
        for slot in &mut self.bodies {
            let Some(b) = slot.body.as_mut() else {
                continue;
            };
            if b.body_type != BodyType::Dynamic {
                b.submerged = 0.0;
                continue;
            }
            // 睡眠体连采样都跳过：浮力/阻尼都不会叫醒它，重算只会白烧 CPU。
            // 代价是 `submerged` 停在"最后一次醒着"的值（报告口径，见 water.rs）。
            if b.sleeping {
                continue;
            }
            let total = water::body_volume(b);
            let mut volume = 0.0f32;
            let mut force = Vec3::ZERO;
            let mut moment = Vec3::ZERO;
            let mut drag = 0.0f32;
            let mut flow = Vec2::ZERO;
            let mut flow_drag = 0.0f32;
            for w in waters {
                let s = water::submerged_in(b, w);
                if !s.is_submerged() {
                    continue;
                }
                volume += s.volume;
                // 浮力 = -g · ρ · V（与重力反向）。
                force -= gravity * (w.density * s.volume);
                moment += s.centroid * s.volume;
                drag = drag.max(w.drag);
                if w.flow.length_squared() > 0.0 && w.drag > flow_drag {
                    flow = w.flow;
                    flow_drag = w.drag;
                }
            }
            b.submerged = if total > 0.0 {
                (volume / total).clamp(0.0, 1.0)
            } else {
                0.0
            };
            if volume <= 0.0 || !force.is_finite() {
                continue;
            }
            // 直接累加到 force/torque（**不调 `apply_force`**）：浮力是环境力，
            // 不该每一步都把刚体唤醒——否则漂浮物永远睡不着。睡眠体上面已经跳过，
            // 醒着的体照常积分。
            b.add_force_at_point(force, moment / volume);
            if flow_drag > 0.0 {
                // 水流：把浸没部分的速度拖向局部流速（只作用于 XZ；垂直速度走原阻尼）。
                let target = Vec3::new(flow.x, 0.0, flow.y);
                let f = 1.0 / (1.0 + flow_drag * b.submerged * dt);
                b.linear_velocity.x = target.x + (b.linear_velocity.x - target.x) * f;
                b.linear_velocity.z = target.z + (b.linear_velocity.z - target.z) * f;
                b.linear_velocity.y *= f;
                b.angular_velocity *= f;
            } else if drag > 0.0 {
                // 简化线性阻尼（只对浸没部分生效）；不是粘性应力。
                let f = 1.0 / (1.0 + drag * b.submerged * dt);
                b.linear_velocity *= f;
                b.angular_velocity *= f;
            }
        }
    }

    /// 单个固定子步。
    fn step_fluids(&mut self, dt: f32) {
        if self.fluids.is_empty() {
            return;
        }
        let mut fluids = std::mem::take(&mut self.fluids);
        fluid::step_all(&mut fluids, &mut self.bodies, self.gravity, dt);
        // 两-way 的另一半：粒子的自由表面给刚体浮力/阻尼。
        // 碰撞冲量管"撞"，这里管"泡在水里被托住"——两者缺一个都不像水。
        let gravity = self.gravity;
        let has_waters = !self.waters.is_empty();
        for slot in &mut self.bodies {
            let Some(b) = slot.body.as_mut() else {
                continue;
            };
            if b.body_type != BodyType::Dynamic {
                continue;
            }
            if !has_waters {
                b.submerged = 0.0;
            }
            if b.sleeping {
                continue;
            }
            let aabb = b.world_aabb();
            if !aabb.is_valid() {
                continue;
            }
            let total = water::body_volume(b);
            if total <= 0.0 {
                continue;
            }
            for fluid in &fluids {
                let desc = fluid.desc();
                let level = match fluid.surface_near(aabb, desc.spacing * 2.0) {
                    Some(y) => y,
                    None => continue,
                };
                let center = Vec2::new(
                    (aabb.min.x + aabb.max.x) * 0.5,
                    (aabb.min.z + aabb.max.z) * 0.5,
                );
                let half_size = Vec2::new(
                    ((aabb.max.x - aabb.min.x) * 0.5).max(0.05),
                    ((aabb.max.z - aabb.min.z) * 0.5).max(0.05),
                );
                let w = WaterVolume {
                    center,
                    half_size,
                    level,
                    density: desc.rest_density,
                    drag: desc.drag,
                    flow: Vec2::ZERO,
                };
                let s = water::submerged_in(b, &w);
                if !s.is_submerged() {
                    continue;
                }
                let ratio = (s.volume / total).clamp(0.0, 1.0);
                b.submerged = b.submerged.max(ratio);
                // 局部气体体积分数：气泡占据的体积不再排开液体，所以把浮力
                // 用的密度按 (1 - 气体分数) 缩小。只是体积分数模型，不是气泡
                // 压力/相变；没有气泡时 gas = 0，旧路径逐位不变。
                let gas = fluid.gas_volume_near(aabb);
                let gas_fraction = if gas > 0.0 {
                    (gas / (s.volume + gas).max(1e-9)).clamp(0.0, 0.9)
                } else {
                    0.0
                };
                let density = desc.rest_density * (1.0 - gas_fraction);
                b.add_force_at_point(-gravity * (density * s.volume), s.centroid);
                if desc.drag > 0.0 {
                    let f = 1.0 / (1.0 + desc.drag * ratio * dt);
                    b.linear_velocity *= f;
                    b.angular_velocity *= f;
                }
                if desc.flow_coupling > 0.0 {
                    // 局部粒子流拖曳：只改 XZ，垂直速度留在原浮力/阻尼路径。
                    // 刚体周围没有粒子时完全跳过（否则会变成额外的零速阻尼）。
                    if let Some(flow) = fluid.flow_velocity_near(aabb) {
                        let f = 1.0 / (1.0 + desc.flow_coupling * ratio * dt);
                        b.linear_velocity.x = flow.x + (b.linear_velocity.x - flow.x) * f;
                        b.linear_velocity.z = flow.z + (b.linear_velocity.z - flow.z) * f;
                    }
                }
            }
        }
        self.fluids = fluids;
    }

    fn substep(&mut self, dt: f32) {
        if dt <= 0.0 || !dt.is_finite() {
            return;
        }
        // 渲染插值用：这一子步开始前的位姿。只复制，不参与求解，旧数值路径不变。
        for slot in &mut self.bodies {
            if let Some(body) = slot.body.as_mut() {
                body.prev_transform = body.transform;
            }
        }
        // 阶段计时（默认关闭：关着的时候连一次取时钟都不会调用）
        let mut lap_at = if self.phase_timing {
            Some(now_seconds())
        } else {
            None
        };
        let total_start = lap_at;
        macro_rules! lap {
            ($field:ident) => {
                if let Some(prev) = lap_at {
                    let now = now_seconds();
                    self.phase_timings.$field += (now - prev) * 1e6;
                    lap_at = Some(now);
                }
            };
        }
        // 0. 快照"施力之前"的速度（静摩擦/动摩擦判定要用，见 `scratch_pre_velocities`）
        let mut pre_velocities = std::mem::take(&mut self.scratch_pre_velocities);
        pre_velocities.clear();
        for slot in self.bodies.iter() {
            match &slot.body {
                Some(b) => pre_velocities.push((b.linear_velocity, b.angular_velocity)),
                None => pre_velocities.push((Vec3::ZERO, Vec3::ZERO)),
            }
        }
        // 0.5 浮力：在速度积分之前施力（与重力一样作为"本步外力"被消费）。
        self.apply_buoyancy(dt);
        // 1. 速度积分
        solver::integrate_velocities(&mut self.bodies, self.gravity, dt);
        lap!(integrate_velocity_us);
        // 2. 宽相 + 窄相
        self.collect_contacts(&mut lap_at);
        lap!(narrowphase_us);
        // 3. 视图（复用上一帧的缓冲：稳态下这里零分配）
        let mut views = std::mem::take(&mut self.scratch_views);
        let mut materials = std::mem::take(&mut self.scratch_materials);
        views.clear();
        materials.clear();
        for slot in self.bodies.iter() {
            match &slot.body {
                Some(b) => {
                    views.push(BodyView::from_body(b));
                    materials.push(solver::ContactMaterial {
                        dynamic_friction: b.friction,
                        static_friction: b.static_friction,
                        rolling_friction: b.rolling_friction,
                        restitution: b.restitution,
                        friction_combine: b.friction_combine,
                        restitution_combine: b.restitution_combine,
                    });
                }
                None => {
                    views.push(BodyView::frozen());
                    materials.push(solver::ContactMaterial {
                        dynamic_friction: 0.0,
                        static_friction: 0.0,
                        rolling_friction: 0.0,
                        restitution: 0.0,
                        friction_combine: None,
                        restitution_combine: None,
                    });
                }
            }
        }
        // 4. 关节准备（把关节表临时取出，避免借用冲突）
        let joints = std::mem::take(&mut self.joints);
        {
            let mut entries = std::mem::take(&mut self.scratch_joint_entries);
            entries.clear();
            for (index, slot) in joints.iter().enumerate() {
                if let Some(j) = &slot.joint {
                    let (ha, hb) = j.bodies();
                    let ia = self.body_index(ha);
                    let ib = self.body_index(hb);
                    if let (Some(ia), Some(ib)) = (ia, ib) {
                        entries.push(JointEntry {
                            a: ia,
                            b: ib,
                            joint_index: index,
                            impulses: slot.impulses,
                        });
                    }
                }
            }
            self.joint_solver.prepare(&views, &joints, &entries);
            self.scratch_joint_entries = entries;
        }
        // 5. 准备接触
        // 大场景（深堆）每个接触要迭代 10 次；把与几何有关的角速度响应预乘，
        // 速度迭代能省掉大量叉积/矩阵乘法。小场景保持旧的逐次运算，摘要逐位不变。
        // 阈值用接触点数（不是流形数）：4 点流形的开销是单点流形的 4 倍。
        let contact_points: usize = self.manifolds.iter().map(|m| m.points.len()).sum();
        let precompute = contact_points >= PRECOMPUTE_CONTACT_RESPONSE_MIN;
        self.contact_solver.prepare(
            &views,
            &materials,
            &pre_velocities,
            &self.manifolds,
            &self.cache,
            &self.config,
            dt,
            precompute,
        );
        // 5.5 确定性 island 划分（诊断 + 将来并行求解的依据）
        self.islands.begin(&views);
        for c in self.contact_solver.contacts.iter() {
            self.islands.union_pair(c.a, c.b);
        }
        for e in self.scratch_joint_entries.iter() {
            self.islands.union_pair(e.a, e.b);
        }
        let island_stats = self.islands.finish();
        self.phase_timings.islands = island_stats.islands;
        self.phase_timings.largest_island = island_stats.largest;
        self.phase_timings.multi_body_islands = island_stats.multi_body_islands;
        // 5.9 扭矩级 PD 马达：先作为外力施加一次，再进约束求解。
        self.joint_solver.apply_pd_torques(&mut views, dt);
        // 6. 热启动
        self.contact_solver.warm_start(&mut views, &self.config);
        self.joint_solver.warm_start(&mut views);
        lap!(prepare_us);
        // 7. 速度迭代
        let iters = self.config.velocity_iterations.max(1);
        let pos_iters = self.config.position_iterations;
        let relax_iters = self.config.relaxation_iterations;
        #[cfg(not(target_arch = "wasm32"))]
        let use_parallel = self.config.parallel
            && self.parallel.build(
                &self.islands,
                &views,
                &self.contact_solver.contacts,
                self.config.max_threads,
            );
        #[cfg(target_arch = "wasm32")]
        let use_parallel = false;
        self.parallel_used = use_parallel;
        if use_parallel {
            // 没有关节时（绝大多数纯刚体场景）：一次分发跑完全部迭代，
            // 每个岛内部保持「速度迭代 → 松弛 → 位置修正」的原顺序。
            #[cfg(not(target_arch = "wasm32"))]
            let no_joints = self.scratch_joint_entries.is_empty();
            #[cfg(not(target_arch = "wasm32"))]
            if no_joints {
                self.parallel.sync_from(&views);
                self.parallel
                    .solve_all(iters, relax_iters, 0.5, dt, &self.config, pos_iters);
                self.parallel.sync_to(&mut views);
                self.parallel
                    .flush_impulses(&mut self.contact_solver.contacts);
            }
            // 有关节时：每个迭代里「先全部接触（岛间并行）再全部关节（串行）」，
            // 与串行版的顺序完全一致。
            #[cfg(not(target_arch = "wasm32"))]
            for _ in 0..iters {
                if no_joints {
                    break;
                }
                self.parallel.sync_from(&views);
                self.parallel.solve_velocity();
                self.parallel.sync_to(&mut views);
                self.joint_solver.solve_velocity(&mut views, dt);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if !no_joints {
                self.parallel.sync_from(&views);
                self.parallel.relax(relax_iters, 0.5);
                self.parallel.sync_to(&mut views);
                self.parallel.sync_from(&views);
                self.parallel.solve_position(dt, &self.config, pos_iters);
                self.parallel.sync_to(&mut views);
                self.parallel
                    .flush_impulses(&mut self.contact_solver.contacts);
            }
        } else {
            for _ in 0..iters {
                self.contact_solver.solve_velocity(&mut views);
                self.joint_solver.solve_velocity(&mut views, dt);
            }
            self.contact_solver.relax(&mut views, relax_iters, 0.5);
        }
        lap!(velocity_us);
        // 8. 位置迭代（分裂冲量）
        // 并行路径两种情况都已经做过接触的位置修正，这里不能再做一遍
        // （否则有关节的场景会迭代两次位置约束）。
        #[cfg(not(target_arch = "wasm32"))]
        let parallel_did_position = use_parallel;
        #[cfg(target_arch = "wasm32")]
        let parallel_did_position = false;
        if !use_parallel || !parallel_did_position {
            self.contact_solver
                .solve_position(&mut views, dt, &self.config, pos_iters);
        }
        self.joint_solver.solve_position(
            &mut views,
            dt,
            pos_iters,
            self.config.baumgarte,
            self.config.max_correction_velocity,
        );
        lap!(position_us);
        // 9. 写回速度与伪速度
        for (i, view) in views.iter().enumerate() {
            if let Some(b) = self.bodies[i].body.as_mut() {
                b.linear_velocity = view.lin;
                b.angular_velocity = view.ang;
                b.pseudo_linear_velocity = view.pseudo_lin;
                b.pseudo_angular_velocity = view.pseudo_ang;
            }
        }
        // 9.5 CCD：开了 `ccd` 的刚体，先做一次扫掠，把这一步朝墙的那部分位移截在接触点。
        // 放在位置积分**之前**：改的是速度，位置积分照旧只按速度走一步（见 `apply_ccd`）。
        self.apply_ccd(dt);
        // 10. 位置积分
        solver::integrate_positions(&mut self.bodies, dt);
        // 10.5 粒子流体：用本步刚体位置做边界（M2 单向：刚体 → 流体）。
        self.step_fluids(dt);
        lap!(writeback_us);
        // 11. 更新接触冲量统计与缓存
        self.update_contact_impulses();
        self.contact_solver.store_cache(&mut self.cache);
        // 12. 关节冲量写回并归还关节表
        let mut impulses = std::mem::take(&mut self.scratch_joint_impulses);
        impulses.clear();
        impulses.resize(joints.len(), JointImpulses::default());
        self.joint_solver.store_impulses(&mut impulses);
        let mut joints = joints;
        let mut k = 0usize;
        for slot in joints.iter_mut() {
            if slot.joint.is_some() {
                if let Some(v) = impulses.get(k) {
                    slot.impulses = *v;
                }
                k += 1;
            }
        }
        self.joints = joints;
        // 12.5 关节断裂：本步约束力量级超过阈值就把关节**移除**
        //
        // 放在速度/位置迭代**之后**：断裂的那一步关节仍然正常起作用，所以行为是
        // "拉到超过阈值才断"，而不是"预测要断就提前松手"。
        //
        // 位置也有讲究：必须在 `self.joints = joints` **之后**——求解期间槽位数组被
        // `mem::take` 借走了，那会儿 `self.joints` 是空的，判定会静默失效。
        // 只有设了阈值的关节才做这一步（省掉绝大多数场景的一次遍历与一次分配）。
        if self
            .joints
            .iter()
            .any(|s| s.break_force.is_some() || s.break_torque.is_some())
        {
            let magnitudes = self.joint_solver.break_magnitudes(dt);
            let mut k = 0usize;
            let mut broken: Vec<(JointHandle, f32, f32)> = Vec::new();
            for (index, slot) in self.joints.iter().enumerate() {
                if slot.joint.is_none() {
                    continue;
                }
                let (force, torque) = magnitudes.get(k).copied().unwrap_or((0.0, 0.0));
                k += 1;
                let force_break = slot.break_force.is_some_and(|limit| force > limit);
                let torque_break = slot.break_torque.is_some_and(|limit| torque > limit);
                if force_break || torque_break {
                    broken.push((
                        JointHandle {
                            index: index as u32,
                            generation: slot.generation,
                        },
                        force,
                        torque,
                    ));
                }
            }
            for (handle, force, torque) in broken {
                let first = self
                    .joint(handle)
                    .map(|j| j.bodies().0)
                    .unwrap_or(BodyHandle::new(0, 0));
                if self.remove_joint(handle) {
                    self.broken_joints.push(handle);
                    let step = self.step_count;
                    self.push_event(PhysicsEvent {
                        step,
                        kind: PhysicsEventKind::JointBroken,
                        a: first,
                        b: None,
                        joint: Some(handle),
                        force,
                        torque,
                    });
                }
            }
        }

        // 归还复用缓冲（下一步继续用，稳态下不再分配）
        self.scratch_pre_velocities = pre_velocities;
        self.scratch_views = views;
        self.scratch_materials = materials;
        self.scratch_joint_impulses = impulses;
        // 13. 睡眠（被马达/参考姿态驱动的刚体这一步不睡，见 `update_sleeping` 的说明）
        let mut actuated = std::mem::take(&mut self.scratch_actuated);
        actuated.clear();
        actuated.resize(self.bodies.len(), false);
        for slot in self.joints.iter() {
            let driven = slot.joint.as_ref().is_some_and(|j| j.drives_bodies());
            if driven {
                let (ha, hb) = slot.joint.as_ref().unwrap().bodies();
                for h in [ha, hb] {
                    if let Some(flag) = actuated.get_mut(h.index as usize) {
                        *flag = true;
                    }
                }
            }
        }
        solver::update_sleeping(&mut self.bodies, dt, &self.config, &self.islands, &actuated);
        self.scratch_actuated = actuated;
        // 睡眠/唤醒事件：与"上一步谁在睡"做差分。
        //
        // 放在这里（而不是 `RigidBody::wake_up()` 里）是因为**唤醒的入口太多**：
        // 手动 `wake_up` / `wake_up_all` / 接触唤醒 / 关节唤醒 / 手动 `sleep`。
        // 逐入口埋点早晚会漏一个，而"每步比对一次状态"是单一事实来源。
        if self.sleeping_prev.len() != self.bodies.len() {
            self.sleeping_prev.clear();
            self.sleeping_prev.resize(self.bodies.len(), false);
        }
        let step = self.step_count;
        for i in 0..self.bodies.len() {
            let (awake_now, handle) = match self.bodies[i].body.as_ref() {
                Some(b) => (
                    !b.sleeping,
                    BodyHandle::new(i as u32, self.bodies[i].generation),
                ),
                None => continue,
            };
            let was_asleep = self.sleeping_prev[i];
            if was_asleep && awake_now {
                self.push_event(PhysicsEvent {
                    step,
                    kind: PhysicsEventKind::Wake,
                    a: handle,
                    b: None,
                    joint: None,
                    force: 0.0,
                    torque: 0.0,
                });
            } else if !was_asleep && !awake_now {
                self.push_event(PhysicsEvent {
                    step,
                    kind: PhysicsEventKind::Sleep,
                    a: handle,
                    b: None,
                    joint: None,
                    force: 0.0,
                    torque: 0.0,
                });
            }
            self.sleeping_prev[i] = !awake_now;
        }
        self.step_count += 1;
        if let Some(prev) = lap_at {
            let now = now_seconds();
            self.phase_timings.bookkeeping_us += (now - prev) * 1e6;
        }
        if let Some(start) = total_start {
            self.phase_timings.total_us += (now_seconds() - start) * 1e6;
            self.phase_timings.steps += 1;
        }
    }

    /// 句柄 → 槽位下标（世代校验）。
    fn body_index(&self, handle: BodyHandle) -> Option<usize> {
        let slot = self.bodies.get(handle.index as usize)?;
        if slot.generation == handle.generation && slot.body.is_some() {
            Some(handle.index as usize)
        } else {
            None
        }
    }

    /// 串行填充窄相结果（也是并行路径关闭/候选太少时的回退）。
    fn fill_narrowphase_hits_serial(&mut self) {
        self.narrowphase_hits.resize(self.pairs.len(), None);
        for k in 0..self.pairs.len() {
            let (ia, ib) = self.pairs[k];
            let (ia, ib) = (ia as usize, ib as usize);
            let hit = if self.narrowphase_pair_ok[k] {
                match (
                    self.bodies.get(ia).and_then(|s| s.body.as_ref()),
                    self.bodies.get(ib).and_then(|s| s.body.as_ref()),
                ) {
                    (Some(a), Some(b)) => collide_bodies(a, b, self.config.speculative_margin),
                    _ => None,
                }
            } else {
                None
            };
            self.narrowphase_hits[k] = hit;
        }
    }

    /// native 并行填充窄相结果；成功返回 true。
    #[cfg(not(target_arch = "wasm32"))]
    fn fill_narrowphase_hits_parallel(&mut self) -> bool {
        const MIN_PAIRS: usize = 256;
        let n = self.pairs.len();
        if n < MIN_PAIRS || !self.config.parallel_narrowphase || !self.config.parallel {
            return false;
        }
        let hw = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(1);
        let cap = if self.config.max_threads == 0 {
            8
        } else {
            self.config.max_threads.min(8)
        };
        let threads = hw.min(cap);
        if threads < 2 {
            return false;
        }
        let chunk = n.div_ceil(threads);
        let bodies = &self.bodies;
        let pairs = &self.pairs;
        let ok = &self.narrowphase_pair_ok;
        let hits = &mut self.narrowphase_hits;
        let margin = self.config.speculative_margin;
        std::thread::scope(|scope| {
            for ((pair_chunk, ok_chunk), hit_chunk) in pairs
                .chunks(chunk)
                .zip(ok.chunks(chunk))
                .zip(hits.chunks_mut(chunk))
            {
                scope.spawn(move || {
                    for ((pair, ok), slot) in
                        pair_chunk.iter().zip(ok_chunk).zip(hit_chunk.iter_mut())
                    {
                        let (ia, ib) = (pair.0 as usize, pair.1 as usize);
                        *slot = if *ok {
                            match (
                                bodies.get(ia).and_then(|s| s.body.as_ref()),
                                bodies.get(ib).and_then(|s| s.body.as_ref()),
                            ) {
                                (Some(a), Some(b)) => collide_bodies(a, b, margin),
                                _ => None,
                            }
                        } else {
                            None
                        };
                    }
                });
            }
        });
        true
    }

    /// 宽相 + 窄相，并处理唤醒。
    /// 收集接触。
    ///
    /// `lap_at` 是阶段计时的「上次打点」（关闭计时时为 `None`）：
    /// 宽相结束后会在这里再打一次点，把宽相与窄相分开统计。
    fn collect_contacts(&mut self, lap_at: &mut Option<f64>) {
        let mut manifolds = std::mem::take(&mut self.manifolds);
        let mut contacts = std::mem::take(&mut self.contacts);
        let mut wake = std::mem::take(&mut self.wake_list);
        let mut sensor_pairs = std::mem::take(&mut self.sensor_pairs);
        let mut touch_pairs = std::mem::take(&mut self.touch_pairs);
        manifolds.clear();
        contacts.clear();
        wake.clear();
        sensor_pairs.clear();
        touch_pairs.clear();
        if !self.collisions_enabled {
            self.manifolds = manifolds;
            self.contacts = contacts;
            self.wake_list = wake;
            self.sensor_pairs = sensor_pairs;
            self.touch_pairs = touch_pairs;
            return;
        }

        // --- 代理 ---
        self.proxies.clear();
        for (i, slot) in self.bodies.iter().enumerate() {
            let b = match &slot.body {
                Some(b) => b,
                None => continue,
            };
            let global = matches!(b.shape, Shape::Plane { .. });
            let aabb = if global {
                Aabb::new(Vec3::NEG_INFINITY, Vec3::INFINITY)
            } else {
                b.world_aabb()
            };
            self.proxies.push(Proxy {
                index: i as u32,
                aabb,
                // 运动学体也必须算「可动」：它质量无穷大、永远不睡，但会推动别人。
                // 只写 `== Dynamic` 的话，运动学推板撞上睡眠刚体时连候选对都不生成，
                // `is_moving`/唤醒那一段永远走不到（曾经的 bug，见 sim_tests 回归用例）。
                movable: matches!(b.body_type, BodyType::Dynamic | BodyType::Kinematic)
                    && !b.is_sleeping(),
                sleeping: b.is_sleeping(),
                global,
                groups: b.groups,
            });
        }

        // --- 候选对 ---
        self.broadphase
            .compute_pairs(&self.proxies, &mut self.pairs);
        if let Some(prev) = *lap_at {
            let now = now_seconds();
            self.phase_timings.broadphase_us += (now - prev) * 1e6;
            *lap_at = Some(now);
            self.phase_timings.candidate_pairs += self.pairs.len() as u64;
        }

        // 分组/关节过滤先串行过一遍（很便宜），后续求交可以按候选对并行。
        // 分组过滤在宽相已经做过一遍；这里再查一次是因为调用方可以直接改
        // `body.groups`（宽相用的是本子步开头的快照，中间改了要以当前值为准）。
        // 关节两端缺省不互相碰撞（Bullet / PhysX 的 `collide_connected = false`）。
        self.narrowphase_pair_ok.clear();
        self.narrowphase_pair_ok.reserve(self.pairs.len());
        for &(ia, ib) in self.pairs.iter() {
            let (ia, ib) = (ia as usize, ib as usize);
            let ok = match (
                self.bodies.get(ia).and_then(|s| s.body.as_ref()),
                self.bodies.get(ib).and_then(|s| s.body.as_ref()),
            ) {
                (Some(a), Some(b)) => {
                    a.groups.interacts(b.groups)
                        && (self.joints.is_empty() || !self.joint_blocks_contact(ia, ib))
                }
                _ => false,
            };
            self.narrowphase_pair_ok.push(ok);
        }
        self.narrowphase_hits.resize(self.pairs.len(), None);
        self.narrowphase_parallel_used = false;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.narrowphase_parallel_used = self.fill_narrowphase_hits_parallel();
        }
        if !self.narrowphase_parallel_used {
            self.fill_narrowphase_hits_serial();
        }

        for k in 0..self.pairs.len() {
            let (ia, ib) = self.pairs[k];
            let (ia, ib) = (ia as usize, ib as usize);
            let Some(m) = self.narrowphase_hits[k].take() else {
                continue;
            };
            let sensor_pair = match (
                self.bodies.get(ia).and_then(|s| s.body.as_ref()),
                self.bodies.get(ib).and_then(|s| s.body.as_ref()),
            ) {
                (Some(a), Some(b)) => a.sensor || b.sensor,
                _ => continue,
            };

            contacts.push(ContactPair {
                a: BodyHandle::new(ia as u32, self.bodies[ia].generation),
                b: BodyHandle::new(ib as u32, self.bodies[ib].generation),
                normal: m.points[0].normal,
                point: m.points[0].position,
                impulse: 0.0,
                sensor: sensor_pair,
            });

            // 传感器：只进接触表与事件，**不进求解器**（不产生冲量），
            // 也不参与唤醒——一个巨大的触发区不该把整个世界唤醒。
            if sensor_pair {
                sensor_pairs.push((ia as u32, ib as u32));
                continue;
            }

            // 事件总线用的"真实接触对"（不含传感器：传感器的进入/离开是另一类事件）
            touch_pairs.push((ia as u32, ib as u32));
            let mut entry = ManifoldEntry {
                a: ia,
                b: ib,
                points: SmallVec::new(),
            };
            entry.points.extend_from_slice(&m.points);
            manifolds.push(entry);

            // 唤醒：接触对方在运动 → 唤醒睡眠体。
            let moving_a = self.is_moving(ia);
            let moving_b = self.is_moving(ib);
            if moving_a && self.is_sleeping(ib) {
                wake.push(ib as u32);
            }
            if moving_b && self.is_sleeping(ia) {
                wake.push(ia as u32);
            }
        }

        // 关节唤醒：伙伴在运动 → 唤醒。
        for slot in self.joints.iter() {
            if let Some(j) = &slot.joint {
                let (ha, hb) = j.bodies();
                if let (Some(ia), Some(ib)) = (self.body_index(ha), self.body_index(hb)) {
                    if self.is_moving(ia) && self.is_sleeping(ib) {
                        wake.push(ib as u32);
                    }
                    if self.is_moving(ib) && self.is_sleeping(ia) {
                        wake.push(ia as u32);
                    }
                }
            }
        }

        wake.sort_unstable();
        wake.dedup();
        for &i in wake.iter() {
            if let Some(b) = self.bodies[i as usize].body.as_mut() {
                b.wake_up();
            }
        }

        // 传感器事件：本子步与上一子步的重叠集合做差分（两边都已按槽位排序）。
        self.sensor_pairs_prev.sort_unstable();
        self.sensor_pairs_prev.dedup();
        self.sensor_events.clear();
        {
            let (mut i, mut j) = (0usize, 0usize);
            while i < sensor_pairs.len() || j < self.sensor_pairs_prev.len() {
                let cur = sensor_pairs.get(i).copied();
                let prev = self.sensor_pairs_prev.get(j).copied();
                match (cur, prev) {
                    (Some(c), Some(p)) if c == p => {
                        i += 1;
                        j += 1;
                    }
                    (Some(c), Some(p)) if c < p => {
                        self.push_sensor_event(c, true);
                        i += 1;
                    }
                    (Some(_), Some(_)) => {
                        // 上一子步有、这一子步没有 → 离开
                        self.push_sensor_event(self.sensor_pairs_prev[j], false);
                        j += 1;
                    }
                    (Some(c), None) => {
                        self.push_sensor_event(c, true);
                        i += 1;
                    }
                    (None, Some(_)) => {
                        self.push_sensor_event(self.sensor_pairs_prev[j], false);
                        j += 1;
                    }
                    (None, None) => break,
                }
            }
        }
        std::mem::swap(&mut self.sensor_pairs_prev, &mut sensor_pairs);

        // 接触开始/结束：与上一子步的"真实接触对"做同样的差分。
        //
        // 顺序：按 (a, b) 排好再差分，保证事件顺序与宽相顺序无关（确定性）。
        touch_pairs.sort_unstable();
        touch_pairs.dedup();
        self.touch_pairs_prev.sort_unstable();
        self.touch_pairs_prev.dedup();
        {
            let (mut i, mut j) = (0usize, 0usize);
            while i < touch_pairs.len() || j < self.touch_pairs_prev.len() {
                let cur = touch_pairs.get(i).copied();
                let prev = self.touch_pairs_prev.get(j).copied();
                match (cur, prev) {
                    (Some(c), Some(p)) if c == p => {
                        i += 1;
                        j += 1;
                    }
                    (Some(c), Some(p)) if c < p => {
                        self.push_pair_event(PhysicsEventKind::ContactBegin, c.0, c.1);
                        i += 1;
                    }
                    (Some(_), Some(_)) => {
                        let p = self.touch_pairs_prev[j];
                        self.push_pair_event(PhysicsEventKind::ContactEnd, p.0, p.1);
                        j += 1;
                    }
                    (Some(c), None) => {
                        self.push_pair_event(PhysicsEventKind::ContactBegin, c.0, c.1);
                        i += 1;
                    }
                    (None, Some(_)) => {
                        let p = self.touch_pairs_prev[j];
                        self.push_pair_event(PhysicsEventKind::ContactEnd, p.0, p.1);
                        j += 1;
                    }
                    (None, None) => break,
                }
            }
        }
        std::mem::swap(&mut self.touch_pairs_prev, &mut touch_pairs);
        self.touch_pairs = touch_pairs;

        self.manifolds = manifolds;
        self.contacts = contacts;
        self.wake_list = wake;
        if self.phase_timing {
            self.phase_timings.manifolds += self.manifolds.len() as u64;
        }
    }

    /// 球体对**所有形状**的扫掠：球心沿 `motion` 走，返回最早接触参数 `t∈[0,1]`
    /// 与接触法线（由对方指向球心）。
    ///
    /// 中心射线只能覆盖"球心航线打到表面"；球擦着盒角/柱顶时球心航线从上方/侧面
    /// 掠过，但球面已经碰到，旧路径会整步穿过去。盒子用球心到盒面的保守推进，
    /// 球用解析二次方程，半空间用线性求交；胶囊/圆柱/圆锥/圆角盒/凸包/三角网等
    /// 用 `distance_point_to_shape` 的距离场做保守推进（距离场 1-Lipschitz，
    /// `t += (dist-r)/|motion|` 不会越过首次接触），法线取距离场中心差分。
    /// 扫掠用**相对运动**：目标这一步按速度线形外推（静态体为零），所以“目标撞上来”
    /// 也能被看到。目标自转对盒子与其它非平面形状用时间采样（角点移动不超过
    /// 半径/4）；旋转平面与球仍按静态姿态。
    /// 旋转目标的球心扫掠（时间采样）：把这一步按“角点移动 ≤ 半径/4”采样，
    /// 每个时刻用旋转后的形状距离场判断球面接触。盒子与其它凸形状共用。
    fn sphere_sweep_rotating_shape(
        &self,
        shape: &Shape,
        base: &Transform,
        sweep: RotatingSweep,
    ) -> Option<(f32, Vec3)> {
        let RotatingSweep {
            omega,
            p0,
            rel_motion,
            radius,
            rotation_reach,
            dt,
        } = sweep;
        let omega_speed = omega.length();
        if omega_speed <= 1e-6 || !(rotation_reach.is_finite() && rotation_reach > 0.0) {
            return None;
        }
        let samples = ((rotation_reach / (radius * 0.25).max(1e-3)).ceil() as usize).clamp(16, 512);
        for k in 0..=samples {
            let t = k as f32 / samples as f32;
            let p = p0 + rel_motion * t;
            let tf = Transform {
                position: base.position,
                rotation: base.rotation
                    * Quat::from_axis_angle(omega.normalize_or_zero(), omega_speed * t * dt),
                scale: base.scale,
            };
            let dist = gjk::distance_point_to_shape(shape, &tf, p);
            if !dist.is_finite() {
                return None;
            }
            if dist <= radius + 1e-4 {
                let eps = (radius * 0.125).max(1e-4);
                let dx = gjk::distance_point_to_shape(shape, &tf, p + Vec3::X * eps)
                    - gjk::distance_point_to_shape(shape, &tf, p - Vec3::X * eps);
                let dy = gjk::distance_point_to_shape(shape, &tf, p + Vec3::Y * eps)
                    - gjk::distance_point_to_shape(shape, &tf, p - Vec3::Y * eps);
                let dz = gjk::distance_point_to_shape(shape, &tf, p + Vec3::Z * eps)
                    - gjk::distance_point_to_shape(shape, &tf, p - Vec3::Z * eps);
                let grad = Vec3::new(dx, dy, dz);
                let n = if grad.length_squared() > 1e-12 {
                    grad.normalize()
                } else {
                    -rel_motion.normalize_or_zero()
                };
                return Some((t, n));
            }
        }
        None
    }

    /// 凸-凸精确 CCD：用 GJK 距离做保守推进，直到两形状真正接触。
    ///
    /// 距离函数在时间上是 1-Lipschitz 的，速度上界
    /// `|v_rel| + |ω_a|·R_a + |ω_b|·R_b` 保证 `Δ距离 / 上界` 不会越过首次接触。
    /// 只处理两侧都是凸形状（半空间/三角网走默认射线/角点路径）；返回归一化
    /// 时间 `t∈[0,1]`。每个开了 `ccd_precise` 的物体每子步 O(其它体)，是
    /// 明确的高成本精确路径，不是默认开关。
    fn sweep_convex_bodies(&self, self_index: usize, dt: f32) -> Option<f32> {
        let a = self.bodies.get(self_index)?.body.as_ref()?;
        if a.shape.is_static_only() || a.sensor || !(dt.is_finite() && dt > 0.0) {
            return None;
        }
        let groups = a.groups;
        let ra = a.shape.bounding_radius();
        let mut best: Option<f32> = None;
        for (i, slot) in self.bodies.iter().enumerate() {
            if i == self_index {
                continue;
            }
            let Some(b) = &slot.body else { continue };
            let plane = matches!(b.shape, Shape::Plane { .. });
            let trimesh = matches!(b.shape, Shape::TriMesh { .. });
            if b.sensor
                || (b.shape.is_static_only() && !plane && !trimesh)
                || !b.groups.interacts(groups)
            {
                continue;
            }
            // 半空间/三角网没有有限包围球；三角网按逐三角形距离推进，自身按静态处理。
            let rb = if plane || trimesh {
                0.0
            } else {
                b.shape.bounding_radius()
            };
            if !(ra.is_finite() && rb.is_finite()) {
                continue;
            }
            let rel_v = a.linear_velocity - b.linear_velocity;
            let bound = rel_v.length()
                + a.angular_velocity.length() * ra
                + b.angular_velocity.length() * rb;
            if !(bound.is_finite() && bound > 1e-6) {
                continue;
            }
            let step = bound * dt;
            let tf = |body: &RigidBody, t: f32| {
                let speed = body.angular_velocity.length();
                let rotation = if speed > 1e-9 {
                    body.transform.rotation
                        * Quat::from_axis_angle(
                            body.angular_velocity.normalize_or_zero(),
                            speed * dt * t,
                        )
                } else {
                    body.transform.rotation
                };
                Transform {
                    position: body.transform.position + body.linear_velocity * (dt * t),
                    rotation,
                    scale: body.transform.scale,
                }
            };
            // 三角网局部 AABB 只算一次；每个时间点只做刚体变换 + 间隙比较。
            let tri_bounds = if let Shape::TriMesh { vertices, indices } = &b.shape {
                let mut out = Vec::with_capacity(indices.len() / 3);
                for tri in indices.chunks(3) {
                    if tri.len() < 3 {
                        continue;
                    }
                    let (Some(v0), Some(v1), Some(v2)) = (
                        vertices.get(tri[0] as usize),
                        vertices.get(tri[1] as usize),
                        vertices.get(tri[2] as usize),
                    ) else {
                        out.push(Aabb::EMPTY);
                        continue;
                    };
                    out.push(Aabb::from_points(&[*v0, *v1, *v2]));
                }
                Some(out)
            } else {
                None
            };
            let tri_grid = tri_bounds.as_ref().and_then(|b| TriGrid::build(b));
            let mut candidates: Vec<u32> = Vec::new();
            let mut t = 0.0f32;
            for _ in 0..64 {
                let ta = tf(a, t);
                let tb = tf(b, t);
                // 半空间是无限支撑，GJK 没有有限支持点；用形状沿 -法线 的
                // 支持点做 1-Lipschitz 保守推进。盒子/凸包/任意凸形状都适用。
                if let Shape::Plane { normal } = &b.shape {
                    let n = tb.rotation.mul_vec3(*normal);
                    if n.length_squared() < 0.5 {
                        break;
                    }
                    let n = n.normalize();
                    let sa = gjk::Support::new(&a.shape, &ta);
                    let support = sa.support(-n);
                    let distance = (support - tb.position).dot(n);
                    if distance <= 1e-4 {
                        if t > 1e-5 && best.is_none_or(|cur| t < cur) {
                            best = Some(t);
                        }
                        break;
                    }
                    let advance = distance / step;
                    if !(advance.is_finite() && advance > 1e-6) {
                        break;
                    }
                    t += advance;
                    if t >= 1.0 {
                        break;
                    }
                    continue;
                }
                // 三角网：先用 AABB 间隙找到最近的候选三角形，取得真实距离后
                // 只对间隙小于它的三角形跑 GJK。薄墙/大网格都不再逐三角形全扫。
                if let (Shape::TriMesh { vertices, indices }, Some(bounds)) =
                    (&b.shape, tri_bounds.as_ref())
                {
                    let sa = gjk::Support::new(&a.shape, &ta);
                    let a_world = a.shape.local_aabb().transformed(&ta.matrix());
                    let triangle = |k: usize| -> Option<[Vec3; 3]> {
                        let tri = indices.get(k * 3..k * 3 + 3)?;
                        let v0 = vertices.get(tri[0] as usize)?;
                        let v1 = vertices.get(tri[1] as usize)?;
                        let v2 = vertices.get(tri[2] as usize)?;
                        Some([
                            tb.position + tb.rotation.mul_vec3(*v0),
                            tb.position + tb.rotation.mul_vec3(*v1),
                            tb.position + tb.rotation.mul_vec3(*v2),
                        ])
                    };
                    // 球-三角形有解析最近点；不要为退化三角形跑 GJK/EPA。
                    let sphere = if let Shape::Sphere { radius } = a.shape {
                        Some((ta.position, radius.abs()))
                    } else {
                        None
                    };
                    let test_triangle = |pts: [Vec3; 3]| -> (bool, f32) {
                        if let Some((center, radius)) = sphere {
                            let d = gjk::point_triangle_distance(center, pts[0], pts[1], pts[2])
                                - radius;
                            (d <= 1e-4, d.max(0.0))
                        } else {
                            let hit = gjk::gjk(&sa, &gjk::Support::triangle(pts));
                            (hit.intersect || hit.distance <= 1e-4, hit.distance)
                        }
                    };
                    // 把局部 AABB 按当前刚体位姿变成世界 AABB（保守：先取绝对值）。
                    // 静态/纯平移三角网走无旋转快路径；大多数地面/墙都是这种。
                    let identity = tb.rotation == Quat::IDENTITY && tb.scale == Vec3::ONE;
                    let world_bounds = |local: &Aabb| -> Aabb {
                        if identity {
                            return Aabb::new(local.min + tb.position, local.max + tb.position);
                        }
                        let center = tb.position + tb.rotation.mul_vec3(local.center() * tb.scale);
                        let half = local.half_extents() * tb.scale;
                        let ext = (tb.rotation.mul_vec3(Vec3::new(half.x, 0.0, 0.0))).abs()
                            + (tb.rotation.mul_vec3(Vec3::new(0.0, half.y, 0.0))).abs()
                            + (tb.rotation.mul_vec3(Vec3::new(0.0, 0.0, half.z))).abs();
                        Aabb::new(center - ext, center + ext)
                    };
                    // 查询 AABB 在三角网局部空间的版本；有网格时只取局部邻域。
                    candidates.clear();
                    if let Some(grid) = tri_grid.as_ref() {
                        let inv = tb.rotation.inverse();
                        let mut local_aabb = Aabb::EMPTY;
                        for corner in 0..8 {
                            let p = Vec3::new(
                                if corner & 1 == 0 {
                                    a_world.min.x
                                } else {
                                    a_world.max.x
                                },
                                if corner & 2 == 0 {
                                    a_world.min.y
                                } else {
                                    a_world.max.y
                                },
                                if corner & 4 == 0 {
                                    a_world.min.z
                                } else {
                                    a_world.max.z
                                },
                            );
                            let q = inv.mul_vec3(p - tb.position);
                            local_aabb.expand_point(Vec3::new(
                                q.x / tb.scale.x,
                                q.y / tb.scale.y,
                                q.z / tb.scale.z,
                            ));
                        }
                        grid.candidates(&local_aabb, &mut candidates);
                    } else {
                        candidates.extend(0..bounds.len() as u32);
                    }
                    let mut nearest = None;
                    let mut nearest_gap = f32::INFINITY;
                    for &k in candidates.iter() {
                        let k = k as usize;
                        let local = &bounds[k];
                        if !local.is_valid() {
                            continue;
                        }
                        let gap = aabb_gap(&a_world, &world_bounds(local));
                        if gap < nearest_gap {
                            nearest_gap = gap;
                            nearest = Some(k);
                        }
                    }
                    // AABB 间隙本身是三角形距离的下界。间隙还没归零时，
                    // 直接按它推进，完全不必跑 GJK；只有 AABB 已贴上才需要
                    // 精确距离/接触判定。
                    if nearest_gap > 1e-4 {
                        let advance = nearest_gap / step;
                        if !(advance.is_finite() && advance > 1e-6) {
                            break;
                        }
                        t += advance;
                        if t >= 1.0 {
                            break;
                        }
                        continue;
                    }
                    let mut min_distance = f32::INFINITY;
                    let mut touched = false;
                    if let Some(k) = nearest {
                        if let Some(points) = triangle(k) {
                            let (hit, distance) = test_triangle(points);
                            if hit {
                                touched = true;
                            } else {
                                min_distance = distance;
                            }
                        }
                    }
                    if !touched {
                        for &k in candidates.iter() {
                            let k = k as usize;
                            let local = &bounds[k];
                            if !local.is_valid() {
                                continue;
                            }
                            let gap = aabb_gap(&a_world, &world_bounds(local));
                            if gap >= min_distance {
                                continue;
                            }
                            let Some(points) = triangle(k) else {
                                continue;
                            };
                            let (hit, distance) = test_triangle(points);
                            if hit {
                                touched = true;
                                break;
                            }
                            min_distance = min_distance.min(distance);
                        }
                    }
                    if touched {
                        if t > 1e-5 && best.is_none_or(|cur| t < cur) {
                            best = Some(t);
                        }
                        break;
                    }
                    if !min_distance.is_finite() {
                        break;
                    }
                    let advance = min_distance / step;
                    if !(advance.is_finite() && advance > 1e-6) {
                        break;
                    }
                    t += advance;
                    if t >= 1.0 {
                        break;
                    }
                    continue;
                }
                let ga = gjk::Support::new(&a.shape, &ta);
                let gb = gjk::Support::new(&b.shape, &tb);
                let hit = gjk::gjk(&ga, &gb);
                if hit.intersect || hit.distance <= 1e-4 {
                    // 起始姿态已经接触时交给窄相/求解器，不要把速度乘 0 冻住
                    // 一个本来只是贴在地板上的精确 CCD 物体。
                    if t > 1e-5 && best.is_none_or(|cur| t < cur) {
                        best = Some(t);
                    }
                    break;
                }
                let advance = hit.distance / step;
                if !(advance.is_finite() && advance > 1e-6) {
                    break;
                }
                t += advance;
                if t >= 1.0 {
                    break;
                }
            }
        }
        best
    }

    fn sphere_sweep_bodies(
        &self,
        self_index: usize,
        p0: Vec3,
        motion: Vec3,
        radius: f32,
        dt: f32,
    ) -> Option<(f32, Vec3)> {
        if !p0.is_finite()
            || !motion.is_finite()
            || !(radius.is_finite() && radius > 0.0)
            || !(dt.is_finite() && dt > 0.0)
        {
            return None;
        }
        let groups = self.bodies.get(self_index)?.body.as_ref()?.groups;
        let sweep = motion.length() * 1.0 + radius;
        let mut best: Option<(f32, Vec3)> = None;
        for (i, slot) in self.bodies.iter().enumerate() {
            if i == self_index {
                continue;
            }
            let Some(body) = &slot.body else { continue };
            if body.sensor || !body.groups.interacts(groups) {
                continue;
            }
            // 目标这一步自己的位移。静态体不动；运动学/动态体按当前速度线性外推。
            // 用相对运动扫掠，才能抓住“球没动、目标撞上来”或反向对撞。
            let target_motion = if body.body_type == BodyType::Static {
                Vec3::ZERO
            } else {
                body.linear_velocity * dt
            };
            let target_omega = if body.body_type == BodyType::Static {
                Vec3::ZERO
            } else {
                body.angular_velocity
            };
            let rel_motion = motion - target_motion;
            let rel_speed = rel_motion.length();
            let aabb = body.world_aabb();
            // 旋转目标扫过的范围可能远大于其初始 AABB（长条盒转 90°），预筛按
            // 角速度 × 包围半径 × dt 膨胀。
            let rotation_reach = target_omega.length() * body.shape.bounding_radius() * dt;
            let grow = sweep + target_motion.length() + rotation_reach;
            if aabb.is_valid()
                && !aabb.grown(grow).contains_point(p0)
                && !aabb.grown(grow).contains_point(p0 + motion)
            {
                continue;
            }
            let hit = match &body.shape {
                Shape::Plane { normal } => {
                    let n = body.transform.rotation.mul_vec3(normal.normalize_or_zero());
                    if n.length_squared() < 0.5 {
                        None
                    } else {
                        let denom = rel_motion.dot(n);
                        let signed = (p0 - body.transform.position).dot(n);
                        if signed <= radius {
                            Some((0.0, n))
                        } else if denom < -1e-9 {
                            let t = (radius - signed) / denom;
                            (0.0..=1.0).contains(&t).then_some((t, n))
                        } else {
                            None
                        }
                    }
                }
                Shape::Sphere { radius: other_r } => {
                    let rel = p0 - body.transform.position;
                    let rr = radius + other_r.abs();
                    let c = rel.length_squared() - rr * rr;
                    if c <= 0.0 {
                        Some((0.0, rel.normalize_or_zero()))
                    } else {
                        let a = rel_motion.length_squared();
                        let b = 2.0 * rel.dot(rel_motion);
                        let disc = b * b - 4.0 * a * c;
                        if a <= 1e-12 || disc < 0.0 {
                            None
                        } else {
                            let t = (-b - disc.sqrt()) / (2.0 * a);
                            (0.0..=1.0).contains(&t).then(|| {
                                let n = (p0 + rel_motion * t - body.transform.position)
                                    .normalize_or_zero();
                                (t, n)
                            })
                        }
                    }
                }
                Shape::Box { half_extents } => {
                    let omega_speed = target_omega.length();
                    if omega_speed > 1e-6 {
                        // 旋转盒：把这一步按“角点移动不超过半径/4”采样成若干时刻，
                        // 每个时刻把盒子转到 `ω·t`，在相对位移 `p0 + rel_motion·t`
                        // 处求到盒面的距离。旋转目标没有简单的 1-Lipschitz 距离场，
                        // 所以这里是**时间采样**而不是保守推进；采样上限 512。
                        // 这是旋转目标 CCD 的第一个形状；其它形状仍按静态姿态扫。
                        let shape = Shape::Box {
                            half_extents: *half_extents,
                        };
                        let samples = ((rotation_reach / (radius * 0.25).max(1e-3)).ceil()
                            as usize)
                            .clamp(16, 512);
                        let mut found = None;
                        for k in 0..=samples {
                            let t = k as f32 / samples as f32;
                            let p = p0 + rel_motion * t;
                            let tf = Transform {
                                position: body.transform.position,
                                rotation: body.transform.rotation
                                    * Quat::from_axis_angle(
                                        target_omega.normalize_or_zero(),
                                        omega_speed * t * dt,
                                    ),
                                scale: body.transform.scale,
                            };
                            let dist = gjk::distance_point_to_shape(&shape, &tf, p);
                            if !dist.is_finite() {
                                break;
                            }
                            if dist <= radius + 1e-4 {
                                let eps = (radius * 0.125).max(1e-4);
                                let dx =
                                    gjk::distance_point_to_shape(&shape, &tf, p + Vec3::X * eps)
                                        - gjk::distance_point_to_shape(
                                            &shape,
                                            &tf,
                                            p - Vec3::X * eps,
                                        );
                                let dy =
                                    gjk::distance_point_to_shape(&shape, &tf, p + Vec3::Y * eps)
                                        - gjk::distance_point_to_shape(
                                            &shape,
                                            &tf,
                                            p - Vec3::Y * eps,
                                        );
                                let dz =
                                    gjk::distance_point_to_shape(&shape, &tf, p + Vec3::Z * eps)
                                        - gjk::distance_point_to_shape(
                                            &shape,
                                            &tf,
                                            p - Vec3::Z * eps,
                                        );
                                let grad = Vec3::new(dx, dy, dz);
                                let n = if grad.length_squared() > 1e-12 {
                                    grad.normalize()
                                } else {
                                    -rel_motion.normalize_or_zero()
                                };
                                found = Some((t, n));
                                break;
                            }
                        }
                        found
                    } else if rel_speed <= 1e-9 {
                        None
                    } else {
                        let inv = body.transform.rotation.inverse();
                        let center0 = inv.mul_vec3(p0 - body.transform.position);
                        let local_motion = inv.mul_vec3(rel_motion);
                        let he = half_extents.abs();
                        let mut t = 0.0f32;
                        let mut found = None;
                        for _ in 0..32 {
                            let center = center0 + local_motion * t;
                            let closest = Vec3::new(
                                center.x.clamp(-he.x, he.x),
                                center.y.clamp(-he.y, he.y),
                                center.z.clamp(-he.z, he.z),
                            );
                            let delta = center - closest;
                            let dist = delta.length();
                            if dist <= radius + 1e-4 {
                                let n_local = if dist > 1e-6 {
                                    delta / dist
                                } else {
                                    -local_motion.normalize_or_zero()
                                };
                                let n = body.transform.rotation.mul_vec3(n_local);
                                found = Some((t.clamp(0.0, 1.0), n));
                                break;
                            }
                            let step = (dist - radius) / rel_speed;
                            if !step.is_finite() || step <= 1e-7 {
                                break;
                            }
                            t += step;
                            if t > 1.0 {
                                break;
                            }
                        }
                        found
                    }
                }
                _ => {
                    // 其它凸形状/三角网：球心到表面的距离做保守推进。距离场是
                    // 1-Lipschitz 的，`t += (dist-r)/speed` 不会越过首次接触；
                    // 法线用距离场中心差分，退化时退回“朝运动反方向”。
                    // 目标自转时改用时间采样（与盒子同一套），否则静态姿态会漏。
                    if target_omega.length() > 1e-6 {
                        self.sphere_sweep_rotating_shape(
                            &body.shape,
                            &body.transform,
                            RotatingSweep {
                                omega: target_omega,
                                p0,
                                rel_motion,
                                radius,
                                rotation_reach,
                                dt,
                            },
                        )
                    } else if rel_speed <= 1e-9 {
                        None
                    } else {
                        let mut t = 0.0f32;
                        let mut found = None;
                        for _ in 0..48 {
                            let p = p0 + rel_motion * t;
                            let dist =
                                gjk::distance_point_to_shape(&body.shape, &body.transform, p);
                            if !dist.is_finite() {
                                break;
                            }
                            if dist <= radius + 1e-4 {
                                let eps = (radius * 0.125).max(1e-4);
                                let dx = gjk::distance_point_to_shape(
                                    &body.shape,
                                    &body.transform,
                                    p + Vec3::X * eps,
                                ) - gjk::distance_point_to_shape(
                                    &body.shape,
                                    &body.transform,
                                    p - Vec3::X * eps,
                                );
                                let dy = gjk::distance_point_to_shape(
                                    &body.shape,
                                    &body.transform,
                                    p + Vec3::Y * eps,
                                ) - gjk::distance_point_to_shape(
                                    &body.shape,
                                    &body.transform,
                                    p - Vec3::Y * eps,
                                );
                                let dz = gjk::distance_point_to_shape(
                                    &body.shape,
                                    &body.transform,
                                    p + Vec3::Z * eps,
                                ) - gjk::distance_point_to_shape(
                                    &body.shape,
                                    &body.transform,
                                    p - Vec3::Z * eps,
                                );
                                let grad = Vec3::new(dx, dy, dz);
                                let n = if grad.length_squared() > 1e-12 {
                                    grad.normalize()
                                } else {
                                    -rel_motion.normalize_or_zero()
                                };
                                found = Some((t.clamp(0.0, 1.0), n));
                                break;
                            }
                            let step = (dist - radius) / rel_speed;
                            if !step.is_finite() || step <= 1e-7 {
                                break;
                            }
                            t += step;
                            if t > 1.0 {
                                break;
                            }
                        }
                        found
                    }
                }
            };
            if let Some((t, n)) = hit {
                if best.is_none_or(|(best_t, _)| t < best_t) {
                    best = Some((t, n));
                }
            }
        }
        best
    }

    /// 连续碰撞检测（CCD）：对开了 `ccd` 的动态刚体，把这一步的位移截在"撞上之前"。    /// 连续碰撞检测（CCD）：对开了 `ccd` 的动态刚体，把这一步的位移截在"撞上之前"。
    ///
    /// 做法（**不是**全局子步细分）：
    /// 1. 取这一步的位移 `motion = v * dt`，方向 `dir = motion / |motion|`；
    /// 2. 从刚体中心沿 `dir` 打一条**中心射线**（[`PhysicsWorld::raycast_filtered`]，
    ///    排除自己、分组不匹配的、以及传感器），最多 `|motion| + reach`，其中
    ///    `reach = Shape::support(dir)·dir` 是"中心到表面在运动方向上的距离"
    ///    （球是半径、盒是各轴投影和，凸形状上精确）；
    /// 3. 中心还能走多远就是 `hit.distance - reach`；把 `motion` 里**朝表面**的那一部分
    ///    截到这里，切向部分原样保留，再把截过的 `motion / dt` 写回速度。
    ///
    /// 这样位置积分之后，刚体正好停在接触点前（不会穿过去），而沿着墙面的滑动不受影响；
    /// 下一子步窄相会正常生成接触，冲量由求解器给（恢复系数、摩擦都照旧生效）。
    /// 代价是**每个开了 CCD 的刚体每子步一次射线查询**，与它的速度无关。
    /// 边界：中心射线 + 形状极端点旋转扫掠；极端点之间仍可能漏，"极快 + 极薄"仍可能漏——见文档。
    /// 纯 FK：把每条带马达目标的铰链子树旋到目标角、滑块子树平移到目标位移。
    ///
    /// 先从静态根沿关节图 BFS 定向（铰链两端谁离根近谁是 parent），再按树的
    /// 先序处理；旋转子树而不是只转 child，保证后续铰链的锚点/轴跟着移动。
    fn apply_kinematic_targets(&mut self) {
        let count = self.bodies.len();
        if count == 0 {
            return;
        }
        let mut raws: Vec<FkRaw> = Vec::new();
        let mut adj: Vec<Vec<(usize, usize)>> = vec![Vec::new(); count];
        for handle in self.joint_handles() {
            let (kind, a, b, anchor_a, anchor_b, axis_a, axis_b, limits, target) =
                match self.joint(handle) {
                    Some(Joint::Fixed {
                        a,
                        b,
                        local_anchor_a,
                        local_anchor_b,
                        ..
                    }) => (
                        FkKind::Fixed,
                        *a,
                        *b,
                        *local_anchor_a,
                        *local_anchor_b,
                        Vec3::ZERO,
                        Vec3::ZERO,
                        None,
                        Some(0.0),
                    ),
                    Some(Joint::Hinge {
                        a,
                        b,
                        local_anchor_a,
                        local_anchor_b,
                        local_axis_a,
                        local_axis_b,
                        limits,
                        motor: Some(motor),
                    }) => (
                        FkKind::Hinge,
                        *a,
                        *b,
                        *local_anchor_a,
                        *local_anchor_b,
                        *local_axis_a,
                        *local_axis_b,
                        *limits,
                        motor.target_angle,
                    ),
                    Some(Joint::Slider {
                        a,
                        b,
                        local_anchor_a,
                        local_anchor_b,
                        local_axis_a,
                        local_axis_b,
                        limits,
                        motor: Some(motor),
                    }) => (
                        FkKind::Slider,
                        *a,
                        *b,
                        *local_anchor_a,
                        *local_anchor_b,
                        *local_axis_a,
                        *local_axis_b,
                        *limits,
                        motor.target_angle,
                    ),
                    _ => continue,
                };
            let Some(target) = target else {
                continue;
            };
            let (Some(ia), Some(ib)) = (self.body_index(a), self.body_index(b)) else {
                continue;
            };
            if ia == ib {
                continue;
            }
            let index = raws.len();
            raws.push(FkRaw {
                kind,
                a: ia,
                anchor_a,
                anchor_b,
                axis_a,
                axis_b,
                target,
                limits,
            });
            adj[ia].push((index, ib));
            adj[ib].push((index, ia));
        }
        if raws.is_empty() {
            return;
        }
        let is_static = |world: &PhysicsWorld, i: usize| {
            world
                .bodies
                .get(i)
                .and_then(|slot| slot.body.as_ref())
                .is_some_and(|body| body.body_type == BodyType::Static)
        };
        let mut roots = Vec::new();
        for i in 0..count {
            if is_static(self, i) {
                roots.push(i);
            }
        }
        // 运动学底座由宿主指定姿态；不能因槽位排在动态连杆之后而被反向驱动。
        // 静态根仍保持原先优先级与顺序。
        roots.extend((0..count).filter(|&i| {
            self.bodies[i]
                .body
                .as_ref()
                .is_some_and(|body| body.body_type == BodyType::Kinematic)
        }));
        if roots.is_empty() {
            roots.extend((0..count).find(|&i| self.bodies[i].body.is_some()));
        }
        let mut visited = vec![false; count];
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); count];
        let mut edges: Vec<Option<FkEdge>> = vec![None; count];
        for root in roots.into_iter().chain(0..count) {
            if visited[root] || self.bodies[root].body.is_none() {
                continue;
            }
            visited[root] = true;
            let mut stack = vec![root];
            while let Some(parent) = stack.pop() {
                for &(edge_index, child) in &adj[parent] {
                    if visited[child] {
                        continue;
                    }
                    visited[child] = true;
                    let raw = raws[edge_index];
                    let (anchor_parent, anchor_child, axis_parent, axis_child, sign) =
                        if raw.a == parent {
                            (raw.anchor_a, raw.anchor_b, raw.axis_a, raw.axis_b, 1.0f32)
                        } else {
                            (raw.anchor_b, raw.anchor_a, raw.axis_b, raw.axis_a, -1.0f32)
                        };
                    edges[child] = Some(FkEdge {
                        kind: raw.kind,
                        anchor_parent,
                        anchor_child,
                        axis_parent,
                        axis_child,
                        target: raw.target,
                        limits: raw.limits,
                        sign,
                    });
                    children[parent].push(child);
                    stack.push(child);
                }
            }
        }
        for root in 0..count {
            if self.is_fk_root(root, &edges) {
                fk_apply(self, root, &children, &edges);
            }
        }
    }

    /// 该槽位是否是 FK 树的根（没有任何边把它当 child）。
    fn is_fk_root(&self, index: usize, edges: &[Option<FkEdge>]) -> bool {
        self.bodies[index].body.is_some() && edges[index].is_none()
    }

    fn apply_ccd(&mut self, dt: f32) {
        if dt <= 0.0 {
            return;
        }
        // 先收集（这一阶段只读 `self`），避免边遍历边改
        let mut updates: Vec<(usize, Vec3, Option<Vec3>)> = Vec::new();
        for (i, slot) in self.bodies.iter().enumerate() {
            let Some(b) = &slot.body else { continue };
            if !b.ccd || b.body_type != BodyType::Dynamic || b.is_sleeping() {
                continue;
            }
            let motion = b.linear_velocity * dt;
            let total = motion.length();
            // 旋转扫掠：对盒/胶囊/柱/锥/圆角盒/凸包的极端点做线段扫掠（长条/薄板最容易
            // "没有线速度、只靠转动穿过薄墙"）。角速度很小时走原来的纯平移路径。
            let spin = b.angular_velocity.length() * dt;
            let sweep_spin = matches!(
                b.shape,
                Shape::Box { .. }
                    | Shape::Capsule { .. }
                    | Shape::Cylinder { .. }
                    | Shape::Cone { .. }
                    | Shape::RoundedBox { .. }
                    | Shape::ConvexHull { .. }
            ) && spin.is_finite()
                && spin > 1e-4;
            if (!total.is_finite() || total <= 0.0) && !sweep_spin {
                continue;
            }
            // 精确凸-凸路径：和旧路径取更早的 TOI；半空间/三角网目标不在
            // 这里处理（GJK 无有限支持点），仍由旧射线/角点扫掠兜底。
            let precise_t = if b.ccd_precise {
                self.sweep_convex_bodies(i, dt)
            } else {
                None
            };
            // 球-三角网的精确 TOI 已经解析；不要把同一面墙再用中心射线/8 角射线
            // 扫 9 遍。只对球启用，凸包/盒仍保留旧路径的更早结果做兜底。
            if let (Some(t), Shape::Sphere { .. }) = (precise_t, &b.shape) {
                updates.push((i, b.linear_velocity * t, Some(b.angular_velocity * t)));
                continue;
            }
            let self_handle = BodyHandle::new(i as u32, slot.generation);
            let groups = b.groups;
            let filter = |handle: BodyHandle| -> bool {
                handle != self_handle
                    && self
                        .body(handle)
                        .is_some_and(|other| !other.sensor && other.groups.interacts(groups))
            };

            // --- 纯平移/平移+小转动：原中心射线 + 8 角射线（历史路径）---
            let mut linear_update: Option<Vec3> = None;
            if total.is_finite() && total > 0.0 {
                let dir = motion / total;
                // 中心到表面在**运动方向**上的距离（凸形状上精确：球是半径、盒是各轴投影和）。
                // 用它把"射线打到表面"换算成"中心还能走多远"。
                let reach = b.shape.support(dir).dot(dir).max(0.0);
                // 中心射线。不用保守推进扫掠（`shape_cast`）：它按"到任意方向最近物体的
                // 距离"推进，贴着地面滑行的子弹会被地面拖住，而且最近点不在运动路径上时
                // 会直接越过真正的碰撞点（实测一个旋转盒的角撞矮墙会漏）。
                let mut best: Option<(f32, Vec3)> = None;
                if let Some(hit) =
                    self.raycast_filtered(b.transform.position, dir, total + reach, filter)
                {
                    let travel = (hit.distance - reach).clamp(0.0, total);
                    best = Some((travel, hit.normal));
                }
                // 球的中心射线漏掉盒角：用球心扫掠补一条更精确的最早接触。
                if let Shape::Sphere { radius } = &b.shape {
                    if let Some((t, n)) =
                        self.sphere_sweep_bodies(i, b.transform.position, motion, radius.abs(), dt)
                    {
                        let travel = (t * total).clamp(0.0, total);
                        if best.is_none_or(|(cur, _)| travel < cur) {
                            best = Some((travel, n));
                        }
                    }
                }
                // 盒子只有角会撞上矮墙时，中心射线会从上方掠过：补 8 条角射线。
                // 盒子补 8 条角射线（球没有旋转扫掠；其他形状见下面的分支）。
                if let Shape::Box { half_extents } = &b.shape {
                    for sx in [-1.0f32, 1.0] {
                        for sy in [-1.0f32, 1.0] {
                            for sz in [-1.0f32, 1.0] {
                                let local = Vec3::new(
                                    half_extents.x * sx,
                                    half_extents.y * sy,
                                    half_extents.z * sz,
                                );
                                let offset = b.transform.rotation.mul_vec3(local);
                                let origin = b.transform.position + offset;
                                let Some(hit) =
                                    self.raycast_filtered(origin, dir, total + reach, filter)
                                else {
                                    continue;
                                };
                                let travel = (hit.distance - offset.dot(dir)).clamp(0.0, total);
                                if best.is_none_or(|(t, _)| travel < t) {
                                    best = Some((travel, hit.normal));
                                }
                            }
                        }
                    }
                }
                if let Some((travel, n)) = best {
                    // 朝表面运动的法向分量（负 = 正在靠近）
                    let mn = motion.dot(n);
                    if mn < -1e-6 {
                        // 只允许靠近 `travel` 这么多：`mn` 与 `-travel` 取"更靠外"的那个
                        let mn_clamped = mn.max(-travel);
                        let mut motion_new = motion - n * (mn - mn_clamped);
                        // 球的接触面可能是弯的：只截法向分量后，剩余切向位移仍可能
                        // 在曲面上“切进去”。再扫 2 次，按新的运动方向继续截，直到
                        // 这一步不再更早接触（盒角/圆柱顶盖擦碰都能受益）。
                        if let Shape::Sphere { radius } = &b.shape {
                            for _ in 0..2 {
                                let Some((t2, n2)) = self.sphere_sweep_bodies(
                                    i,
                                    b.transform.position,
                                    motion_new,
                                    radius.abs(),
                                    dt,
                                ) else {
                                    break;
                                };
                                let mn2 = motion_new.dot(n2);
                                if mn2 >= -1e-6 {
                                    break;
                                }
                                let travel2 = t2 * motion_new.length();
                                let clamped2 = mn2.max(-travel2);
                                let next = motion_new - n2 * (mn2 - clamped2);
                                if (next - motion_new).length_squared() <= 0.0 {
                                    // 曲面：法向分量已不足以描述“还会走多远”，
                                    // 直接把整段位移截到这次接触，宁可早停不穿透。
                                    let len = motion_new.length();
                                    if len > 1e-9 && travel2 < len {
                                        motion_new *= travel2 / len;
                                    }
                                    break;
                                }
                                motion_new = next;
                            }
                        }
                        if (motion_new - motion).length_squared() > 0.0 {
                            linear_update = Some(motion_new / dt);
                        }
                    }
                }
            }

            // --- 旋转扫掠：把形状的极端点在这一步走出的**线段**当扫掠体 ---
            // 只缩小时间（线速度与角速度同乘 t），不做反弹；下一子步窄相照常
            // 生成接触并给冲量。线段是真实圆弧的弦，偏保守（宁可早停不穿透）。
            // 盒子是 8 角；胶囊是两端点；柱/锥用 16 边环；凸包用全部顶点（最多 256）。
            let mut spin_t = 1.0f32;
            if sweep_spin {
                let angle = spin;
                let axis = b.angular_velocity.normalize_or_zero();
                let q0 = b.transform.rotation;
                let q1 = Quat::from_axis_angle(axis, angle) * q0;
                let p0 = b.transform.position;
                let p1 = p0 + motion;
                let qm = Quat::from_axis_angle(axis, angle * 0.5) * q0;
                let pm = p0 + motion * 0.5;
                let mut sweep_segment = |from: Vec3, to: Vec3, base: f32, span: f32| {
                    let seg = to - from;
                    let len = seg.length();
                    if !len.is_finite() || len <= 1e-6 {
                        return;
                    }
                    let Some(hit) = self.raycast_filtered(from, seg / len, len, filter) else {
                        return;
                    };
                    let local_t = (hit.distance / len).clamp(0.0, 1.0);
                    let t = (base + local_t * span).clamp(0.0, 1.0);
                    spin_t = spin_t.min(t);
                };
                // 半步 + 整步两段：只测端点弦会漏掉"圆弧顶点在两帧之间"的薄墙
                // （胶囊端盖的极点最典型：弦两端都在 y<1，顶点正好越过 y=1）。
                let mut sweep = |local: Vec3| {
                    let a = p0 + q0.mul_vec3(local);
                    let b = pm + qm.mul_vec3(local);
                    let c = p1 + q1.mul_vec3(local);
                    sweep_segment(a, b, 0.0, 0.5);
                    sweep_segment(b, c, 0.5, 0.5);
                };
                match &b.shape {
                    Shape::Box { half_extents } => {
                        for sx in [-1.0f32, 1.0] {
                            for sy in [-1.0f32, 1.0] {
                                for sz in [-1.0f32, 1.0] {
                                    sweep(Vec3::new(
                                        half_extents.x * sx,
                                        half_extents.y * sy,
                                        half_extents.z * sz,
                                    ));
                                }
                            }
                        }
                    }
                    Shape::Capsule {
                        half_height,
                        radius,
                    } => {
                        // 端点 + 两端"侧壁"的支撑点；旋转时侧壁也可能扫到薄墙。
                        for sy in [-1.0f32, 1.0] {
                            for sx in [-1.0f32, 0.0, 1.0] {
                                for sz in [-1.0f32, 0.0, 1.0] {
                                    if sx == 0.0 && sz == 0.0 {
                                        continue;
                                    }
                                    sweep(Vec3::new(radius * sx, half_height * sy, radius * sz));
                                }
                            }
                            sweep(Vec3::new(0.0, half_height * sy, 0.0));
                            // 端盖极点：胶囊最外沿；只扫中心线/侧壁会漏掉这一圈。
                            sweep(Vec3::new(0.0, (half_height + radius) * sy, 0.0));
                        }
                    }
                    Shape::Cylinder {
                        half_height,
                        radius,
                    } => {
                        for sy in [-1.0f32, 1.0] {
                            for k in 0..16 {
                                let a = k as f32 / 16.0 * std::f32::consts::TAU;
                                sweep(Vec3::new(
                                    radius * a.cos(),
                                    half_height * sy,
                                    radius * a.sin(),
                                ));
                            }
                        }
                    }
                    Shape::Cone {
                        half_height,
                        radius,
                    } => {
                        for k in 0..16 {
                            let a = k as f32 / 16.0 * std::f32::consts::TAU;
                            sweep(Vec3::new(radius * a.cos(), -half_height, radius * a.sin()));
                        }
                        sweep(Vec3::new(0.0, *half_height, 0.0));
                    }
                    Shape::RoundedBox {
                        half_extents,
                        radius,
                    } => {
                        for sx in [-1.0f32, 1.0] {
                            for sy in [-1.0f32, 1.0] {
                                for sz in [-1.0f32, 1.0] {
                                    sweep(Vec3::new(
                                        half_extents.x * sx,
                                        half_extents.y * sy,
                                        half_extents.z * sz,
                                    ));
                                    let _ = radius;
                                }
                            }
                        }
                    }
                    Shape::ConvexHull { points } => {
                        let stride = points.len().div_ceil(256).max(1);
                        for (i, p) in points.iter().enumerate() {
                            if i % stride == 0 {
                                sweep(*p);
                            }
                        }
                    }
                    _ => {}
                }
            }
            let mut final_update: Option<(Vec3, Option<Vec3>)> = None;
            let mut final_t = 1.0f32;
            if spin_t < 1.0 {
                final_t = spin_t;
                final_update = Some((
                    b.linear_velocity * spin_t,
                    Some(b.angular_velocity * spin_t),
                ));
            } else if let Some(v) = linear_update {
                final_update = Some((v, None));
            }
            if let Some(t) = precise_t {
                if t < final_t {
                    final_t = t;
                    final_update = Some((b.linear_velocity * t, Some(b.angular_velocity * t)));
                }
            }
            let _ = final_t;
            if let Some((v, w)) = final_update {
                updates.push((i, v, w));
            }
        }
        for (i, v, w) in updates {
            if let Some(b) = self.bodies[i].body.as_mut() {
                b.linear_velocity = v;
                if let Some(w) = w {
                    b.angular_velocity = w;
                }
            }
        }
    }

    /// 记录一条传感器事件（句柄带世代校验；槽位已经空掉的直接跳过）。
    /// 往事件总线里放一条（满了就丢 + 记账）。
    fn push_event(&mut self, event: PhysicsEvent) {
        if self.events.len() >= self.event_capacity {
            self.events_dropped += 1;
            return;
        }
        self.events.push(event);
    }

    /// 两个刚体对的事件（接触开始/结束、传感器进入/离开）。
    fn push_pair_event(&mut self, kind: PhysicsEventKind, first: u32, second: u32) {
        let (Some(sa), Some(sb)) = (
            self.bodies.get(first as usize),
            self.bodies.get(second as usize),
        ) else {
            return;
        };
        if sa.body.is_none() || sb.body.is_none() {
            return;
        }
        let event = PhysicsEvent {
            step: self.step_count,
            kind,
            a: BodyHandle::new(first, sa.generation),
            b: Some(BodyHandle::new(second, sb.generation)),
            joint: None,
            force: 0.0,
            torque: 0.0,
        };
        self.push_event(event);
    }

    fn push_sensor_event(&mut self, pair: (u32, u32), started: bool) {
        let (ia, ib) = (pair.0 as usize, pair.1 as usize);
        if let (Some(sa), Some(sb)) = (self.bodies.get(ia), self.bodies.get(ib)) {
            if sa.body.is_none() || sb.body.is_none() {
                return;
            }
            // 两个都是传感器时只报一条，`sensor` 取 a
            let sensor_is_a = sa.body.as_ref().is_some_and(|b| b.sensor);
            let (sensor, other) = if sensor_is_a {
                (
                    BodyHandle::new(pair.0, sa.generation),
                    BodyHandle::new(pair.1, sb.generation),
                )
            } else {
                (
                    BodyHandle::new(pair.1, sb.generation),
                    BodyHandle::new(pair.0, sa.generation),
                )
            };
            let kind = if started {
                PhysicsEventKind::SensorEnter
            } else {
                PhysicsEventKind::SensorExit
            };
            self.push_pair_event(kind, sensor.index, other.index);
            self.sensor_events.push(SensorEvent {
                sensor,
                other,
                started,
            });
        }
    }

    /// 是否处于「明显运动」状态（用于唤醒邻居）。
    fn is_moving(&self, index: usize) -> bool {
        match self.bodies.get(index).and_then(|s| s.body.as_ref()) {
            Some(b) => {
                b.body_type == BodyType::Dynamic
                    && !b.is_sleeping()
                    && (b.linear_velocity.length() > crate::body::SLEEP_LINEAR_TOL
                        || b.angular_velocity.length() > crate::body::SLEEP_ANGULAR_TOL)
                    || (b.body_type == BodyType::Kinematic
                        && (b.linear_velocity.length() > 1e-5
                            || b.angular_velocity.length() > 1e-5))
            }
            None => false,
        }
    }

    /// 是否睡眠。
    fn is_sleeping(&self, index: usize) -> bool {
        self.bodies
            .get(index)
            .and_then(|s| s.body.as_ref())
            .map(|b| b.is_sleeping())
            .unwrap_or(false)
    }

    /// 把求解器里的冲量汇总到公开接触对上。
    ///
    /// `prepare` 按流形顺序生成接触，而 `self.contacts` 也按同一顺序每个流形一条，
    /// 因此这里只需一次线性扫描（同一形状对的接触是连续的），**不需要**每步建映射表。
    fn update_contact_impulses(&mut self) {
        if self.contacts.is_empty() {
            return;
        }
        let contacts = &self.contact_solver.contacts;
        let mut cursor = 0usize;
        for pair in self.contacts.iter_mut() {
            let (want_a, want_b) = (pair.a.index, pair.b.index);
            let mut sum = 0.0f32;
            while let Some(c) = contacts.get(cursor) {
                if c.a as u32 == want_a && c.b as u32 == want_b {
                    sum += c.normal_impulse;
                    cursor += 1;
                } else {
                    break;
                }
            }
            pair.impulse = sum;
        }
    }
}

/// 一对刚体的所有部件两两求交，把接触点合成**一个**流形。
///
/// 单形状 × 单形状（绝大多数场景）直接走老路径 `collide_shapes`，一个点都不多算——
/// 这是"复合形状不改变现有场景结果"的关键（不是"应该一样"，是同一个分支）。
///
/// 多个部件命中时把点合起来，最多 [`MAX_COMPOUND_POINTS`] 个：多了对求解器没有好处，
/// 还会让热启动缓存抖动。超限时按**穿透深度**取最深的那几个（稳定排序，确定性）。
/// 接触特征 ID：把接触点换算到两端局部坐标，按 0.5 mm 量化后哈希。
///
/// 0 保留为"未知"。量化尺度是**取舍**：太大（比如 1 cm）会让同一物体上相邻的两个
/// 特征点撞成同一个 ID；太小（比如 1 µm）会让浮点噪声把同一个点算成两个 ID。
/// 0.5 mm 对 0.1~10 m 尺度的场景都合适（写进文档，用户不必猜）。
fn feature_id_of(a: &RigidBody, b: &RigidBody, world: Vec3) -> u32 {
    crate::contact::feature_id_from_local(
        a.transform.inverse_transform_point(world),
        b.transform.inverse_transform_point(world),
    )
}

fn collide_bodies(a: &RigidBody, b: &RigidBody, margin: f32) -> Option<ContactManifold> {
    let mut m = collide_bodies_raw(a, b, margin)?;
    if margin > 0.0 {
        // 膨胀窄相把"分离 margin 以内"也算成接触，穿透量要减回去：
        // 正值 = 重叠，负值 = 还差多少贴上（求解器据此决定允许多快逼近）。
        for p in m.points.iter_mut() {
            p.penetration -= margin;
        }
    }
    for p in m.points.iter_mut() {
        p.feature_id = feature_id_of(a, b, p.position);
    }
    Some(m)
}

fn collide_bodies_raw(a: &RigidBody, b: &RigidBody, margin: f32) -> Option<ContactManifold> {
    if a.extra_parts.is_empty() && b.extra_parts.is_empty() {
        let sa = a.shape.inflated(margin);
        let sb = b.shape.inflated(margin);
        return narrowphase::collide_shapes(&sa, &a.transform, &sb, &b.transform);
    }
    let mut points: SmallVec<ContactPoint, { ContactManifold::MAX_POINTS }> = SmallVec::new();
    for (sa, ta) in a.parts() {
        let wa = a.part_world_transform(&ta);
        for (sb, tb) in b.parts() {
            let wb = b.part_world_transform(&tb);
            // 余量对复合刚体同样生效：逐部件独立膨胀（每个部件都是凸体，"这个部件长胖
            // margin"就是它自己的 Minkowski 和，不需要重算刚体的局部质心或聚合 AABB ——
            // 那些只跟**装配**有关，跟窄相用哪个几何无关）。`collide_bodies` 之后统一
            // 把 penetration 减回 margin。
            let hit = if margin > 0.0 {
                narrowphase::collide_shapes(&sa.inflated(margin), &wa, &sb.inflated(margin), &wb)
            } else {
                narrowphase::collide_shapes(sa, &wa, sb, &wb)
            };
            if let Some(m) = hit {
                points.extend_from_slice(&m.points);
            }
        }
    }
    if points.is_empty() {
        return None;
    }
    if points.len() > MAX_COMPOUND_POINTS {
        let mut order: Vec<usize> = (0..points.len()).collect();
        // 稳定排序：深度相同就按下标（部件顺序）——保证确定性
        order.sort_by(|&i, &j| {
            points[j]
                .penetration
                .partial_cmp(&points[i].penetration)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then(i.cmp(&j))
        });
        order.truncate(MAX_COMPOUND_POINTS);
        order.sort_unstable();
        let kept: SmallVec<ContactPoint, { ContactManifold::MAX_POINTS }> =
            order.iter().map(|&i| points[i]).collect();
        return Some(ContactManifold { points: kept });
    }
    Some(ContactManifold { points })
}

/// 一个形状 vs 一个刚体（逐部件）求交，接触点合成一个流形。
///
/// 角色控制器用它做胶囊 vs 世界的接触查询；单形状刚体走老路径。
pub(crate) fn collide_shape_with_body(
    shape: &Shape,
    t: &Transform,
    body: &RigidBody,
) -> Option<ContactManifold> {
    if body.extra_parts.is_empty() {
        return narrowphase::collide_shapes(shape, t, &body.shape, &body.transform);
    }
    let mut points: SmallVec<ContactPoint, { ContactManifold::MAX_POINTS }> = SmallVec::new();
    for (part, local) in body.parts() {
        let world = body.part_world_transform(&local);
        if let Some(m) = narrowphase::collide_shapes(shape, t, part, &world) {
            points.extend_from_slice(&m.points);
            if points.len() >= MAX_COMPOUND_POINTS {
                break;
            }
        }
    }
    if points.is_empty() {
        return None;
    }
    points.truncate(MAX_COMPOUND_POINTS);
    Some(ContactManifold { points })
}

/// 物理世界的运行时快照。
///
/// 保存的是**状态**而不是场景配置：刚体槽位/世代、位置/旋转/速度、睡眠计时、
/// 关节槽位与热启动冲量、接触热启动缓存、世界累加器、步数、事件队列与上一子步的
/// 接触/睡眠差分表。恢复后查询用的句柄仍然有效（槽位布局原样还原）。
///
/// 不保存求解器临时缓冲（`scratch_*`、`manifolds`、`pairs`）——它们在下一步开头重建；
/// 需要跨步保留的热启动数据在 `cache` 与关节 `impulses` 里。
#[derive(Clone, Debug)]
pub struct PhysicsSnapshot {
    bodies: Vec<BodySlot>,
    free: Vec<u32>,
    joints: Vec<JointSlot>,
    joint_free: Vec<u32>,
    gravity: Vec3,
    collisions_enabled: bool,
    waters: Vec<WaterVolume>,
    fluids: Vec<Fluid>,
    fixed_dt: f32,
    accumulator: f32,
    step_count: u64,
    cache: ContactCache,
    sensor_pairs_prev: Vec<(u32, u32)>,
    touch_pairs_prev: Vec<(u32, u32)>,
    sleeping_prev: Vec<bool>,
    events: Vec<PhysicsEvent>,
    events_dropped: u64,
    broken_joints: Vec<JointHandle>,
}

impl PhysicsWorld {
    /// 生成运行时快照。O(刚体数 + 关节数 + 接触缓存 + 粒子数)。
    pub fn snapshot(&self) -> PhysicsSnapshot {
        PhysicsSnapshot {
            bodies: self.bodies.clone(),
            free: self.free.clone(),
            joints: self.joints.clone(),
            joint_free: self.joint_free.clone(),
            gravity: self.gravity,
            collisions_enabled: self.collisions_enabled,
            waters: self.waters.clone(),
            fluids: self.fluids.clone(),
            fixed_dt: self.fixed_dt,
            accumulator: self.accumulator,
            step_count: self.step_count,
            cache: self.cache.clone(),
            sensor_pairs_prev: self.sensor_pairs_prev.clone(),
            touch_pairs_prev: self.touch_pairs_prev.clone(),
            sleeping_prev: self.sleeping_prev.clone(),
            events: self.events.clone(),
            events_dropped: self.events_dropped,
            broken_joints: self.broken_joints.clone(),
        }
    }

    /// 恢复快照。世界内容、刚体/关节状态、热启动缓存与步数都回到快照点。
    ///
    /// 快照里的槽位布局会整体替换当前世界；快照创建时的 `BodyHandle`/`JointHandle`
    /// 恢复后仍然有效。当前世界的临时查询缓存会被丢弃并让 `version` 递增。
    pub fn restore(&mut self, snapshot: PhysicsSnapshot) {
        self.bodies = snapshot.bodies;
        self.free = snapshot.free;
        self.joints = snapshot.joints;
        self.joint_free = snapshot.joint_free;
        self.gravity = snapshot.gravity;
        self.collisions_enabled = snapshot.collisions_enabled;
        self.waters = snapshot.waters;
        self.fluids = snapshot.fluids;
        // 快照恢复后渲染端的表面网格可能比快照新/旧：强制下一帧重建。
        for f in &mut self.fluids {
            f.bump_revision();
        }
        self.fixed_dt = snapshot.fixed_dt;
        self.accumulator = snapshot.accumulator;
        self.step_count = snapshot.step_count;
        self.cache = snapshot.cache;
        self.sensor_pairs_prev = snapshot.sensor_pairs_prev;
        self.touch_pairs_prev = snapshot.touch_pairs_prev;
        self.sleeping_prev = snapshot.sleeping_prev;
        self.events = snapshot.events;
        self.events_dropped = snapshot.events_dropped;
        self.broken_joints = snapshot.broken_joints;

        // 下一步会重建这些；留着会让查询/绘制短暂看到快照之后的世界。
        self.sensor_pairs.clear();
        self.sensor_events.clear();
        self.contacts.clear();
        self.manifolds.clear();
        self.pairs.clear();
        self.proxies.clear();
        self.wake_list.clear();
        self.islands = Islands::default();
        self.contact_solver = ContactSolver::default();
        self.joint_solver = JointSolver::default();
        self.scratch_views.clear();
        self.scratch_materials.clear();
        self.scratch_pre_velocities.clear();
        self.scratch_actuated.clear();
        self.scratch_joint_entries.clear();
        self.scratch_joint_impulses.clear();
        self.parallel_used = false;

        // 查询索引靠版本号判断是否过期：恢复等于换了一份世界，必须失效。
        self.version = self.version.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::BodyBuilder;

    fn sphere(r: f32) -> Shape {
        Shape::Sphere { radius: r }
    }

    fn dense_stack() -> PhysicsWorld {
        let mut w = PhysicsWorld::new();
        w.add_body(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build());
        for i in 0..400 {
            let x = (i % 20) as f32 * 0.9 - 9.0;
            let z = (i / 20) as f32 * 0.9 - 9.0;
            w.add_body(
                BodyBuilder::dynamic(Shape::Box {
                    half_extents: Vec3::splat(0.4),
                })
                .position(Vec3::new(x, 0.45, z))
                .build(),
            );
        }
        w
    }

    fn body_bits(world: &PhysicsWorld) -> Vec<u32> {
        let mut out = Vec::new();
        for (_, b) in world.bodies() {
            for v in [
                b.position().x,
                b.position().y,
                b.position().z,
                b.rotation().x,
                b.rotation().y,
                b.rotation().z,
                b.rotation().w,
                b.linear_velocity.x,
                b.linear_velocity.y,
                b.linear_velocity.z,
                b.angular_velocity.x,
                b.angular_velocity.y,
                b.angular_velocity.z,
            ] {
                out.push(v.to_bits());
            }
            out.push(b.is_sleeping() as u32);
        }
        out
    }

    #[test]
    fn sphere_sweep_accounts_for_a_moving_target() {
        // 球沿 +X 飞，目标盒子是运动学体、沿 +Y 穿过球心航线。只看目标的
        // 起始位置，球会从盒子下方 4 m 处飞过；但这一步盒子会向上走 4 m，
        // 球面实际会撞上。CCD 必须用相对速度扫掠。
        let mut w = PhysicsWorld::new();
        w.set_gravity(Vec3::ZERO);
        let sphere = w.add_builder(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
                .position(Vec3::new(-4.0, 0.0, 0.0))
                .linear_velocity(Vec3::new(4.0, 0.0, 0.0)),
        );
        let s = sphere.index as usize;
        w.add_builder(
            BodyBuilder::kinematic(Shape::Box {
                half_extents: Vec3::splat(0.5),
            })
            .position(Vec3::new(0.0, -4.0, 0.0))
            .linear_velocity(Vec3::new(0.0, 240.0, 0.0)),
        );
        let hit =
            w.sphere_sweep_bodies(s, Vec3::new(-4.0, 0.0, 0.0), Vec3::X * 4.0, 0.1, 1.0 / 60.0);
        assert!(
            hit.is_some(),
            "运动学目标穿过球心航线时，扫掠必须看到相对运动"
        );
    }

    #[test]
    fn aerated_fluid_reduces_buoyancy_on_a_body() {
        use crate::fluid::{Bubble, FluidDesc};

        fn buoyant_force(with_bubble: bool) -> f32 {
            let mut w = PhysicsWorld::new();
            w.set_gravity(Vec3::new(0.0, -9.81, 0.0));
            let body = w.add_builder(
                BodyBuilder::dynamic(Shape::Sphere { radius: 0.2 })
                    .position(Vec3::new(0.0, 2.0, 0.0)),
            );
            w.add_fluid(FluidDesc {
                center: Vec3::new(0.0, 2.0, 0.0),
                half_extents: Vec3::splat(0.4),
                spacing: 0.1,
                radius: 0.05,
                max_particles: 1000,
                ..FluidDesc::default()
            })
            .expect("流体参数合法");
            if with_bubble {
                w.fluids_mut()[0].add_bubble(Bubble {
                    position: Vec3::new(0.33, 2.0, 0.0),
                    velocity: Vec3::ZERO,
                    radius: 0.15,
                    rest_radius: 0.15,
                    age: 0.0,
                    life: 10.0,
                });
            }
            w.step_fluids(1.0 / 60.0);
            let b = w.bodies[body.index as usize]
                .body
                .as_ref()
                .expect("刚体存在");
            b.force.y
        }

        let without = buoyant_force(false);
        let with = buoyant_force(true);
        assert!(without > 0.0, "未曝气时浮力应向上：force.y = {without}");
        assert!(
            with < without * 0.95,
            "气泡占据体积应降低局部浮力：无气泡 {without}，有气泡 {with}"
        );
    }

    #[test]
    fn sphere_sweep_sees_a_rotating_target_corner() {
        // 球沿 +X 飞；薄盒水平放在 y=2，静止姿态从球上方掠过。盒在一步内绕 Z
        // 转 90°，下角扫到 y=0，正好扫进球的航线。只按起始姿态扫会漏。
        let mut w = PhysicsWorld::new();
        w.set_gravity(Vec3::ZERO);
        let sphere = w.add_builder(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
                .position(Vec3::new(-4.0, 0.0, 0.0))
                .linear_velocity(Vec3::new(4.0, 0.0, 0.0)),
        );
        let s = sphere.index as usize;
        w.add_builder(
            BodyBuilder::kinematic(Shape::Box {
                half_extents: Vec3::new(2.0, 0.1, 0.1),
            })
            .position(Vec3::new(0.0, 2.0, 0.0))
            .angular_velocity(Vec3::new(0.0, 0.0, std::f32::consts::FRAC_PI_2 * 60.0)),
        );
        let hit =
            w.sphere_sweep_bodies(s, Vec3::new(-4.0, 0.0, 0.0), Vec3::X * 4.0, 0.1, 1.0 / 60.0);
        assert!(hit.is_some(), "旋转盒的下角扫过球心航线时必须被截住");
    }

    #[test]
    fn sphere_sweep_sees_a_rotating_capsule() {
        // 长胶囊水平放在 y=2，静止姿态从球上方掠过；一步转 90° 后下端盖扫到
        // y=0。旋转盒已覆盖，但胶囊走的是另一条距离场分支，必须同样看到。
        let mut w = PhysicsWorld::new();
        w.set_gravity(Vec3::ZERO);
        let sphere = w.add_builder(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
                .position(Vec3::new(-4.0, 0.0, 0.0))
                .linear_velocity(Vec3::new(4.0, 0.0, 0.0)),
        );
        let s = sphere.index as usize;
        w.add_builder(
            BodyBuilder::kinematic(Shape::Capsule {
                half_height: 2.0,
                radius: 0.1,
            })
            .position(Vec3::new(0.0, 2.0, 0.0))
            .rotation(Quat::from_axis_angle(Vec3::Z, std::f32::consts::FRAC_PI_2))
            .angular_velocity(Vec3::new(0.0, 0.0, std::f32::consts::FRAC_PI_2 * 60.0)),
        );
        let hit =
            w.sphere_sweep_bodies(s, Vec3::new(-4.0, 0.0, 0.0), Vec3::X * 4.0, 0.1, 1.0 / 60.0);
        assert!(hit.is_some(), "旋转胶囊的下端盖扫过球心航线时必须被截住");
    }

    #[test]
    fn parallel_narrowphase_is_bit_identical_to_serial() {
        let mut serial = dense_stack();
        serial.config.parallel_narrowphase = false;
        let mut parallel = dense_stack();
        parallel.config.parallel_narrowphase = true;
        for _ in 0..10 {
            serial.step(1.0 / 60.0);
            parallel.step(1.0 / 60.0);
        }
        assert!(
            parallel.uses_parallel_narrowphase(),
            "候选对足够多时应真的走并行窄相"
        );
        assert_eq!(
            body_bits(&serial),
            body_bits(&parallel),
            "并行窄相必须与串行逐位相同"
        );
    }

    #[test]
    fn add_remove_bodies_invalidates_handles() {
        let mut w = PhysicsWorld::new();
        let h = w.add_body(BodyBuilder::dynamic(sphere(0.5)).build());
        assert!(w.body(h).is_some());
        assert_eq!(w.body_count(), 1);
        assert!(w.remove_body(h).is_some());
        assert!(w.body(h).is_none());
        assert_eq!(w.body_count(), 0);
        // 复用槽位后世代递增，旧句柄依然失效。
        let h2 = w.add_body(BodyBuilder::dynamic(sphere(0.5)).build());
        assert_eq!(h2.index, h.index);
        assert_ne!(h2.generation, h.generation);
        assert!(w.body(h).is_none());
        assert!(w.body(h2).is_some());
        assert!(w.remove_body(h).is_none());
    }

    #[test]
    fn gravity_default_and_settable() {
        let mut w = PhysicsWorld::new();
        assert_eq!(w.gravity(), crate::GRAVITY);
        w.set_gravity(vec3(0.0, -1.0, 0.0));
        assert_eq!(w.gravity(), vec3(0.0, -1.0, 0.0));
        w.set_gravity(Vec3::splat(f32::NAN));
        assert_eq!(w.gravity(), vec3(0.0, -1.0, 0.0), "NaN 应被忽略");
    }

    #[test]
    fn collisions_can_be_disabled_for_drive_playback() {
        let mut w = PhysicsWorld::new();
        w.add_body(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build());
        let ball = w.add_body(
            BodyBuilder::dynamic(sphere(0.5))
                .position(vec3(0.0, 0.4, 0.0))
                .build(),
        );
        w.set_collisions_enabled(false);
        assert!(!w.collisions_enabled());
        w.step(1.0 / 60.0);
        assert!(w.contacts().is_empty(), "drive 模式不许生成接触");
        assert!(
            w.body(ball).unwrap().position().y < 0.4,
            "无接触时球应继续下落"
        );
        // 重新打开后接触恢复；这里只验证状态开关，不要求它把已经穿过去的球推回来。
        w.set_collisions_enabled(true);
        assert!(w.collisions_enabled());
    }

    #[test]
    fn fixed_dt_is_clamped() {
        let mut w = PhysicsWorld::new();
        assert!((w.fixed_dt() - DEFAULT_FIXED_DT).abs() < 1e-9);
        w.set_fixed_dt(0.0);
        assert!((w.fixed_dt() - DEFAULT_FIXED_DT).abs() < 1e-9);
        w.set_fixed_dt(1.0);
        assert!(w.fixed_dt() <= 0.1 + 1e-6);
        w.set_fixed_dt(f32::NAN);
        assert!(w.fixed_dt() > 0.0);
    }

    #[test]
    fn step_rejects_bad_dt() {
        let mut w = PhysicsWorld::new();
        assert_eq!(w.step_with_accumulator(0.0), 0);
        assert_eq!(w.step_with_accumulator(-1.0), 0);
        assert_eq!(w.step_with_accumulator(f32::NAN), 0);
        assert_eq!(w.step_with_accumulator(1.0 / 60.0), 1);
        assert_eq!(w.step_count, 1);
    }

    #[test]
    fn joints_add_and_remove() {
        let mut w = PhysicsWorld::new();
        let a = w.add_body(BodyBuilder::r#static(sphere(0.5)).build());
        let b = w.add_body(BodyBuilder::dynamic(sphere(0.5)).build());
        let j = w.add_joint(Joint::Ball {
            a,
            b,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: Vec3::ZERO,
            cone_limit: None,
        });
        assert_eq!(w.joint_count(), 1);
        assert!(w.joint(j).is_some());
        assert!(w.remove_joint(j));
        assert!(!w.remove_joint(j), "重复删除应失败");
        assert_eq!(w.joint_count(), 0);
    }

    #[test]
    fn joint_visuals_report_world_anchors_and_axis() {
        let mut w = PhysicsWorld::new();
        let a = w.add_body(
            BodyBuilder::r#static(sphere(0.5))
                .position(Vec3::new(1.0, 0.0, 0.0))
                .build(),
        );
        let b = w.add_body(
            BodyBuilder::dynamic(sphere(0.5))
                .position(Vec3::new(3.0, 0.0, 0.0))
                .build(),
        );
        w.add_joint(Joint::Hinge {
            a,
            b,
            local_anchor_a: Vec3::new(0.5, 0.0, 0.0),
            local_anchor_b: Vec3::new(-0.5, 0.0, 0.0),
            local_axis_a: Vec3::Z,
            local_axis_b: Vec3::Z,
            limits: None,
            motor: None,
        });
        let visuals = w.joint_visuals();
        assert_eq!(visuals.len(), 1);
        assert!((visuals[0].a - Vec3::new(1.5, 0.0, 0.0)).length() < 1e-6);
        assert!((visuals[0].b - Vec3::new(2.5, 0.0, 0.0)).length() < 1e-6);
        assert!((visuals[0].axis.unwrap_or(Vec3::ZERO) - Vec3::Z).length() < 1e-6);

        // 删除刚体后不返回悬空关节。
        w.remove_body(b);
        assert!(w.joint_visuals().is_empty());
    }

    #[test]
    fn interpolated_transform_is_between_the_last_two_fixed_steps() {
        let mut w = PhysicsWorld::new();
        w.set_fixed_dt(0.1);
        let handle = w.add_builder(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
                .position(Vec3::new(0.0, 10.0, 0.0))
                .linear_velocity(Vec3::new(0.0, 10.0, 0.0))
                .gravity_scale(0.0),
        );
        w.step(0.1);
        assert!((w.body(handle).unwrap().transform.position.y - 11.0).abs() < 1e-5);
        let start = w.body_interpolated_transform(handle, 0.0).unwrap();
        let mid = w.body_interpolated_transform(handle, 0.5).unwrap();
        let end = w.body_interpolated_transform(handle, 1.0).unwrap();
        assert!((start.position.y - 10.0).abs() < 1e-5, "{start:?}");
        assert!((mid.position.y - 10.5).abs() < 1e-5, "{mid:?}");
        assert!((end.position.y - 11.0).abs() < 1e-5, "{end:?}");
    }

    #[test]
    fn snapshot_restore_round_trips_body_joint_and_step_state() {
        let mut w = PhysicsWorld::new();
        w.set_gravity(Vec3::new(0.0, -9.81, 0.0));
        let ground = w.add_body(
            BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::splat(2.0),
            })
            .position(Vec3::new(0.0, -2.5, 0.0))
            .build(),
        );
        let a = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.25),
            })
            .position(Vec3::new(0.0, 0.8, 0.0))
            .build(),
        );
        let b = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.25),
            })
            .position(Vec3::new(1.0, 0.8, 0.0))
            .build(),
        );
        let joint = w.add_joint(Joint::Hinge {
            a,
            b,
            local_anchor_a: Vec3::new(0.5, 0.0, 0.0),
            local_anchor_b: Vec3::new(-0.5, 0.0, 0.0),
            local_axis_a: Vec3::Z,
            local_axis_b: Vec3::Z,
            limits: Some((-0.5, 0.5)),
            motor: Some(crate::joints::HingeMotor {
                target_velocity: 0.0,
                max_impulse: 0.2,
                target_angle: Some(0.25),
                stiffness: 8.0,
                damping: 1.0,
                kp: 0.0,
                kd: 0.0,
                max_torque: 0.0,
            }),
        });

        for _ in 0..120 {
            w.step(1.0 / 60.0);
        }
        let snapshot = w.snapshot();
        let step_count = w.step_count;
        let ground_body = w.body(ground).cloned().expect("ground");
        let a_body = w.body(a).cloned().expect("a");
        let b_body = w.body(b).cloned().expect("b");
        let target = w.joint_target(joint);
        assert!(a_body.position().y < 0.8, "120 步后应当已经下落");

        for _ in 0..45 {
            w.step(1.0 / 60.0);
        }
        assert_ne!(w.body(a), Some(&a_body), "快照后世界必须真的变了");

        w.restore(snapshot.clone());
        assert_eq!(w.step_count, step_count);
        assert_eq!(w.body(ground), Some(&ground_body));
        assert_eq!(w.body(a), Some(&a_body), "刚体位姿/速度/睡眠必须逐位还原");
        assert_eq!(w.body(b), Some(&b_body));
        assert_eq!(w.joint_target(joint), target);
        assert_eq!(w.joint_count(), 1);

        // 从同一快照再跑 30 步，必须与第一次恢复后的 30 步逐位一致
        // （热启动缓存也属于快照；只还原位姿会让第二步开始分叉）。
        for _ in 0..30 {
            w.step(1.0 / 60.0);
        }
        let first_a = w.body(a).cloned().expect("a after first continuation");
        let first_b = w.body(b).cloned().expect("b after first continuation");
        let first_steps = w.step_count;
        w.restore(snapshot);
        for _ in 0..30 {
            w.step(1.0 / 60.0);
        }
        assert_eq!(w.body(a), Some(&first_a), "恢复后的轨迹必须逐位可重复");
        assert_eq!(w.body(b), Some(&first_b));
        assert_eq!(w.step_count, first_steps);
    }
    #[test]
    fn tri_grid_candidates_cover_every_overlapping_triangle() {
        let mut bounds = Vec::new();
        for i in 0..400 {
            let x = (i % 20) as f32 * 0.37 - 3.5;
            let y = ((i / 20) % 20) as f32 * 0.29 - 2.8;
            let z = (i / 400) as f32 * 0.5;
            let half = Vec3::new(
                0.03 + (i % 7) as f32 * 0.02,
                0.02 + (i % 5) as f32 * 0.03,
                0.04 + (i % 3) as f32 * 0.01,
            );
            let c = Vec3::new(x, y, z);
            bounds.push(Aabb::new(c - half, c + half));
        }
        let grid = TriGrid::build(&bounds).expect("400 个三角形应建立网格");
        for q in [
            Aabb::new(Vec3::splat(-0.2), Vec3::splat(0.2)),
            Aabb::new(Vec3::new(-3.0, -2.0, -0.1), Vec3::new(-2.0, -1.0, 0.2)),
            Aabb::new(Vec3::new(2.0, 1.5, -1.0), Vec3::new(3.5, 2.5, 1.0)),
        ] {
            let mut out = Vec::new();
            grid.candidates(&q, &mut out);
            out.sort_unstable();
            out.dedup();
            for (k, b) in bounds.iter().enumerate() {
                if b.intersects(&q) {
                    assert!(
                        out.binary_search(&(k as u32)).is_ok(),
                        "网格漏掉与查询 AABB 相交的三角形 {k}: {b:?} vs {q:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn kinematic_fk_uses_kinematic_roots_independent_of_insertion_order() {
        for root_first in [false, true] {
            let mut w = PhysicsWorld::new();
            w.set_gravity(Vec3::ZERO);
            w.set_collisions_enabled(false);
            w.set_kinematic_fk(true);
            let base = BodyBuilder::kinematic(Shape::Sphere { radius: 0.1 }).build();
            let link = BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
                .position(Vec3::X)
                .build();
            let (root, child) = if root_first {
                let root = w.add_body(base);
                (root, w.add_body(link))
            } else {
                let child = w.add_body(link);
                (w.add_body(base), child)
            };
            w.add_joint(Joint::Hinge {
                a: root,
                b: child,
                local_anchor_a: Vec3::ZERO,
                local_anchor_b: -Vec3::X,
                local_axis_a: Vec3::Y,
                local_axis_b: Vec3::Y,
                limits: None,
                motor: Some(crate::joints::HingeMotor::servo(
                    std::f32::consts::FRAC_PI_2,
                    10.0,
                )),
            });
            let before = w.body(root).unwrap().transform;
            w.step_substeps(1.0 / 60.0, 1);
            assert_eq!(
                w.body(root).unwrap().transform,
                before,
                "FK must preserve the prescribed base pose: root_first={root_first}"
            );
            assert!((w.body(child).unwrap().position() - Vec3::NEG_Z).length() < 1e-4);
        }
    }

    #[test]
    fn kinematic_fk_carries_fixed_attachments() {
        for slider in [false, true] {
            for reverse in [false, true] {
                let mut w = PhysicsWorld::new();
                w.set_gravity(Vec3::ZERO);
                w.set_collisions_enabled(false);
                w.set_kinematic_fk(true);
                let root = w.add_body(BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }).build());
                let child = w.add_body(
                    BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
                        .position(Vec3::X)
                        .build(),
                );
                let payload = w.add_body(
                    BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
                        .position(Vec3::X * 2.0)
                        .build(),
                );
                let motor = Some(crate::joints::HingeMotor::servo(
                    if slider {
                        2.0
                    } else {
                        std::f32::consts::FRAC_PI_2
                    },
                    10.0,
                ));
                w.add_joint(if slider {
                    Joint::Slider {
                        a: root,
                        b: child,
                        local_anchor_a: Vec3::ZERO,
                        local_anchor_b: Vec3::ZERO,
                        local_axis_a: Vec3::X,
                        local_axis_b: Vec3::X,
                        limits: None,
                        motor,
                    }
                } else {
                    Joint::Hinge {
                        a: root,
                        b: child,
                        local_anchor_a: Vec3::ZERO,
                        local_anchor_b: -Vec3::X,
                        local_axis_a: Vec3::Y,
                        local_axis_b: Vec3::Y,
                        limits: None,
                        motor,
                    }
                });
                w.add_joint(Joint::Fixed {
                    a: if reverse { payload } else { child },
                    b: if reverse { child } else { payload },
                    local_anchor_a: if reverse { Vec3::ZERO } else { Vec3::X },
                    local_anchor_b: if reverse { Vec3::X } else { Vec3::ZERO },
                    local_frame_a: Quat::IDENTITY,
                    local_frame_b: Quat::IDENTITY,
                });
                let q = if slider {
                    Quat::IDENTITY
                } else {
                    Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)
                };
                let expected = if slider {
                    Vec3::X * 3.0
                } else {
                    q.mul_vec3(Vec3::X * 2.0)
                };
                w.apply_kinematic_targets();
                for integrated in [false, true] {
                    if integrated {
                        w.step_substeps(1.0 / 60.0, 1);
                    }
                    let body = w.body(payload).unwrap();
                    assert!(
                        (body.position() - expected).length() < 1e-4,
                        "slider={slider} reverse={reverse} integrated={integrated}: {:?}",
                        body.position()
                    );
                    assert!(
                        (body.rotation().mul_vec3(Vec3::X) - q.mul_vec3(Vec3::X)).length() < 1e-4
                    );
                    let anchor = w.body(child).unwrap().transform.transform_point(Vec3::X);
                    assert!((anchor - body.position()).length() < 1e-4);
                    assert!(body.linear_velocity.length() < 1e-4);
                    assert!(body.angular_velocity.length() < 1e-4);
                }
            }
        }
    }

    #[test]
    fn kinematic_fk_waits_for_an_accumulated_physics_step() {
        let mut w = PhysicsWorld::new();
        w.set_gravity(Vec3::ZERO);
        w.set_collisions_enabled(false);
        w.set_kinematic_fk(true);
        let root = w.add_body(BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }).build());
        let child = w.add_body(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
                .position(Vec3::X)
                .build(),
        );
        w.add_joint(Joint::Hinge {
            a: root,
            b: child,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: -Vec3::X,
            local_axis_a: Vec3::Y,
            local_axis_b: Vec3::Y,
            limits: None,
            motor: Some(crate::joints::HingeMotor::servo(
                std::f32::consts::FRAC_PI_2,
                10.0,
            )),
        });
        let before = w.body(child).unwrap().transform;
        let quarter = w.fixed_dt * 0.25;
        for dt in [
            0.0,
            -1.0,
            f32::NAN,
            f32::INFINITY,
            quarter,
            quarter,
            quarter,
        ] {
            assert_eq!(w.step_with_accumulator(dt), 0);
            assert_eq!(
                w.body(child).unwrap().transform,
                before,
                "zero substeps must not apply FK: dt={dt}"
            );
        }
        assert_eq!(w.step_with_accumulator(quarter), 1);
        let expected = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2).mul_vec3(Vec3::X);
        assert!((w.body(child).unwrap().position() - expected).length() < 2e-3);
    }

    #[test]
    fn kinematic_fk_rotates_the_hinge_subtree_to_the_target_immediately() {
        let mut w = PhysicsWorld::new();
        w.set_gravity(Vec3::ZERO);
        w.set_collisions_enabled(false);
        w.set_kinematic_fk(true);
        let root = w.add_body(
            BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::splat(0.1),
            })
            .position(Vec3::ZERO)
            .build(),
        );
        let child = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.1),
            })
            .position(Vec3::X)
            .build(),
        );
        let motor = crate::joints::HingeMotor {
            target_velocity: 0.0,
            max_impulse: 10.0,
            target_angle: Some(std::f32::consts::FRAC_PI_2),
            stiffness: 8.0,
            damping: 1.0,
            kp: 0.0,
            kd: 0.0,
            max_torque: 0.0,
        };
        let joint = w.add_joint(Joint::Hinge {
            a: root,
            b: child,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: -Vec3::X,
            local_axis_a: Vec3::Y,
            local_axis_b: Vec3::Y,
            limits: None,
            motor: Some(motor),
        });
        w.step_substeps(1.0 / 60.0, 1);
        let expected =
            Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2).mul_vec3(Vec3::X);
        let got = w.body(child).expect("child").position();
        assert!(
            (got - expected).length() < 2e-3,
            "纯 FK 一步应直接到 +90°：{got:?} vs {expected:?}"
        );
        let angle = w
            .joint_telemetry(joint, 1.0 / 60.0)
            .expect("joint telemetry")
            .coordinate;
        assert!(
            (angle - std::f32::consts::FRAC_PI_2).abs() < 2e-3,
            "关节角应直接到 +90°，实际 {angle}"
        );
    }
    #[test]
    fn kinematic_fk_moves_the_whole_descendant_subtree() {
        let mut w = PhysicsWorld::new();
        w.set_gravity(Vec3::ZERO);
        w.set_collisions_enabled(false);
        w.set_kinematic_fk(true);
        let root = w.add_body(
            BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::splat(0.1),
            })
            .position(Vec3::ZERO)
            .build(),
        );
        let child = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.1),
            })
            .position(Vec3::X)
            .build(),
        );
        let grandchild = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.1),
            })
            .position(Vec3::X * 2.0)
            .build(),
        );
        let motor = crate::joints::HingeMotor {
            target_velocity: 0.0,
            max_impulse: 10.0,
            target_angle: Some(std::f32::consts::FRAC_PI_2),
            stiffness: 8.0,
            damping: 1.0,
            kp: 0.0,
            kd: 0.0,
            max_torque: 0.0,
        };
        w.add_joint(Joint::Hinge {
            a: root,
            b: child,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: -Vec3::X,
            local_axis_a: Vec3::Y,
            local_axis_b: Vec3::Y,
            limits: None,
            motor: Some(motor),
        });
        let hold = crate::joints::HingeMotor {
            target_angle: Some(0.0),
            ..motor
        };
        w.add_joint(Joint::Hinge {
            a: child,
            b: grandchild,
            local_anchor_a: Vec3::X,
            local_anchor_b: Vec3::ZERO,
            local_axis_a: Vec3::Y,
            local_axis_b: Vec3::Y,
            limits: None,
            motor: Some(hold),
        });
        w.step_substeps(1.0 / 60.0, 1);
        let expected =
            Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2).mul_vec3(Vec3::X * 2.0);
        let got = w.body(grandchild).expect("grandchild").position();
        assert!(
            (got - expected).length() < 2e-3,
            "后代必须随子树整体旋转：{got:?} vs {expected:?}"
        );
    }
    #[test]
    fn kinematic_fk_ignores_joints_with_reused_body_slots() {
        for remove_root in [false, true] {
            let mut w = PhysicsWorld::new();
            w.set_gravity(Vec3::ZERO);
            w.set_collisions_enabled(false);
            w.set_kinematic_fk(true);
            let shape = Shape::Box {
                half_extents: Vec3::splat(0.1),
            };
            let root = w.add_body(BodyBuilder::r#static(shape.clone()).build());
            let child = w.add_body(BodyBuilder::dynamic(shape.clone()).build());
            let joint = w.add_joint(Joint::Slider {
                a: root,
                b: child,
                local_anchor_a: Vec3::ZERO,
                local_anchor_b: Vec3::ZERO,
                local_axis_a: Vec3::X,
                local_axis_b: Vec3::X,
                limits: None,
                motor: Some(crate::joints::HingeMotor {
                    target_angle: Some(2.0),
                    max_impulse: 0.0,
                    target_velocity: 0.0,
                    stiffness: 0.0,
                    damping: 0.0,
                    kp: 0.0,
                    kd: 0.0,
                    max_torque: 0.0,
                }),
            });
            let removed = if remove_root { root } else { child };
            w.remove_body(removed).expect("live endpoint");
            let replacement = w.add_body(if remove_root {
                BodyBuilder::r#static(shape).build()
            } else {
                BodyBuilder::dynamic(shape).build()
            });
            assert_eq!(replacement.index, removed.index);
            assert_ne!(replacement.generation, removed.generation);
            assert!(w.joint_telemetry(joint, 1.0 / 60.0).is_none());
            w.step_substeps(1.0 / 60.0, 1);
            let dynamic = if remove_root { child } else { replacement };
            let position = w.body(dynamic).unwrap().position();
            assert!(position.length() < 1e-6,
                "stale joint must not move a body: remove_root={remove_root}, position={position:?}");
        }
    }

    #[test]
    fn kinematic_fk_reversed_endpoints_preserve_target_sign_and_limits() {
        for slider in [true, false] {
            for (target, limits, expected) in [
                (0.7, None, 0.7),
                (-0.7, None, -0.7),
                (0.7, Some((-0.2, 0.4)), 0.4),
                (-0.7, Some((-0.2, 0.4)), -0.2),
            ] {
                let mut w = PhysicsWorld::new();
                w.set_gravity(Vec3::ZERO);
                w.set_collisions_enabled(false);
                w.set_kinematic_fk(true);
                let shape = Shape::Box {
                    half_extents: Vec3::splat(0.1),
                };
                let root = w.add_body(BodyBuilder::r#static(shape.clone()).build());
                let child = w.add_body(BodyBuilder::dynamic(shape).build());
                let motor = Some(crate::joints::HingeMotor {
                    target_angle: Some(target),
                    max_impulse: 0.0,
                    target_velocity: 0.0,
                    stiffness: 0.0,
                    damping: 0.0,
                    kp: 0.0,
                    kd: 0.0,
                    max_torque: 0.0,
                });
                let joint = w.add_joint(if slider {
                    Joint::Slider {
                        a: child,
                        b: root,
                        local_anchor_a: Vec3::ZERO,
                        local_anchor_b: Vec3::ZERO,
                        local_axis_a: Vec3::X,
                        local_axis_b: Vec3::X,
                        limits,
                        motor,
                    }
                } else {
                    Joint::Hinge {
                        a: child,
                        b: root,
                        local_anchor_a: Vec3::ZERO,
                        local_anchor_b: Vec3::ZERO,
                        local_axis_a: Vec3::X,
                        local_axis_b: Vec3::X,
                        limits,
                        motor,
                    }
                });
                w.step_substeps(1.0 / 60.0, 1);
                let coordinate = w.joint_telemetry(joint, 1.0 / 60.0).unwrap().coordinate;
                assert!((coordinate - expected).abs() < 2e-3,
                    "slider={slider}, target={target}, limits={limits:?}: coordinate={coordinate}, expected={expected}");
            }
        }
    }

    #[test]
    fn kinematic_fk_slides_a_slider_subtree_to_the_target() {
        let mut w = PhysicsWorld::new();
        w.set_gravity(Vec3::ZERO);
        w.set_collisions_enabled(false);
        w.set_kinematic_fk(true);
        let root = w.add_body(
            BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::splat(0.1),
            })
            .position(Vec3::ZERO)
            .build(),
        );
        let child = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.1),
            })
            .position(Vec3::X)
            .build(),
        );
        let motor = crate::joints::HingeMotor {
            target_velocity: 0.0,
            max_impulse: 10.0,
            target_angle: Some(2.0),
            stiffness: 8.0,
            damping: 1.0,
            kp: 0.0,
            kd: 0.0,
            max_torque: 0.0,
        };
        let joint = w.add_joint(Joint::Slider {
            a: root,
            b: child,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: Vec3::ZERO,
            local_axis_a: Vec3::X,
            local_axis_b: Vec3::X,
            limits: None,
            motor: Some(motor),
        });
        w.step_substeps(1.0 / 60.0, 1);
        let got = w.body(child).expect("child").position();
        assert!(
            (got.x - 2.0).abs() < 2e-3,
            "纯 FK 滑块应把子体沿 +X 移到 2.0，实际 {got:?}"
        );
        let coordinate = w
            .joint_telemetry(joint, 1.0 / 60.0)
            .expect("joint telemetry")
            .coordinate;
        assert!(
            (coordinate - 2.0).abs() < 2e-3,
            "滑块坐标应到 2.0，实际 {coordinate}"
        );
    }
}
