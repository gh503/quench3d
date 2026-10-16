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
use crate::island::{IslandStats, Islands};
use crate::joints::{Joint, JointEntry, JointHandle, JointImpulses, JointSolver};
use crate::narrowphase;
#[cfg(not(target_arch = "wasm32"))]
use crate::parallel::ParallelLayout;
use crate::query::{self, QueryFilter, ShapeDistance};
use crate::shape::Shape;
use crate::solver::{self, BodyView, ContactCache, ContactSolver, ManifoldEntry, SolverConfig};

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
struct BodySlot {
    generation: u32,
    body: Option<RigidBody>,
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

    /// 关节数量。
    pub fn joint_count(&self) -> usize {
        self.joints.iter().filter(|s| s.joint.is_some()).count()
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

    /// 单个固定子步。
    fn substep(&mut self, dt: f32) {
        if dt <= 0.0 || !dt.is_finite() {
            return;
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
        self.contact_solver.prepare(
            &views,
            &materials,
            &pre_velocities,
            &self.manifolds,
            &self.cache,
            &self.config,
            dt,
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
        // 13. 睡眠（被马达驱动的刚体这一步不睡，见 `update_sleeping` 的说明）
        let mut actuated = std::mem::take(&mut self.scratch_actuated);
        actuated.clear();
        actuated.resize(self.bodies.len(), false);
        for slot in self.joints.iter() {
            let driven = slot.joint.as_ref().is_some_and(|j| j.has_active_motor());
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

        for k in 0..self.pairs.len() {
            let (ia, ib) = self.pairs[k];
            let (ia, ib) = (ia as usize, ib as usize);
            let (manifold, sensor_pair) = {
                let a = match self.bodies.get(ia).and_then(|s| s.body.as_ref()) {
                    Some(a) => a,
                    None => continue,
                };
                let b = match self.bodies.get(ib).and_then(|s| s.body.as_ref()) {
                    Some(b) => b,
                    None => continue,
                };
                // 分组过滤在宽相已经做过一遍；这里再查一次是因为调用方可以直接改
                // `body.groups`（宽相用的是本子步开头的快照，中间改了要以当前值为准）。
                if !a.groups.interacts(b.groups) {
                    continue;
                }
                // 关节两端缺省不互相碰撞（Bullet / PhysX 的 `collide_connected = false`）。
                // 放在这里而不是宽相：宽相用的是"谁和谁可能碰"的粗筛，
                // 关节过滤依赖关节表，而关节表在子步里是稳定的。
                if !self.joints.is_empty() && self.joint_blocks_contact(ia, ib) {
                    continue;
                }
                (
                    collide_bodies(a, b, self.config.speculative_margin),
                    a.sensor || b.sensor,
                )
            };
            let m = match manifold {
                Some(m) => m,
                None => continue,
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

    /// 连续碰撞检测（CCD）：对开了 `ccd` 的动态刚体，把这一步的位移截在"撞上之前"。
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
    /// 边界：只扫中心线（擦着角过去可能漏）、没有旋转扫掠，"极快 + 极薄"仍可能漏——见文档。
    fn apply_ccd(&mut self, dt: f32) {
        if dt <= 0.0 {
            return;
        }
        // 先收集（这一阶段只读 `self`），避免边遍历边改
        let mut updates: Vec<(usize, Vec3)> = Vec::new();
        for (i, slot) in self.bodies.iter().enumerate() {
            let Some(b) = &slot.body else { continue };
            if !b.ccd || b.body_type != BodyType::Dynamic || b.is_sleeping() {
                continue;
            }
            let motion = b.linear_velocity * dt;
            let total = motion.length();
            if !total.is_finite() || total <= 0.0 {
                continue;
            }
            let self_handle = BodyHandle::new(i as u32, slot.generation);
            let groups = b.groups;
            let dir = motion / total;
            // 中心到表面在**运动方向**上的距离（凸形状上精确：球是半径、盒是各轴投影和）。
            // 用它把"射线打到表面"换算成"中心还能走多远"。
            let reach = b.shape.support(dir).dot(dir).max(0.0);
            let filter = |handle: BodyHandle| -> bool {
                handle != self_handle
                    && self
                        .body(handle)
                        .is_some_and(|other| !other.sensor && other.groups.interacts(groups))
            };
            // 为什么不用保守推进扫掠（`shape_cast`）：它按"到任意方向最近物体的距离"推进，
            // 贴着地面滑行的子弹会被脚下的地面一直拖着（每步只推进地面距离那么远），
            // 而且一旦最近的是"擦身而过"的物体，返回的命中就不该拦——试过，
            // 加了地面之后子弹照样穿墙（见 sim_tests 的同名回归用例）。
            // 中心射线只报告**沿运动方向**的命中，没有这个问题，而且一次查询就够。
            let Some(hit) = self.raycast_filtered(b.transform.position, dir, total + reach, filter)
            else {
                continue;
            };
            let travel = (hit.distance - reach).clamp(0.0, total);
            let n = hit.normal;
            // 朝表面运动的法向分量（负 = 正在靠近）
            let mn = motion.dot(n);
            if mn >= -1e-6 {
                // 掠过的情形：没有再往表面里走，不用管
                continue;
            }
            // 只允许靠近 `travel` 这么多：`mn` 与 `-travel` 取"更靠外"的那个
            let mn_clamped = mn.max(-travel);
            let motion_new = motion - n * (mn - mn_clamped);
            if (motion_new - motion).length_squared() > 0.0 {
                updates.push((i, motion_new / dt));
            }
        }
        for (i, v) in updates {
            if let Some(b) = self.bodies[i].body.as_mut() {
                b.linear_velocity = v;
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
        // 【边界】接触余量只作用于单形状刚体：复合刚体逐部件膨胀要重算局部质心与
        // 聚合 AABB，收益（复合体在贴上前的早接触）远小于代价。
        let sa = a.shape.inflated(margin);
        let sb = b.shape.inflated(margin);
        return narrowphase::collide_shapes(&sa, &a.transform, &sb, &b.transform);
    }
    let mut points: SmallVec<ContactPoint, { ContactManifold::MAX_POINTS }> = SmallVec::new();
    for (sa, ta) in a.parts() {
        let wa = a.part_world_transform(&ta);
        for (sb, tb) in b.parts() {
            let wb = b.part_world_transform(&tb);
            if let Some(m) = narrowphase::collide_shapes(sa, &wa, sb, &wb) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::BodyBuilder;

    fn sphere(r: f32) -> Shape {
        Shape::Sphere { radius: r }
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
}
