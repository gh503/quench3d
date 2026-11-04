//! 顺序冲量求解器：热启动 + 累积摩擦 + 分裂冲量（伪速度）+ 松弛 + 睡眠。
//!
//! 关键设计：
//! * 每个接触点维护「法向 1 + 切向 2」三个累积冲量，逐迭代做投影（clamp）；
//! * 恢复冲量使用**求解前**的相对法向速度，避免求解过程改变恢复目标；
//! * 位置修正走**分裂冲量**：单独维护伪速度，只用于积分位置，不进入真实速度，
//!   因此不会给系统注入能量；
//! * 睡眠：低速持续 0.5s 后冻结速度并停止积分，接触配对时会被运动体唤醒。

use quench3d_math::*;

use crate::body::{
    BodyType, CombineRule, RigidBody, MAX_ANGULAR_SPEED, MAX_LINEAR_SPEED, SLEEP_ANGULAR_TOL,
    SLEEP_LINEAR_TOL, SLEEP_TIME,
};
use crate::contact::ContactPoint;
use crate::small_vec::SmallVec;

/// 求解器侧每个流形最多保留的接触点数（与 `ContactManifold::MAX_POINTS` 一致）。
const MAX_MANIFOLD_POINTS: usize = 4;

/// 求解器可调参数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolverConfig {
    /// 速度迭代次数。
    pub velocity_iterations: usize,
    /// 位置（分裂冲量）迭代次数。
    pub position_iterations: usize,
    /// 松弛迭代次数。
    pub relaxation_iterations: usize,
    /// 是否启用热启动。
    pub warm_starting: bool,
    /// 是否允许 island 并行求解（native）。
    ///
    /// 并行求解在数学上与串行**逐位相同**（岛与岛不共享清醒刚体、岛内保持原顺序），
    /// 这个开关的存在是为了让回归测试能把两条路径的结果直接对比。
    /// wasm 上没有多线程，这个字段恒被忽略。
    pub parallel: bool,
    /// 是否允许窄相候选对并行求交（native；输出与串行逐位相同）。
    ///
    /// 只影响性能，不影响结果：每对候选独立求交，结果按原候选顺序合并。
    /// 候选数太少（< 256）时自动退回串行，避免线程开销。
    pub parallel_narrowphase: bool,
    /// 并行求解最多用几个线程；`0` = 自动（按硬件并行度）。
    ///
    /// 线程数**不影响结果**：岛与岛不共享清醒刚体，每个刚体经历的运算顺序
    /// 与串行完全一致。这个字段主要用于测试「不同线程数结果逐位相同」。
    pub max_threads: usize,
    /// Baumgarte 位置修正系数。
    pub baumgarte: f32,
    /// 允许的穿透余量。
    pub slop: f32,
    /// 位置修正的最大伪速度。
    pub max_correction_velocity: f32,
    /// 低于此相对速度不做恢复（防止抖动）。
    pub restitution_threshold: f32,
    /// 是否启用自动睡眠。
    pub sleep_enabled: bool,
    /// 世界的**缺省**摩擦合成规则（两端都没显式指定时用它）。
    pub friction_combine: CombineRule,
    /// 世界的**缺省**恢复合成规则（两端都没显式指定时用它）。
    ///
    /// 缺省 `Max` 是为了与历史行为逐位一致（旧实现写死 `max(ea, eb)`）。
    pub restitution_combine: CombineRule,
    /// 切向相对速度**低于**这个值（m/s）时用静摩擦系数，高于则用动摩擦系数。
    ///
    /// 静摩擦 ≥ 动摩擦是"推一下才滑动、松手就停住"的关键：只有一个摩擦系数时，
    /// 静止的箱子会以极小的速度缓慢蠕动（每一次求解都把切向冲量顶到锥面上）。
    pub static_friction_speed: f32,
    /// 接触余量（米）：分离但距离小于它的形状对也生成接触点（穿透量为**负**=分离）。
    ///
    /// 缺省 `0.0` = 只在重叠时生成接触（历史行为，结果逐位不变）。写正值让快速
    /// 下落/高速碰撞的物体"即将接触"时就进入约束求解，减少一帧的穿透突跳。
    pub speculative_margin: f32,
    /// 摩擦锥：把两切向累积冲量按**向量模长**夹到 `μ·N`，而不是每轴各夹一次。
    ///
    /// 缺省 `false` = 历史方形摩擦（对角线方向等效摩擦可达 √2·μ），旧场景逐位不变；
    /// 打开后更接近 PhysX/Rapier 的圆形摩擦锥。需要每步按配置解析，不能只存在
    /// 接触里（配置可能在步之间改）。
    pub friction_cone: bool,
    /// Opt in to true-mass global projected normal iterations.
    pub global_normal_projection: bool,
}

impl Default for SolverConfig {
    fn default() -> Self {
        Self {
            velocity_iterations: 10,
            position_iterations: 4,
            relaxation_iterations: 1,
            warm_starting: true,
            parallel: true,
            parallel_narrowphase: true,
            max_threads: 0,
            baumgarte: 0.2,
            slop: 0.005,
            max_correction_velocity: 3.0,
            restitution_threshold: 1.0,
            sleep_enabled: true,
            friction_combine: CombineRule::GeometricMean,
            restitution_combine: CombineRule::Max,
            static_friction_speed: 0.02,
            speculative_margin: 0.0,
            friction_cone: false,
            global_normal_projection: false,
        }
    }
}

/// 刚体存储抽象：世界用带世代的槽位，单元测试用 `Vec<Option<RigidBody>>`。
pub(crate) trait BodyStore {
    /// 按槽位下标可变访问。
    fn slot_mut(&mut self, index: usize) -> Option<&mut RigidBody>;
    /// 槽位数量。
    fn slot_count(&self) -> usize;
}

impl BodyStore for Vec<Option<RigidBody>> {
    fn slot_mut(&mut self, index: usize) -> Option<&mut RigidBody> {
        self.get_mut(index).and_then(|s| s.as_mut())
    }

    fn slot_count(&self) -> usize {
        self.len()
    }
}

/// 求解器看到的刚体视图（值语义，避免迭代时的借用冲突）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct BodyView {
    pub is_static: bool,
    /// 形状原点位置。
    pub pos: Vec3,
    /// 旋转。
    pub rot: Quat,
    /// 质心世界位置。
    pub com: Vec3,
    /// 线速度。
    pub lin: Vec3,
    /// 角速度。
    pub ang: Vec3,
    /// 伪线速度（位置修正用）。
    pub pseudo_lin: Vec3,
    /// 伪角速度。
    pub pseudo_ang: Vec3,
    /// 有效质量倒数（睡眠/静态/运动学为 0）。
    pub inv_m: f32,
    /// 世界逆惯量张量 `R·diag(inv_i_local)·Rᵀ`（冻结时为零矩阵）。
    ///
    /// 每个刚体每步只在这里算一次，之后 `inv_i_world` 就只是一次 3×3 乘向量。
    /// 之前是每次调用都做「四元数逆旋转 → 乘对角 → 四元数正旋转」，
    /// 而它在求解器里每个接触每次迭代都要调用（两侧各一次），
    /// 深堆场景一步就是十几万次，占了相当一部分时间。
    pub inv_i_world_mat: Mat3,
    /// 各向异性摩擦的本地主轴。
    pub friction_axis: Vec3,
    /// 各向异性摩擦强度（0 = 各向同性）。
    pub friction_anisotropy: f32,
}

/// 世界坐标下的逆惯量张量：`R·diag(local)·Rᵀ`。
///
/// `R·diag(local)` 就是把旋转矩阵的各列按 `local` 缩放，再右乘 `Rᵀ` 即可。
#[inline]
pub(crate) fn world_inv_inertia(rot: Quat, local: Vec3) -> Mat3 {
    let r = rot.to_mat3();
    let scaled = Mat3::from_cols(
        r.cols[0] * local.x,
        r.cols[1] * local.y,
        r.cols[2] * local.z,
    );
    scaled * r.transpose()
}

impl BodyView {
    /// 从刚体构造；睡眠或非动态体按无限质量处理。
    pub fn from_body(b: &RigidBody) -> Self {
        let frozen = b.sleeping || b.body_type != BodyType::Dynamic;
        Self {
            is_static: b.body_type == BodyType::Static,
            pos: b.transform.position,
            rot: b.transform.rotation,
            com: b.com_world(),
            lin: b.linear_velocity,
            ang: b.angular_velocity,
            pseudo_lin: Vec3::ZERO,
            pseudo_ang: Vec3::ZERO,
            inv_m: if frozen { 0.0 } else { b.inv_mass },
            inv_i_world_mat: if frozen {
                Mat3::ZERO
            } else {
                world_inv_inertia(b.transform.rotation, b.inv_inertia_local)
            },
            friction_axis: b.friction_axis,
            friction_anisotropy: b.friction_anisotropy,
        }
    }

    /// 空槽位占位（完全冻结、位于原点）。
    pub fn frozen() -> Self {
        Self {
            is_static: false,
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            com: Vec3::ZERO,
            lin: Vec3::ZERO,
            ang: Vec3::ZERO,
            pseudo_lin: Vec3::ZERO,
            pseudo_ang: Vec3::ZERO,
            inv_m: 0.0,
            inv_i_world_mat: Mat3::ZERO,
            friction_axis: Vec3::X,
            friction_anisotropy: 0.0,
        }
    }

    /// 世界逆惯量张量作用。
    #[inline(always)]
    pub fn inv_i_world(&self, v: Vec3) -> Vec3 {
        self.inv_i_world_mat.mul_vec3(v)
    }

    /// 某点速度：`v + ω × r`。
    #[inline]
    pub fn velocity_at(&self, r: Vec3) -> Vec3 {
        self.lin + self.ang.cross(r)
    }

    /// 某点伪速度。
    #[inline]
    pub fn pseudo_velocity_at(&self, r: Vec3) -> Vec3 {
        self.pseudo_lin + self.pseudo_ang.cross(r)
    }

    /// 沿 `n` 的有效质量倒数：`1/m + n·[(I⁻¹(r×n))×r]`。
    #[inline]
    pub fn k_axis(&self, r: Vec3, n: Vec3) -> f32 {
        self.inv_m + n.dot(self.inv_i_world(r.cross(n)).cross(r))
    }
}

/// 缓存的冲量（跨步热启动）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct CachedImpulse {
    /// A 的本地接触点。
    pub local_a: Vec3,
    /// B 的本地接触点。
    pub local_b: Vec3,
    /// 累积法向冲量。
    pub normal: f32,
    /// 累积切向冲量。
    pub tangent: [f32; 2],
    /// 接触特征 ID（见 [`crate::contact::feature_id_from_local`]）。
    pub feature_id: u32,
}

/// 接触冲量缓存（热启动用）。
///
/// 结构是「排序后的刚体对键 + 区间 + 展平的冲量」，所有缓冲跨步复用：
/// 早期实现每步把所有条目塞进 `BTreeMap` 再清空，一个 2000 体场景因此每步
/// 要分配上万次——换掉之后稳态零分配，行为完全一致（按键稳定排序，
/// 同一个键内部保持原来的插入顺序，选中规则不变）。
#[derive(Clone, Debug, Default)]
pub(crate) struct ContactCache {
    /// 稳定排序后的刚体对键。
    keys: Vec<(u32, u32)>,
    /// 每个键在 [`ContactCache::impulses`] 中的区间 `[start, end)`。
    ranges: Vec<(u32, u32)>,
    /// 展平的冲量，按键分组。
    impulses: Vec<CachedImpulse>,
    /// 重建时的排序缓冲 `(键, 原始序号, 冲量)`；
    /// 序号保证不稳定排序后的顺序与稳定排序完全一致（且不分配临时缓冲）。
    scratch: Vec<((u32, u32), u32, CachedImpulse)>,
}

#[cfg(test)]
std::thread_local! {
    pub(crate) static CACHE_REFERENCE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    pub(crate) static CACHE_FAST_REBUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static CACHE_PAIR_SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl ContactCache {
    #[cfg(test)]
    #[allow(clippy::type_complexity)] // Canonical exporter returns borrowed cache columns.
    pub(crate) fn test_export_193(&self) -> (&[(u32, u32)], &[(u32, u32)], &[CachedImpulse]) {
        (&self.keys, &self.ranges, &self.impulses)
    }

    /// 清空（保留已分配的容量）。
    pub fn clear(&mut self) {
        self.keys.clear();
        self.ranges.clear();
        self.impulses.clear();
        self.scratch.clear();
    }

    /// 用当前这一步的接触重建缓存（缓冲复用，稳态零分配）。
    pub fn rebuild(&mut self, contacts: &[SolverContact]) {
        #[cfg(test)]
        if CACHE_REFERENCE.get() == 1 || CACHE_REFERENCE.get() == 3 {
            self.rebuild_unsorted(contacts);
            return;
        }
        // A sorted input already has exactly the stable (pair, sequence) order.
        // Avoid sorting/copying the large scratch tuples on this common path.
        if !contacts
            .windows(2)
            .all(|w| (w[0].a as u32, w[0].b as u32) <= (w[1].a as u32, w[1].b as u32))
        {
            self.rebuild_unsorted(contacts);
            return;
        }
        self.keys.clear();
        self.ranges.clear();
        self.impulses.clear();
        self.scratch.clear();
        self.impulses.reserve(contacts.len());
        for c in contacts {
            let key = (c.a as u32, c.b as u32);
            if self.keys.last() != Some(&key) {
                let start = self.impulses.len() as u32;
                self.keys.push(key);
                self.ranges.push((start, start));
            }
            self.impulses.push(CachedImpulse {
                local_a: c.local_a,
                local_b: c.local_b,
                normal: c.normal_impulse,
                tangent: c.tangent_impulse,
                feature_id: c.feature_id,
            });
            self.ranges.last_mut().unwrap().1 = self.impulses.len() as u32;
        }
        #[cfg(test)]
        if !contacts.is_empty() {
            CACHE_FAST_REBUILDS.set(CACHE_FAST_REBUILDS.get() + 1);
        }
    }

    fn rebuild_unsorted(&mut self, contacts: &[SolverContact]) {
        self.keys.clear();
        self.ranges.clear();
        self.impulses.clear();
        self.scratch.clear();
        self.scratch.reserve(contacts.len());
        for (seq, c) in contacts.iter().enumerate() {
            self.scratch.push((
                (c.a as u32, c.b as u32),
                seq as u32,
                CachedImpulse {
                    local_a: c.local_a,
                    local_b: c.local_b,
                    normal: c.normal_impulse,
                    tangent: c.tangent_impulse,
                    feature_id: c.feature_id,
                },
            ));
        }
        // 按 (刚体对, 原始序号) 做**不稳定**排序：结果与稳定排序完全一致，
        // 但不会为了稳定性分配临时缓冲（那会变成每步一次分配）。
        self.scratch
            .sort_unstable_by(|x, y| x.0.cmp(&y.0).then(x.1.cmp(&y.1)));
        let mut k = 0usize;
        while k < self.scratch.len() {
            let key = self.scratch[k].0;
            let start = self.impulses.len() as u32;
            while k < self.scratch.len() && self.scratch[k].0 == key {
                self.impulses.push(self.scratch[k].2);
                k += 1;
            }
            self.keys.push(key);
            self.ranges.push((start, self.impulses.len() as u32));
        }
    }

    /// 某个刚体对的缓存区间。
    #[inline]
    fn list(&self, a: u32, b: u32) -> &[CachedImpulse] {
        match self.keys.binary_search(&(a, b)) {
            Ok(pos) => {
                let (start, end) = self.ranges[pos];
                &self.impulses[start as usize..end as usize]
            }
            Err(_) => &[],
        }
    }

    /// 缓存条目总数（调试用）。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.impulses.len()
    }

    /// Select within an already located pair, retaining feature and tie order.
    fn lookup_in_list(
        list: &[CachedImpulse],
        local_a: Vec3,
        local_b: Vec3,
        feature_id: u32,
        tol: f32,
    ) -> Option<CachedImpulse> {
        // 首选**特征 ID 精确匹配**：与点顺序、微小位移无关，不需要容差。
        // 这也是"持久流形"的关键——没有它，滑动中的物体会把冲量沿用到相邻特征点上。
        if feature_id != 0 {
            for c in list {
                if c.feature_id == feature_id {
                    return Some(*c);
                }
            }
        }
        let mut best: Option<(f32, CachedImpulse)> = None;
        for c in list {
            // 两端锚点都要接近，避免把不同特征点的冲量错误沿用。
            let d = (c.local_a - local_a).length_squared() + (c.local_b - local_b).length_squared();
            if d > tol * tol {
                continue;
            }
            if best.is_none() || d < best.unwrap().0 {
                best = Some((d, *c));
            }
        }
        best.map(|(_, c)| c)
    }
    #[cfg(test)]
    /// 查询与给定本地锚点最接近的缓存冲量。
    fn lookup(
        &self,
        a: u32,
        b: u32,
        local_a: Vec3,
        local_b: Vec3,
        feature_id: u32,
        tol: f32,
    ) -> Option<CachedImpulse> {
        let list = self.list(a, b);
        // 首选**特征 ID 精确匹配**：与点顺序、微小位移无关，不需要容差。
        // 这也是"持久流形"的关键——没有它，滑动中的物体会把冲量沿用到相邻特征点上。
        if feature_id != 0 {
            for c in list {
                if c.feature_id == feature_id {
                    return Some(*c);
                }
            }
        }
        let mut best: Option<(f32, CachedImpulse)> = None;
        for c in list {
            // 两端锚点都要接近，避免把不同特征点的冲量错误沿用。
            let d = (c.local_a - local_a).length_squared() + (c.local_b - local_b).length_squared();
            if d > tol * tol {
                continue;
            }
            if best.is_none() || d < best.unwrap().0 {
                best = Some((d, *c));
            }
        }
        best.map(|(_, c)| c)
    }
}

/// 求解器内部使用的接触点。
#[derive(Clone, Copy, Debug)]
pub(crate) struct SolverContact {
    /// A 的刚体下标。
    pub a: usize,
    /// B 的刚体下标。
    pub b: usize,
    /// 法线（A→B）。
    pub normal: Vec3,
    /// A 上接触点相对质心的向量。
    pub ra: Vec3,
    /// B 上接触点相对质心的向量。
    pub rb: Vec3,
    /// A 的本地接触点（相对形状原点）。
    pub local_a: Vec3,
    /// B 的本地接触点。
    pub local_b: Vec3,
    /// 切向基。
    pub tangent: [Vec3; 2],
    /// 法向有效质量倒数。
    pub k_normal: f32,
    /// 切向有效质量倒数。
    pub k_tangent: [f32; 2],
    /// 单位法向冲量在 A/B 上产生的角速度变化（准备阶段预乘，迭代里直接缩放）。
    pub normal_arm: [Vec3; 2],
    /// 单位切向冲量在 A/B 上产生的角速度变化。
    pub tangent_arm: [[Vec3; 2]; 2],
    /// 本接触是否使用预算好的角速度响应（大场景快速路径）。
    pub precomputed: bool,
    /// 摩擦系数（各向同性路径的标量值，也保留给缓存/诊断）。
    pub friction: f32,
    /// 两个切向方向的摩擦系数（各向异性时不同；缺省都等于 `friction`）。
    pub friction_tangent: [f32; 2],
    /// 滚动阻力系数（米）。
    pub rolling_friction: f32,
    /// 是否用圆形摩擦锥（准备时从配置冻结，迭代中不变）。
    pub friction_cone: bool,
    /// 恢复目标速度。
    /// 恢复系数偏置。
    pub restitution_bias: f32,
    /// 接触余量偏置（gap #20）：接触点还**分离**时允许的逼近速度 `separation/dt`。
    /// 0 = 已重叠（历史行为）。
    pub speculative_bias: f32,
    /// 初始穿透深度。
    pub penetration: f32,
    /// 累积法向冲量。
    pub normal_impulse: f32,
    /// 累积切向冲量。
    pub tangent_impulse: [f32; 2],
    /// 接触特征 ID（热启动按它对账，见 [`ContactCache::lookup_in_list`]）。
    pub feature_id: u32,
    /// 伪冲量（位置修正）。
    pub pseudo_impulse: f32,
}

/// 求解器看到的"材质"：一对刚体接触时需要的全部摩擦/恢复参数。
///
/// `friction_combine` / `restitution_combine` 各自独立：PhysX 与 Unity 都允许
/// "摩擦取最大值、恢复取平均值"这类组合，用一个全局规则会逼着用户把所有材质
/// 调成同一套。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ContactMaterial {
    /// 动摩擦系数（已经在滑动时用）。
    pub dynamic_friction: f32,
    /// 静摩擦系数（相对切向速度很小时用）。
    pub static_friction: f32,
    /// 滚动阻力系数（米）。
    pub rolling_friction: f32,
    /// 恢复系数。
    pub restitution: f32,
    /// 摩擦的合成规则；`None` = 这一端没指定。
    pub friction_combine: Option<CombineRule>,
    /// 恢复的合成规则；`None` = 这一端没指定。
    pub restitution_combine: Option<CombineRule>,
}

/// 解析一对材质的合成规则：先显式、后缺省；两端都显式且不同时用 `a`。
///
/// 为什么不是"两端的规则再合成一次"：Min/Max 可以合成，`Average` 与
/// `GeometricMean` 混在一起没有物理意义（"平均的平均"是什么？）。
fn resolve_rule(
    a: Option<CombineRule>,
    b: Option<CombineRule>,
    world_default: CombineRule,
) -> CombineRule {
    match (a, b) {
        (Some(ra), Some(rb)) if ra == rb => ra,
        (Some(ra), _) => ra,
        (None, Some(rb)) => rb,
        (None, None) => world_default,
    }
}

/// 一个待求解的接触流形（记录刚体下标）。
#[derive(Clone, Debug)]
pub(crate) struct ManifoldEntry {
    /// A 下标。
    pub a: usize,
    /// B 下标。
    pub b: usize,
    /// 接触点（内联存放，容量 4；超出会自动退化成堆，行为不变）。
    pub points: SmallVec<ContactPoint, MAX_MANIFOLD_POINTS>,
}

/// 接触求解器。
#[derive(Clone, Debug, Default)]
pub(crate) struct ContactSolver {
    /// 所有求解接触点。
    pub contacts: Vec<SolverContact>,
    pub(crate) normal_memory: NormalSolveState,
}

impl ContactSolver {
    /// 准备阶段：计算有效质量、恢复目标、热启动冲量。
    // 参数确实多，但每一个都是"这一步需要的事实"（视图/材质/步前速度/流形/缓存/
    // 配置/步长）。塞进一个结构体只会把调用点都改一遍、把借用关系搞复杂，换不来可读性。
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        views: &[BodyView],
        materials: &[ContactMaterial],
        pre_velocities: &[(Vec3, Vec3)],
        manifolds: &[ManifoldEntry],
        cache: &ContactCache,
        cfg: &SolverConfig,
        dt: f32,
        precompute: bool,
    ) {
        self.normal_memory.reset();
        #[cfg(test)]
        if CACHE_REFERENCE.get() == 1 || CACHE_REFERENCE.get() == 2 {
            self.prepare_reference(
                views,
                materials,
                pre_velocities,
                manifolds,
                cache,
                cfg,
                dt,
                precompute,
            );
            return;
        }
        #[cfg(test)]
        let mut pair_searches = 0;
        self.contacts.clear();
        for m in manifolds {
            let (a, b) = (m.a, m.b);
            let ma = match materials.get(a) {
                Some(x) => *x,
                None => continue,
            };
            let mb = match materials.get(b) {
                Some(x) => *x,
                None => continue,
            };
            let va = views[a];
            let vb = views[b];
            // 合成规则怎么定（**先显式、后缺省**，与 Unity 的 per-collider
            // combine + 工程缺省同一套思路）：
            // 1. 只有一端显式指定 → 用它（"我只在冰面上写 min" 能生效）；
            // 2. 两端都指定且相同 → 就是它；
            // 3. 两端都指定但不同 → 用 **A**（`ContactPair` 的 A 是 slot 序号小的
            //    那个，宽相输出按序号有序，所以结果与线程数/遍历顺序无关）；
            // 4. 两端都没指定 → 世界的缺省规则。
            let friction_rule = resolve_rule(
                ma.friction_combine,
                mb.friction_combine,
                cfg.friction_combine,
            );
            let restitution_rule = resolve_rule(
                ma.restitution_combine,
                mb.restitution_combine,
                cfg.restitution_combine,
            );
            let friction_dynamic = friction_rule.combine(ma.dynamic_friction, mb.dynamic_friction);
            let friction_static = friction_rule.combine(ma.static_friction, mb.static_friction);
            let rolling_friction = ma.rolling_friction.max(mb.rolling_friction);
            let restitution = restitution_rule.combine(ma.restitution, mb.restitution);
            // Lazy: do not search when warm starting is disabled or all points are invalid.
            let mut cached_pair = None;
            for cp in &m.points {
                let (position, normal, penetration) = (cp.position, cp.normal, cp.penetration);
                if !position.is_finite() || !normal.is_finite() || !penetration.is_finite() {
                    continue;
                }
                let n = normal.normalize_or_zero();
                if n.length_squared() < 0.5 {
                    continue;
                }
                let ra = position - va.com;
                let rb = position - vb.com;
                let k_normal = va.k_axis(ra, n) + vb.k_axis(rb, n);
                if !k_normal.is_finite() || k_normal <= 1e-12 {
                    continue;
                }
                let (t1, t2) = n.any_orthonormal_pair();
                let k1 = va.k_axis(ra, t1) + vb.k_axis(rb, t1);
                let k2 = va.k_axis(ra, t2) + vb.k_axis(rb, t2);
                // 单位冲量产生的角速度变化只与接触几何有关，准备阶段算一次，
                // 10 次速度迭代里直接乘标量，省掉每次的叉积 + 3x3 乘法。
                let (normal_arm, tangent_arm) = if precompute {
                    let arm = |v: &BodyView, r: Vec3, dir: Vec3| {
                        if v.inv_m > 0.0 {
                            v.inv_i_world(r.cross(dir))
                        } else {
                            Vec3::ZERO
                        }
                    };
                    (
                        [arm(&va, ra, n), arm(&vb, rb, n)],
                        [
                            [arm(&va, ra, t1), arm(&vb, rb, t1)],
                            [arm(&va, ra, t2), arm(&vb, rb, t2)],
                        ],
                    )
                } else {
                    ([Vec3::ZERO; 2], [[Vec3::ZERO; 2]; 2])
                };
                // 恢复：用求解前的相对法向速度。
                let vrel = vb.velocity_at(rb) - va.velocity_at(ra);
                let vn = vrel.dot(n);
                let restitution_bias = if vn < -cfg.restitution_threshold {
                    -restitution * vn
                } else {
                    0.0
                };
                // 接触余量：穿透量为负 = 两点还差 `-penetration` 米才贴上，
                // 这一步允许以 `separation/dt` 的速度继续逼近；否则物体在余量
                // 距离上就被法向约束顶住（"悬空"）。
                // 只在**用户显式开了接触余量**时才改速度目标：缺省 0 时必须与历史
                // 逐位相同（窄相本来就会给少量负穿透，这里若不加这道门会把它们
                // 也当成"分离的余量接触"，结果整体漂移——digest 测试当场抓出来）。
                let speculative_bias = if cfg.speculative_margin > 0.0 && penetration < 0.0 {
                    (penetration / dt).min(0.0)
                } else {
                    0.0
                };
                let local_a = va.rot.inverse().mul_vec3(position - va.pos);
                let local_b = vb.rot.inverse().mul_vec3(position - vb.pos);
                // 与宽相/窄相用同一个哈希，缓存键才对得上
                let feature_id = crate::contact::feature_id_from_local(local_a, local_b);
                // 静摩擦 vs 动摩擦：看**施力之前**的切向相对速度（`pre_velocities`）。
                //
                // 两个"必须"：
                // * 必须用施力前的速度：静止的箱子被推的那一步，施力后速度已经是
                //   Δt × 加速度（1/60 秒 × 5 m/s² ≈ 0.08 m/s），按它判定会永远走
                //   动摩擦，"静摩擦"这一项就形同虚设；
                // * 必须在准备阶段定一次：如果每次速度迭代重新判断，摩擦锥上界会在
                //   迭代中跳变，收敛会变差，而且结果依赖迭代次数。
                let vt = {
                    let (la, wa) = pre_velocities.get(a).copied().unwrap_or_default();
                    let (lb, wb) = pre_velocities.get(b).copied().unwrap_or_default();
                    let vrel = (lb + wb.cross(rb)) - (la + wa.cross(ra));
                    let tangential = vrel - n * vrel.dot(n);
                    tangential.length()
                };
                let friction = if vt > cfg.static_friction_speed {
                    friction_dynamic
                } else {
                    friction_static
                };
                let anisotropy = if va.friction_anisotropy >= vb.friction_anisotropy {
                    (va.friction_axis, va.friction_anisotropy, va.rot)
                } else {
                    (vb.friction_axis, vb.friction_anisotropy, vb.rot)
                };
                let friction_tangent = anisotropic_friction(
                    friction,
                    anisotropy.0,
                    anisotropy.1,
                    anisotropy.2,
                    n,
                    t1,
                    t2,
                );
                let mut normal_impulse = 0.0;
                let mut tangent_impulse = [0.0f32; 2];
                if cfg.warm_starting {
                    let list = cached_pair.get_or_insert_with(|| {
                        #[cfg(test)]
                        {
                            pair_searches += 1;
                        }
                        cache.list(a as u32, b as u32)
                    });
                    if let Some(c) =
                        ContactCache::lookup_in_list(list, local_a, local_b, feature_id, 0.02)
                    {
                        normal_impulse = c.normal.max(0.0);
                        tangent_impulse = c.tangent;
                    }
                }
                self.contacts.push(SolverContact {
                    a,
                    b,
                    normal: n,
                    ra,
                    rb,
                    local_a,
                    local_b,
                    tangent: [t1, t2],
                    k_normal,
                    k_tangent: [k1, k2],
                    normal_arm,
                    tangent_arm,
                    precomputed: precompute,
                    friction,
                    friction_tangent,
                    rolling_friction,
                    friction_cone: cfg.friction_cone,
                    restitution_bias,
                    speculative_bias,
                    penetration,
                    normal_impulse,
                    tangent_impulse,
                    feature_id,
                    pseudo_impulse: 0.0,
                });
            }
        }
        #[cfg(test)]
        CACHE_PAIR_SEARCHES.set(CACHE_PAIR_SEARCHES.get() + pair_searches);
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn prepare_reference(
        &mut self,
        views: &[BodyView],
        materials: &[ContactMaterial],
        pre_velocities: &[(Vec3, Vec3)],
        manifolds: &[ManifoldEntry],
        cache: &ContactCache,
        cfg: &SolverConfig,
        dt: f32,
        precompute: bool,
    ) {
        self.contacts.clear();
        for m in manifolds {
            let (a, b) = (m.a, m.b);
            let ma = match materials.get(a) {
                Some(x) => *x,
                None => continue,
            };
            let mb = match materials.get(b) {
                Some(x) => *x,
                None => continue,
            };
            let va = views[a];
            let vb = views[b];
            // 合成规则怎么定（**先显式、后缺省**，与 Unity 的 per-collider
            // combine + 工程缺省同一套思路）：
            // 1. 只有一端显式指定 → 用它（"我只在冰面上写 min" 能生效）；
            // 2. 两端都指定且相同 → 就是它；
            // 3. 两端都指定但不同 → 用 **A**（`ContactPair` 的 A 是 slot 序号小的
            //    那个，宽相输出按序号有序，所以结果与线程数/遍历顺序无关）；
            // 4. 两端都没指定 → 世界的缺省规则。
            let friction_rule = resolve_rule(
                ma.friction_combine,
                mb.friction_combine,
                cfg.friction_combine,
            );
            let restitution_rule = resolve_rule(
                ma.restitution_combine,
                mb.restitution_combine,
                cfg.restitution_combine,
            );
            let friction_dynamic = friction_rule.combine(ma.dynamic_friction, mb.dynamic_friction);
            let friction_static = friction_rule.combine(ma.static_friction, mb.static_friction);
            let rolling_friction = ma.rolling_friction.max(mb.rolling_friction);
            let restitution = restitution_rule.combine(ma.restitution, mb.restitution);
            for cp in &m.points {
                let (position, normal, penetration) = (cp.position, cp.normal, cp.penetration);
                if !position.is_finite() || !normal.is_finite() || !penetration.is_finite() {
                    continue;
                }
                let n = normal.normalize_or_zero();
                if n.length_squared() < 0.5 {
                    continue;
                }
                let ra = position - va.com;
                let rb = position - vb.com;
                let k_normal = va.k_axis(ra, n) + vb.k_axis(rb, n);
                if !k_normal.is_finite() || k_normal <= 1e-12 {
                    continue;
                }
                let (t1, t2) = n.any_orthonormal_pair();
                let k1 = va.k_axis(ra, t1) + vb.k_axis(rb, t1);
                let k2 = va.k_axis(ra, t2) + vb.k_axis(rb, t2);
                // 单位冲量产生的角速度变化只与接触几何有关，准备阶段算一次，
                // 10 次速度迭代里直接乘标量，省掉每次的叉积 + 3x3 乘法。
                let (normal_arm, tangent_arm) = if precompute {
                    let arm = |v: &BodyView, r: Vec3, dir: Vec3| {
                        if v.inv_m > 0.0 {
                            v.inv_i_world(r.cross(dir))
                        } else {
                            Vec3::ZERO
                        }
                    };
                    (
                        [arm(&va, ra, n), arm(&vb, rb, n)],
                        [
                            [arm(&va, ra, t1), arm(&vb, rb, t1)],
                            [arm(&va, ra, t2), arm(&vb, rb, t2)],
                        ],
                    )
                } else {
                    ([Vec3::ZERO; 2], [[Vec3::ZERO; 2]; 2])
                };
                // 恢复：用求解前的相对法向速度。
                let vrel = vb.velocity_at(rb) - va.velocity_at(ra);
                let vn = vrel.dot(n);
                let restitution_bias = if vn < -cfg.restitution_threshold {
                    -restitution * vn
                } else {
                    0.0
                };
                // 接触余量：穿透量为负 = 两点还差 `-penetration` 米才贴上，
                // 这一步允许以 `separation/dt` 的速度继续逼近；否则物体在余量
                // 距离上就被法向约束顶住（"悬空"）。
                // 只在**用户显式开了接触余量**时才改速度目标：缺省 0 时必须与历史
                // 逐位相同（窄相本来就会给少量负穿透，这里若不加这道门会把它们
                // 也当成"分离的余量接触"，结果整体漂移——digest 测试当场抓出来）。
                let speculative_bias = if cfg.speculative_margin > 0.0 && penetration < 0.0 {
                    (penetration / dt).min(0.0)
                } else {
                    0.0
                };
                let local_a = va.rot.inverse().mul_vec3(position - va.pos);
                let local_b = vb.rot.inverse().mul_vec3(position - vb.pos);
                // 与宽相/窄相用同一个哈希，缓存键才对得上
                let feature_id = crate::contact::feature_id_from_local(local_a, local_b);
                // 静摩擦 vs 动摩擦：看**施力之前**的切向相对速度（`pre_velocities`）。
                //
                // 两个"必须"：
                // * 必须用施力前的速度：静止的箱子被推的那一步，施力后速度已经是
                //   Δt × 加速度（1/60 秒 × 5 m/s² ≈ 0.08 m/s），按它判定会永远走
                //   动摩擦，"静摩擦"这一项就形同虚设；
                // * 必须在准备阶段定一次：如果每次速度迭代重新判断，摩擦锥上界会在
                //   迭代中跳变，收敛会变差，而且结果依赖迭代次数。
                let vt = {
                    let (la, wa) = pre_velocities.get(a).copied().unwrap_or_default();
                    let (lb, wb) = pre_velocities.get(b).copied().unwrap_or_default();
                    let vrel = (lb + wb.cross(rb)) - (la + wa.cross(ra));
                    let tangential = vrel - n * vrel.dot(n);
                    tangential.length()
                };
                let friction = if vt > cfg.static_friction_speed {
                    friction_dynamic
                } else {
                    friction_static
                };
                let anisotropy = if va.friction_anisotropy >= vb.friction_anisotropy {
                    (va.friction_axis, va.friction_anisotropy, va.rot)
                } else {
                    (vb.friction_axis, vb.friction_anisotropy, vb.rot)
                };
                let friction_tangent = anisotropic_friction(
                    friction,
                    anisotropy.0,
                    anisotropy.1,
                    anisotropy.2,
                    n,
                    t1,
                    t2,
                );
                let mut normal_impulse = 0.0;
                let mut tangent_impulse = [0.0f32; 2];
                if cfg.warm_starting {
                    if let Some(c) =
                        cache.lookup(a as u32, b as u32, local_a, local_b, feature_id, 0.02)
                    {
                        normal_impulse = c.normal.max(0.0);
                        tangent_impulse = c.tangent;
                    }
                }
                self.contacts.push(SolverContact {
                    a,
                    b,
                    normal: n,
                    ra,
                    rb,
                    local_a,
                    local_b,
                    tangent: [t1, t2],
                    k_normal,
                    k_tangent: [k1, k2],
                    normal_arm,
                    tangent_arm,
                    precomputed: precompute,
                    friction,
                    friction_tangent,
                    rolling_friction,
                    friction_cone: cfg.friction_cone,
                    restitution_bias,
                    speculative_bias,
                    penetration,
                    normal_impulse,
                    tangent_impulse,
                    feature_id,
                    pseudo_impulse: 0.0,
                });
            }
        }
    }

    /// 热启动：把缓存冲量作为初始猜测应用到速度上。
    pub fn warm_start(&self, views: &mut [BodyView], cfg: &SolverConfig) {
        if !cfg.warm_starting {
            return;
        }
        for c in &self.contacts {
            let p = c.normal * c.normal_impulse
                + c.tangent[0] * c.tangent_impulse[0]
                + c.tangent[1] * c.tangent_impulse[1];
            apply_impulse(views, c.a, c.b, c.ra, c.rb, p);
        }
    }

    /// 一轮速度迭代。
    pub fn solve_velocity(&mut self, views: &mut [BodyView], global: bool) {
        solve_velocity_contacts_state(&mut self.contacts, views, global, &mut self.normal_memory);
    }

    /// 松弛：只在仍然相互接近时补一点冲量，绝不抽走能量。
    pub fn relax(&mut self, views: &mut [BodyView], iterations: usize, gain: f32) {
        relax_contacts(&mut self.contacts, views, iterations, gain);
    }

    /// 位置修正（分裂冲量）：只更新伪速度。
    pub fn solve_position(
        &mut self,
        views: &mut [BodyView],
        dt: f32,
        cfg: &SolverConfig,
        iterations: usize,
    ) {
        solve_position_contacts(&mut self.contacts, views, dt, cfg, iterations);
    }

    /// 把本步的冲量写回缓存，供下一帧热启动。
    pub fn store_cache(&self, cache: &mut ContactCache) {
        cache.rebuild(&self.contacts);
    }
}

/// 把标量摩擦系数按刚体本地主轴投影成两个切向方向的系数。
///
/// `axis` 是本地轴、`rot` 是刚体旋转；投影到接触切平面后，切向基上的分量平方
/// 决定插值：沿轴方向 `μ(1+ratio)`，垂直方向 `μ(1-ratio)`。轴与法线平行
/// （投影退化）时保持各向同性。缺省 ratio=0 时两个返回值都逐位等于 `friction`。
#[allow(clippy::too_many_arguments)]
fn anisotropic_friction(
    friction: f32,
    axis_local: Vec3,
    ratio: f32,
    rot: Quat,
    n: Vec3,
    t1: Vec3,
    t2: Vec3,
) -> [f32; 2] {
    if ratio <= 0.0 {
        return [friction, friction];
    }
    let world_axis = rot.mul_vec3(axis_local);
    let projected = world_axis - n * world_axis.dot(n);
    if projected.length_squared() <= 1e-10 {
        return [friction, friction];
    }
    let axis = projected.normalize();
    let d0 = axis.dot(t1);
    let d1 = axis.dot(t2);
    let scale0 = 1.0 + ratio * (2.0 * d0 * d0 - 1.0);
    let scale1 = 1.0 + ratio * (2.0 * d1 * d1 - 1.0);
    [(friction * scale0).max(0.0), (friction * scale1).max(0.0)]
}

/// 对一组接触做一轮速度迭代。
///
/// 串行路径传整张接触表，并行路径传某个 island 的接触切片——**同一份实现**，
/// 避免两条路径的浮点运算顺序悄悄分叉。调用方必须保证切片内接触的下标
/// 已经映射到 `views` 的局部下标。
pub(crate) fn solve_velocity_contacts_memory(
    contacts: &mut [SolverContact],
    views: &mut [BodyView],
    global: bool,
    memory: &mut NormalDirectionMemory,
) {
    if global {
        global_normal_round(contacts, views, memory);
        return;
    }
    for c in contacts.iter_mut() {
        let Some((va, vb)) = split_body_pair(views, c.a, c.b) else {
            continue;
        };
        solve_velocity_contact(c, va, vb);
    }
}

#[inline(always)]
fn solve_velocity_contact(c: &mut SolverContact, va: &mut BodyView, vb: &mut BodyView) {
    let mut vrel = pair_relative_velocity(va, vb, c.ra, c.rb);
    // 摩擦（用上一轮的法向冲量作为上界）
    let max_friction = c.friction * c.normal_impulse;
    // 各向异性时两个切向的上界不同；ratio=0 时两值逐位等于 friction，
    // 下面仍走原来的标量路径，旧场景/旧测试逐位不变。
    let anisotropic = c.friction_tangent[0].to_bits() != c.friction.to_bits()
        || c.friction_tangent[1].to_bits() != c.friction.to_bits();
    if c.friction_cone {
        // 圆形摩擦锥：两个切向方向一起投影，再按总量缩放。旧路径的
        // 逐轴 clamp 在对角线方向允许 √2·μ·N。
        if c.k_tangent[0] > 1e-12 && c.k_tangent[1] > 1e-12 {
            let d0 = -vrel.dot(c.tangent[0]) / c.k_tangent[0];
            let d1 = -vrel.dot(c.tangent[1]) / c.k_tangent[1];
            let mut next = [c.tangent_impulse[0] + d0, c.tangent_impulse[1] + d1];
            let len = (next[0] * next[0] + next[1] * next[1]).sqrt();
            // 各向异性 + 圆形锥没有单一 μ：取两个方向上限的平均值作为
            // 保守上界。真正沿椭圆锥的投影留给后续版本。
            let cone_limit = if anisotropic {
                (c.friction_tangent[0] + c.friction_tangent[1]) * 0.5 * c.normal_impulse
            } else {
                max_friction
            };
            if len > cone_limit {
                let scale = cone_limit / len;
                next[0] *= scale;
                next[1] *= scale;
            }
            for (i, n) in next.iter().enumerate() {
                let d = *n - c.tangent_impulse[i];
                c.tangent_impulse[i] = *n;
                if d != 0.0 {
                    if c.precomputed {
                        apply_precomputed(va, vb, c.tangent[i], c.tangent_arm[i], d);
                    } else {
                        pair_apply_impulse(va, vb, c.ra, c.rb, c.tangent[i] * d);
                    }
                }
            }
            vrel = pair_relative_velocity(va, vb, c.ra, c.rb);
        }
    } else {
        for i in 0..2 {
            let k = c.k_tangent[i];
            if k <= 1e-12 {
                continue;
            }
            let vt = vrel.dot(c.tangent[i]);
            let mut d = -vt / k;
            let limit = if anisotropic {
                c.friction_tangent[i] * c.normal_impulse
            } else {
                max_friction
            };
            let new = clamp(c.tangent_impulse[i] + d, -limit, limit);
            d = new - c.tangent_impulse[i];
            c.tangent_impulse[i] = new;
            // 冲量没变时不需要再乘一遍/重算相对速度；结果逐位相同。
            if d != 0.0 {
                if c.precomputed {
                    apply_precomputed(va, vb, c.tangent[i], c.tangent_arm[i], d);
                } else {
                    pair_apply_impulse(va, vb, c.ra, c.rb, c.tangent[i] * d);
                }
                vrel = pair_relative_velocity(va, vb, c.ra, c.rb);
            }
        }
    }
    // 法向
    let vn = vrel.dot(c.normal);
    // `speculative_bias` 见 prepare：分离的接触点允许以 separation/dt 逼近
    let mut d = -(vn - (c.restitution_bias + c.speculative_bias)) / c.k_normal;
    let new = (c.normal_impulse + d).max(0.0);
    d = new - c.normal_impulse;
    c.normal_impulse = new;
    if d != 0.0 {
        if c.precomputed {
            apply_precomputed(va, vb, c.normal, c.normal_arm, d);
        } else {
            pair_apply_impulse(va, vb, c.ra, c.rb, c.normal * d);
        }
    }

    // 滚动阻力：用角冲量把相对角速度往 0 拉，上限与法向冲量成正比；
    // 缺省 0 时这条分支完全不会进入，历史结果逐位不变。
    if c.rolling_friction > 0.0 && c.normal_impulse > 0.0 {
        let wrel = vb.ang - va.ang;
        // 标量近似 1/trace(invI_a + invI_b)，避免为每个接触求 3x3 逆。
        let k = va.inv_i_world_mat.cols[0].x
            + va.inv_i_world_mat.cols[1].y
            + va.inv_i_world_mat.cols[2].z
            + vb.inv_i_world_mat.cols[0].x
            + vb.inv_i_world_mat.cols[1].y
            + vb.inv_i_world_mat.cols[2].z;
        if k.is_finite() && k > 1e-9 {
            let limit = c.rolling_friction * c.normal_impulse * 0.5;
            let dl = (wrel * (-1.0 / k)).clamp_length(limit);
            pair_apply_angular_impulse(va, vb, dl);
        }
    }
}

/// 从 `views` 里安全取出两个**不同下标**的可变引用（保持 `(a, b)` 顺序）。
#[inline(always)]
fn split_body_pair(
    views: &mut [BodyView],
    a: usize,
    b: usize,
) -> Option<(&mut BodyView, &mut BodyView)> {
    if a == b || a >= views.len() || b >= views.len() {
        return None;
    }
    if a < b {
        let (left, right) = views.split_at_mut(b);
        Some((&mut left[a], &mut right[0]))
    } else {
        let (left, right) = views.split_at_mut(a);
        Some((&mut right[0], &mut left[b]))
    }
}

/// 测试用的整表版本；生产路径已改为 [`pair_relative_velocity`]。
#[cfg(test)]
#[inline]
fn relative_velocity(views: &[BodyView], a: usize, b: usize, ra: Vec3, rb: Vec3) -> Vec3 {
    let va = &views[a];
    let vb = &views[b];
    (vb.lin + vb.ang.cross(rb)) - (va.lin + va.ang.cross(ra))
}

/// 测试用的整表版本；生产路径已改为 [`pair_relative_pseudo_velocity`]。
#[cfg(test)]
#[inline]
fn relative_pseudo_velocity(views: &[BodyView], a: usize, b: usize, ra: Vec3, rb: Vec3) -> Vec3 {
    let va = &views[a];
    let vb = &views[b];
    (vb.pseudo_lin + vb.pseudo_ang.cross(rb)) - (va.pseudo_lin + va.pseudo_ang.cross(ra))
}

#[inline(always)]
fn pair_relative_velocity(va: &BodyView, vb: &BodyView, ra: Vec3, rb: Vec3) -> Vec3 {
    (vb.lin + vb.ang.cross(rb)) - (va.lin + va.ang.cross(ra))
}

/// 用准备阶段预乘好的角速度响应施加单位方向冲量。
///
/// 线性部分保持 `(dir * d) * inv_m` 的原运算顺序；角速度部分把
/// `inv_i_world(r × (dir*d))` 改成 `inv_i_world(r × dir) * d`。两者数学等价，
/// 但浮点末位可能差 1 ulp，所以只有几何预乘这一步在准备阶段做。
#[inline(always)]
fn apply_precomputed(va: &mut BodyView, vb: &mut BodyView, dir: Vec3, arm: [Vec3; 2], d: f32) {
    if !d.is_finite() {
        return;
    }
    let impulse = dir * d;
    if va.inv_m > 0.0 {
        va.lin -= impulse * va.inv_m;
        va.ang -= arm[0] * d;
    }
    if vb.inv_m > 0.0 {
        vb.lin += impulse * vb.inv_m;
        vb.ang += arm[1] * d;
    }
}

#[inline(always)]
fn pair_apply_impulse(va: &mut BodyView, vb: &mut BodyView, ra: Vec3, rb: Vec3, impulse: Vec3) {
    if !impulse.is_finite() {
        return;
    }
    if va.inv_m > 0.0 {
        va.lin -= impulse * va.inv_m;
        va.ang -= va.inv_i_world(ra.cross(impulse));
    }
    if vb.inv_m > 0.0 {
        vb.lin += impulse * vb.inv_m;
        vb.ang += vb.inv_i_world(rb.cross(impulse));
    }
}

#[inline(always)]
fn pair_apply_angular_impulse(va: &mut BodyView, vb: &mut BodyView, l: Vec3) {
    if !l.is_finite() {
        return;
    }
    va.ang -= va.inv_i_world(l);
    vb.ang += vb.inv_i_world(l);
}

/// 对一组接触做松弛（实现见 [`ContactSolver::relax`]）。
pub(crate) fn relax_contacts(
    contacts: &mut [SolverContact],
    views: &mut [BodyView],
    iterations: usize,
    gain: f32,
) {
    {
        for _ in 0..iterations {
            for c in contacts.iter_mut() {
                let Some((va, vb)) = split_body_pair(views, c.a, c.b) else {
                    continue;
                };
                let vrel = pair_relative_velocity(va, vb, c.ra, c.rb);
                let vn = vrel.dot(c.normal);
                if vn >= 0.0 || c.normal_impulse <= 0.0 {
                    continue;
                }
                let d = -gain * vn / c.k_normal;
                c.normal_impulse += d;
                if c.precomputed {
                    apply_precomputed(va, vb, c.normal, c.normal_arm, d);
                } else {
                    pair_apply_impulse(va, vb, c.ra, c.rb, c.normal * d);
                }
            }
        }
    }
}

/// 对一组接触做位置修正（实现见 [`ContactSolver::solve_position`]）。
pub(crate) fn solve_position_contacts(
    contacts: &mut [SolverContact],
    views: &mut [BodyView],
    dt: f32,
    cfg: &SolverConfig,
    iterations: usize,
) {
    {
        if dt <= 0.0 {
            return;
        }
        for c in contacts.iter_mut() {
            c.pseudo_impulse = 0.0;
        }
        for _ in 0..iterations {
            for c in contacts.iter_mut() {
                // 用窄相本步给出的穿透深度作为目标：解析接触的两个「锚点」可能
                // 退化成同一点（例如球-面），因此不能用锚点间距反推穿透。
                let pen = c.penetration;
                if pen <= cfg.slop {
                    continue;
                }
                let Some((va, vb)) = split_body_pair(views, c.a, c.b) else {
                    continue;
                };
                let target =
                    ((cfg.baumgarte / dt) * (pen - cfg.slop)).min(cfg.max_correction_velocity);
                let vrel = pair_relative_pseudo_velocity(va, vb, c.ra, c.rb);
                let vn = vrel.dot(c.normal);
                let mut d = (target - vn) / c.k_normal;
                let new = (c.pseudo_impulse + d).max(0.0);
                d = new - c.pseudo_impulse;
                if d <= 0.0 {
                    continue;
                }
                c.pseudo_impulse = new;
                if c.precomputed {
                    apply_pseudo_precomputed(va, vb, c.normal, c.normal_arm, d);
                } else {
                    apply_pseudo_pair(va, vb, c.ra, c.rb, c.normal * d);
                }
            }
        }
    }
}

#[inline(always)]
fn pair_relative_pseudo_velocity(va: &BodyView, vb: &BodyView, ra: Vec3, rb: Vec3) -> Vec3 {
    (vb.pseudo_lin + vb.pseudo_ang.cross(rb)) - (va.pseudo_lin + va.pseudo_ang.cross(ra))
}

/// 与 [`apply_pseudo_impulse`] 逐位相同的双引用版本。
#[inline(always)]
fn apply_pseudo_pair(va: &mut BodyView, vb: &mut BodyView, ra: Vec3, rb: Vec3, impulse: Vec3) {
    if !impulse.is_finite() {
        return;
    }
    if va.inv_m > 0.0 {
        va.pseudo_lin -= impulse * va.inv_m;
        va.pseudo_ang -= va.inv_i_world(ra.cross(impulse));
    }
    if vb.inv_m > 0.0 {
        vb.pseudo_lin += impulse * vb.inv_m;
        vb.pseudo_ang += vb.inv_i_world(rb.cross(impulse));
    }
}

#[inline(always)]
fn apply_pseudo_precomputed(
    va: &mut BodyView,
    vb: &mut BodyView,
    dir: Vec3,
    arm: [Vec3; 2],
    d: f32,
) {
    if !d.is_finite() {
        return;
    }
    let impulse = dir * d;
    if va.inv_m > 0.0 {
        va.pseudo_lin -= impulse * va.inv_m;
        va.pseudo_ang -= arm[0] * d;
    }
    if vb.inv_m > 0.0 {
        vb.pseudo_lin += impulse * vb.inv_m;
        vb.pseudo_ang += arm[1] * d;
    }
}

/// 对一对刚体施加世界冲量（作用于指定力臂）。
#[inline]
pub(crate) fn apply_impulse(
    views: &mut [BodyView],
    a: usize,
    b: usize,
    ra: Vec3,
    rb: Vec3,
    impulse: Vec3,
) {
    if !impulse.is_finite() {
        return;
    }
    if let Some(va) = views.get_mut(a) {
        if va.inv_m > 0.0 {
            va.lin -= impulse * va.inv_m;
            va.ang -= va.inv_i_world(ra.cross(impulse));
        }
    }
    if let Some(vb) = views.get_mut(b) {
        if vb.inv_m > 0.0 {
            vb.lin += impulse * vb.inv_m;
            vb.ang += vb.inv_i_world(rb.cross(impulse));
        }
    }
}

/// 伪冲量版本（只影响位置）。
#[inline]
pub(crate) fn apply_pseudo_impulse(
    views: &mut [BodyView],
    a: usize,
    b: usize,
    ra: Vec3,
    rb: Vec3,
    impulse: Vec3,
) {
    if !impulse.is_finite() {
        return;
    }
    if let Some(va) = views.get_mut(a) {
        if va.inv_m > 0.0 {
            va.pseudo_lin -= impulse * va.inv_m;
            va.pseudo_ang -= va.inv_i_world(ra.cross(impulse));
        }
    }
    if let Some(vb) = views.get_mut(b) {
        if vb.inv_m > 0.0 {
            vb.pseudo_lin += impulse * vb.inv_m;
            vb.pseudo_ang += vb.inv_i_world(rb.cross(impulse));
        }
    }
}

/// 速度积分：重力、外力、阻尼。
pub(crate) fn integrate_velocities<S: BodyStore>(bodies: &mut S, gravity: Vec3, dt: f32) {
    for i in 0..bodies.slot_count() {
        let b = match bodies.slot_mut(i) {
            Some(b) => b,
            None => continue,
        };
        if b.body_type != BodyType::Dynamic || b.sleeping {
            b.force = Vec3::ZERO;
            b.torque = Vec3::ZERO;
            continue;
        }
        let mut v = b.linear_velocity + (gravity * b.gravity_scale + b.force * b.inv_mass) * dt;
        let mut w = b.angular_velocity + b.apply_inv_inertia_world(b.torque) * dt;
        if b.linear_damping > 0.0 {
            v /= 1.0 + b.linear_damping * dt;
        }
        if b.angular_damping > 0.0 {
            w /= 1.0 + b.angular_damping * dt;
        }
        b.linear_velocity = v.clamp_length(MAX_LINEAR_SPEED);
        b.angular_velocity = w.clamp_length(MAX_ANGULAR_SPEED);
        b.force = Vec3::ZERO;
        b.torque = Vec3::ZERO;
    }
}

/// 位置积分：`x += (v + v_pseudo)·dt`，四元数用一阶导数并重新归一化。
pub(crate) fn integrate_positions<S: BodyStore>(bodies: &mut S, dt: f32) {
    for i in 0..bodies.slot_count() {
        let b = match bodies.slot_mut(i) {
            Some(b) => b,
            None => continue,
        };
        if b.body_type == BodyType::Static || b.sleeping {
            continue;
        }
        let v = b.linear_velocity + b.pseudo_linear_velocity;
        let w = b.angular_velocity + b.pseudo_angular_velocity;
        if v.is_finite() {
            b.transform.position += v * dt;
        }
        if w.is_finite() {
            let q = b.transform.rotation;
            let dq = Quat::new(w.x, w.y, w.z, 0.0) * q;
            let half = 0.5 * dt;
            b.transform.rotation = Quat::new(
                q.x + dq.x * half,
                q.y + dq.y * half,
                q.z + dq.z * half,
                q.w + dq.w * half,
            )
            .normalize_or_identity();
        }
        b.pseudo_linear_velocity = Vec3::ZERO;
        b.pseudo_angular_velocity = Vec3::ZERO;
        b.sanitize();
    }
}

/// 睡眠状态更新。
/// `actuated` 是"被马达驱动"的刚体（按槽位索引，长度 = `slot_count`）：
/// 这些刚体这一步**不许睡**，也不能让同岛的其他刚体睡。
///
/// 为什么：马达是执行器。一个"保持 90°"的伺服在到位之后速度正好是 0，
/// 只看速度的睡眠判定会立刻把它判为静止、`sleep()` 清零速度并冻结——
/// 于是伺服停在一个**没到目标**的角度上再也回不来（真实踩到过：
/// 门停在 116° 而目标是 90°）。执行器驱动的东西必须持续参与求解。
pub(crate) fn update_sleeping<S: BodyStore>(
    bodies: &mut S,
    dt: f32,
    cfg: &SolverConfig,
    islands: &crate::island::Islands,
    actuated: &[bool],
) {
    if !cfg.sleep_enabled {
        for i in 0..bodies.slot_count() {
            if let Some(b) = bodies.slot_mut(i) {
                b.sleep_timer = 0.0;
                b.sleeping = false;
            }
        }
        return;
    }
    // 第一遍：逐体累计「足够慢」的时间。
    //
    // 注意这里**不直接入睡**——是否入睡由整个 island 统一决定（第二遍）。
    // 逐体入睡会留下「一半睡着、一半还在抖」的堆叠：醒着的那几个继续被求解，
    // 睡着的那几个被当成无限质量冻结，接触力传不下去，堆叠就会慢慢渗进去。
    for i in 0..bodies.slot_count() {
        let b = match bodies.slot_mut(i) {
            Some(b) => b,
            None => continue,
        };
        if b.body_type != BodyType::Dynamic {
            continue;
        }
        // `allow_sleep == false` 是**硬承诺**：永远清醒。
        //
        // 少了这一段，"我明确说了不许睡"会被一次手动 `RigidBody::sleep()` 悄悄推翻
        // （它把自己置为睡眠，而这一遍只看速度、不看 `allow_sleep`，于是它就一直睡着）。
        // 反过来这里醒来是有代价的（每一步都要参与求解），但那正是调用方要的语义。
        if !b.allow_sleep {
            b.sleeping = false;
            b.sleep_timer = 0.0;
            continue;
        }
        // 被马达驱动的刚体同理（而且是**临时**的：马达一撤就恢复正常的睡眠判定）。
        if actuated.get(i).copied().unwrap_or(false) {
            b.sleeping = false;
            b.sleep_timer = 0.0;
            continue;
        }
        let slow = b.linear_velocity.length() < SLEEP_LINEAR_TOL
            && b.angular_velocity.length() < SLEEP_ANGULAR_TOL;
        if slow {
            b.sleep_timer += dt;
        } else {
            b.sleep_timer = 0.0;
            b.sleeping = false;
        }
    }
    // 第二遍：岛内**所有**成员都慢够了时间，整岛一起入睡。
    //
    // 「慢够了」用计时器判断：计时器到阈值就意味着它一直在阈值以下
    // （一旦超阈值计时器立刻清零）。`allow_sleep == false` 的刚体（例如玩家控制的
    // 角色）不阻止同岛的其他刚体入睡，但它自己不会被置为睡眠。
    for k in 0..islands.len() {
        let members = islands.bodies_of(k as u32);
        if members.is_empty() {
            continue;
        }
        let mut all_ready = true;
        for &slot in members {
            if let Some(b) = bodies.slot_mut(slot as usize) {
                if b.sleep_timer < SLEEP_TIME {
                    all_ready = false;
                }
            }
        }
        if all_ready {
            for &slot in members {
                if let Some(b) = bodies.slot_mut(slot as usize) {
                    if b.allow_sleep
                        && !b.sleeping
                        && !actuated.get(slot as usize).copied().unwrap_or(false)
                    {
                        b.sleep();
                    }
                }
            }
        }
    }
    // 兜底：没有进入任何岛的动态刚体（正常情况不会有）仍然按逐体计时入睡，
    // 避免因为划分的边界情况让某个刚体永远醒着。
    for i in 0..bodies.slot_count() {
        if islands.island_of(i).is_some() {
            continue;
        }
        if let Some(b) = bodies.slot_mut(i) {
            if b.body_type == BodyType::Dynamic
                && b.allow_sleep
                && !b.sleeping
                && b.sleep_timer >= SLEEP_TIME
                && !actuated.get(i).copied().unwrap_or(false)
            {
                b.sleep();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::BodyBuilder;
    use crate::shape::Shape;

    fn box_body(y: f32) -> RigidBody {
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .position(vec3(0.0, y, 0.0))
        .build()
    }

    #[test]
    fn body_view_freezes_sleeping_bodies() {
        let mut b = box_body(0.0);
        b.sleeping = true;
        let v = BodyView::from_body(&b);
        assert_eq!(v.inv_m, 0.0);
        // 冻结体：世界逆惯量是零矩阵，任何冲量都推不动
        assert_eq!(v.inv_i_world(Vec3::new(1.0, 2.0, 3.0)), Vec3::ZERO);
        b.sleeping = false;
        let v2 = BodyView::from_body(&b);
        assert!(v2.inv_m > 0.0);
        assert!(v2.inv_i_world(Vec3::new(0.0, 1.0, 0.0)).length() > 0.0);
    }

    #[test]
    fn impulse_application_changes_velocity() {
        let bodies = [Some(box_body(0.0)), Some(box_body(1.0))];
        let mut views: Vec<BodyView> = bodies
            .iter()
            .map(|b| BodyView::from_body(b.as_ref().unwrap()))
            .collect();
        apply_impulse(
            &mut views,
            0,
            1,
            Vec3::ZERO,
            Vec3::ZERO,
            vec3(1.0, 0.0, 0.0),
        );
        assert!(views[0].lin.x < 0.0, "A 应被推向 -X");
        assert!(views[1].lin.x > 0.0, "B 应被推向 +X");
    }

    /// 优化后的相对速度辅助函数必须与旧的“复制 BodyView 再调 velocity_at”逐位一致。
    #[test]
    fn relative_velocity_helper_matches_body_view_formula() {
        let mut a = box_body(0.0);
        let mut b = box_body(1.0);
        a.linear_velocity = vec3(0.25, -0.5, 1.5);
        a.angular_velocity = vec3(0.1, 0.2, -0.3);
        b.linear_velocity = vec3(-1.0, 0.75, 0.0);
        b.angular_velocity = vec3(-0.4, 0.05, 0.6);
        b.transform.rotation =
            quench3d_math::Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize(), 0.7);
        let mut views = vec![BodyView::from_body(&a), BodyView::from_body(&b)];
        views[0].pseudo_lin = vec3(0.2, 0.0, -0.1);
        views[0].pseudo_ang = vec3(0.0, 0.3, 0.0);
        views[1].pseudo_lin = vec3(-0.2, 0.1, 0.0);
        views[1].pseudo_ang = vec3(0.1, 0.0, -0.2);
        for (ra, rb) in [
            (Vec3::ZERO, Vec3::ZERO),
            (vec3(0.5, 0.0, 0.0), vec3(-0.5, 0.0, 0.0)),
            (vec3(0.1, 0.2, 0.3), vec3(-0.3, 0.0, 0.2)),
        ] {
            let old = views[1].velocity_at(rb) - views[0].velocity_at(ra);
            assert_eq!(relative_velocity(&views, 0, 1, ra, rb), old);
            let old_pseudo = views[1].pseudo_velocity_at(rb) - views[0].pseudo_velocity_at(ra);
            assert_eq!(relative_pseudo_velocity(&views, 0, 1, ra, rb), old_pseudo);
        }
    }

    #[test]
    fn integration_keeps_quaternion_normalized() {
        let mut b = box_body(0.0);
        b.angular_velocity = vec3(1.0, 2.0, 3.0);
        let mut bodies = vec![Some(b)];
        for _ in 0..600 {
            integrate_positions(&mut bodies, 1.0 / 60.0);
        }
        let q = bodies[0].as_ref().unwrap().transform.rotation;
        assert!((q.length() - 1.0).abs() < 1e-4, "四元数模长 {}", q.length());
        assert!(q.is_finite());
    }

    /// 把一个刚体数组做成「每个刚体各自一个岛」的划分（单元测试用）。
    fn singleton_islands(bodies: &[Option<RigidBody>]) -> crate::island::Islands {
        let views: Vec<BodyView> = bodies.iter().flatten().map(BodyView::from_body).collect();
        let mut islands = crate::island::Islands::new();
        islands.begin(&views);
        islands.finish();
        islands
    }

    #[test]
    fn sleep_after_still_period() {
        let mut bodies = vec![Some(box_body(0.0))];
        let cfg = SolverConfig::default();
        for _ in 0..60 {
            let islands = singleton_islands(&bodies);
            update_sleeping(&mut bodies, 1.0 / 60.0, &cfg, &islands, &[]);
        }
        assert!(bodies[0].as_ref().unwrap().is_sleeping());
        bodies[0].as_mut().unwrap().apply_impulse(Vec3::Y);
        assert!(!bodies[0].as_ref().unwrap().is_sleeping());
    }
}

#[cfg(test)]
mod world_inv_inertia_tests {
    use super::*;

    /// 旧的实现方式：两次四元数旋转 + 逐分量乘对角。
    fn by_quat_formula(rot: Quat, local: Vec3, v: Vec3) -> Vec3 {
        let l = rot.inverse().mul_vec3(v) * local;
        rot.mul_vec3(l)
    }

    fn sample_quat(i: usize) -> Quat {
        // 确定性「伪随机」单位四元数：不用 rand，保证测试可复现
        let a = (i as f32 * 0.7).sin();
        let b = (i as f32 * 1.3).cos();
        let c = (i as f32 * 2.1).sin();
        Quat::from_axis_angle(Vec3::new(a, b, c).normalize_or_zero(), i as f32 * 0.37)
    }

    /// 预计算矩阵必须与「每次重建」的公式数值等价（差异只在 ULP 量级）。
    #[test]
    fn matrix_form_matches_quat_formula() {
        let mut worst: f32 = 0.0;
        for i in 0..500 {
            let rot = sample_quat(i);
            let local = Vec3::new(
                0.5 + (i % 7) as f32 * 0.3,
                0.4 + (i % 5) as f32 * 0.2,
                0.6 + (i % 3) as f32 * 0.4,
            );
            let m = world_inv_inertia(rot, local);
            for j in 0..9 {
                let v = Vec3::new(
                    (i * 3 + j) as f32 * 0.11 - 1.0,
                    (i * 5 + j) as f32 * 0.07 - 0.5,
                    (i * 7 + j) as f32 * 0.13 - 0.8,
                );
                let a = m.mul_vec3(v);
                let b = by_quat_formula(rot, local, v);
                let scale = b.length().max(1e-3);
                worst = worst.max((a - b).length() / scale);
            }
        }
        println!("矩阵形式与四元数公式的最大相对偏差: {worst:e}");
        assert!(
            worst < 1e-4,
            "矩阵形式与四元数公式的最大相对偏差 {worst:e} 过大"
        );
    }

    /// 冻结体必须是零矩阵（无限惯量），否则睡眠体会被冲量推动。
    #[test]
    fn frozen_view_has_zero_inertia() {
        let v = BodyView::frozen();
        assert!(v.inv_i_world(Vec3::new(1.0, 2.0, 3.0)).length() == 0.0);
    }
}

#[cfg(test)]
mod cache_algorithm_tests {
    use super::*;

    fn contact(a: usize, b: usize, i: usize) -> SolverContact {
        SolverContact {
            a,
            b,
            normal: Vec3::Y,
            ra: Vec3::ZERO,
            rb: Vec3::ZERO,
            local_a: vec3((i % 4) as f32 * 0.005, 0.0, 0.0),
            local_b: Vec3::ZERO,
            tangent: [Vec3::X, Vec3::Z],
            k_normal: 1.0,
            k_tangent: [1.0; 2],
            normal_arm: [Vec3::ZERO; 2],
            tangent_arm: [[Vec3::ZERO; 2]; 2],
            precomputed: false,
            friction: 0.8,
            friction_tangent: [0.8; 2],
            rolling_friction: 0.0,
            friction_cone: false,
            restitution_bias: 0.0,
            speculative_bias: 0.0,
            penetration: 0.01,
            normal_impulse: i as f32 + 0.25,
            tangent_impulse: [i as f32 * 0.01, -0.0],
            feature_id: (i % 3) as u32,
            pseudo_impulse: 0.0,
        }
    }

    fn bits(c: CachedImpulse) -> Vec<u32> {
        let mut out = Vec::new();
        for v in [c.local_a, c.local_b] {
            out.extend([v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]);
        }
        out.extend([
            c.normal.to_bits(),
            c.tangent[0].to_bits(),
            c.tangent[1].to_bits(),
            c.feature_id,
        ]);
        out
    }

    #[test]
    fn global_normal_cached_reference_round_bits_and_invalidation() {
        let mut vs = vec![BodyView::frozen(); 5];
        vs[0].is_static = true;
        for v in &mut vs[1..4] {
            v.inv_m = 1.0;
            v.inv_i_world_mat = Mat3::IDENTITY;
            v.lin = -Vec3::Y;
        }
        let mut cs = Vec::new();
        for edge in 0..3 {
            for (x, z) in [(-0.2, -0.2), (0.2, -0.2), (-0.2, 0.2), (0.2, 0.2)] {
                let mut c = contact(edge, edge + 1, 0);
                c.ra = vec3(x, 0.25, z);
                c.rb = vec3(x, -0.25, z);
                c.k_normal = 2.0;
                c.normal_impulse = 0.0;
                c.tangent_impulse = [-0.0, 0.0];
                cs.push(c);
            }
        }
        let mut reference_cs = cs.clone();
        let mut reference_vs = vs.clone();
        let mut state = NormalSolveState::default();
        state.geometry.historical_local = true;
        state.geometry.force_reference = true;
        state.memory.copy_commit = true;
        let mut memory = NormalDirectionMemory::default();
        let mut warmed = None;
        for step in 0..2 {
            state.reset();
            memory.clear();
            if step == 1 {
                vs[2].inv_m = 0.75;
                vs[2].inv_i_world_mat = Mat3::from_cols(
                    vec3(1.2, 0.1, 0.0),
                    vec3(0.1, 0.9, 0.0),
                    vec3(0.0, 0.0, 1.1),
                );
                reference_vs = vs.clone();
                cs[4].normal = vec3(0.01, 1.0, 0.0).normalize();
                reference_cs = cs.clone();
            }
            for round in 0..20 {
                if round % 3 == 0 {
                    let dv = vec3(0.03, -0.02, 0.01);
                    vs[2].lin += dv;
                    reference_vs[2].lin += dv;
                    cs[round % 12].normal_impulse = 0.0;
                    reference_cs[round % 12].normal_impulse = 0.0;
                }
                solve_velocity_contacts_state(&mut cs, &mut vs, true, &mut state);
                solve_velocity_contacts_memory(
                    &mut reference_cs,
                    &mut reference_vs,
                    true,
                    &mut memory,
                );
                for (a, b) in vs.iter().zip(&reference_vs) {
                    let bits = |v: &BodyView| {
                        [v.lin.x, v.lin.y, v.lin.z, v.ang.x, v.ang.y, v.ang.z].map(f32::to_bits)
                    };
                    assert_eq!(bits(a), bits(b), "step={step} round={round}");
                }
                for (a, b) in cs.iter().zip(&reference_cs) {
                    assert_eq!(a.normal_impulse.to_bits(), b.normal_impulse.to_bits());
                    assert_eq!(
                        a.tangent_impulse.map(f32::to_bits),
                        b.tangent_impulse.map(f32::to_bits)
                    );
                }
                assert_eq!(
                    state
                        .memory
                        .p
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>(),
                    memory.p.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
                );
                assert_eq!(
                    state
                        .memory
                        .ap
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>(),
                    memory.ap.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
                );
                assert_eq!(state.memory.curvature.to_bits(), memory.curvature.to_bits());
                assert_eq!(state.memory.free, memory.free);
                let caps = state.work.capacities();
                if step == 0 && round == 3 {
                    warmed = Some(caps);
                }
                if step == 0 && round > 3 {
                    assert_eq!(Some(caps), warmed, "workspace capacity grew after warmup");
                }
            }
        }
        println!("ROUND195_REFERENCE 40rounds bits=true changedmask/joint_velocity/invalidation/holes=true workspacecapacitystable=true");
    }

    #[test]
    fn global_normal_memory_swap_actual_coarse_bits() {
        let mut vs = vec![BodyView::frozen(); 5];
        vs[0].is_static = true;
        for v in &mut vs[1..4] {
            v.inv_m = 1.0;
            v.inv_i_world_mat = Mat3::IDENTITY;
            v.lin = -Vec3::Y;
        }
        let mut cs = Vec::new();
        for edge in 0..3 {
            for (x, z) in [(-0.2, -0.2), (0.2, -0.2), (-0.2, 0.2), (0.2, 0.2)] {
                let mut c = contact(edge, edge + 1, 0);
                c.ra = vec3(x, 0.25, z);
                c.rb = vec3(x, -0.25, z);
                c.k_normal = 2.0;
                c.normal_impulse = 0.0;
                c.tangent_impulse = [-0.0, 0.0];
                cs.push(c);
            }
        }
        let mut reference_cs = cs.clone();
        let mut reference_vs = vs.clone();
        let mut state = NormalSolveState::default();

        let mut reference_state = NormalSolveState::default();
        reference_state.memory.copy_commit = true;
        reference_state.geometry.force_reference = true;
        state.geometry.force_reference = true;
        let mut warmed = None;
        for step in 0..2 {
            state.reset();
            reference_state.reset();
            if step == 1 {
                vs[2].inv_m = 0.75;
                vs[2].inv_i_world_mat = Mat3::from_cols(
                    vec3(1.2, 0.1, 0.0),
                    vec3(0.1, 0.9, 0.0),
                    vec3(0.0, 0.0, 1.1),
                );
                reference_vs = vs.clone();
                cs[4].normal = vec3(0.01, 1.0, 0.0).normalize();
                reference_cs = cs.clone();
            }
            for round in 0..20 {
                if round % 3 == 0 {
                    let dv = vec3(0.03, -0.02, 0.01);
                    vs[2].lin += dv;
                    reference_vs[2].lin += dv;
                    cs[round % 12].normal_impulse = 0.0;
                    reference_cs[round % 12].normal_impulse = 0.0;
                }
                solve_velocity_contacts_state(&mut cs, &mut vs, true, &mut state);
                solve_velocity_contacts_state(
                    &mut reference_cs,
                    &mut reference_vs,
                    true,
                    &mut reference_state,
                );
                let memory = &reference_state.memory;
                for (a, b) in vs.iter().zip(&reference_vs) {
                    let bits = |v: &BodyView| {
                        [v.lin.x, v.lin.y, v.lin.z, v.ang.x, v.ang.y, v.ang.z].map(f32::to_bits)
                    };
                    assert_eq!(bits(a), bits(b), "step={step} round={round}");
                }
                for (a, b) in cs.iter().zip(&reference_cs) {
                    assert_eq!(a.normal_impulse.to_bits(), b.normal_impulse.to_bits());
                    assert_eq!(
                        a.tangent_impulse.map(f32::to_bits),
                        b.tangent_impulse.map(f32::to_bits)
                    );
                }
                assert_eq!(
                    state
                        .memory
                        .p
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>(),
                    memory.p.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
                );
                assert_eq!(
                    state
                        .memory
                        .ap
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>(),
                    memory.ap.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
                );
                assert_eq!(state.memory.curvature.to_bits(), memory.curvature.to_bits());
                assert_eq!(state.memory.free, memory.free);
                let mut caps = [
                    state.work.p.capacity(),
                    state.work.ap.capacity(),
                    state.memory.p.capacity(),
                    state.memory.ap.capacity(),
                ];
                caps.sort();
                if step == 0 && round == 3 {
                    warmed = Some(caps);
                }
                if step == 0 && round > 3 {
                    assert_eq!(Some(caps), warmed, "workspace capacity grew after warmup");
                }
            }
        }
        println!("ROUND209 actual208coarse40round swap-vs-copy rawbits/pass fourcapacitysetstableafterwarm3");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)] // Preserve explicit independent numerical fixture indexing.
    fn global_normal_local_kernel_once() {
        let mut vs = vec![BodyView::frozen(); 2];
        for v in &mut vs {
            v.inv_m = 1.0;
            v.inv_i_world_mat = Mat3::IDENTITY;
        }
        let mut cs = Vec::new();
        for (x, z) in [(-0.2, -0.2), (0.2, -0.2), (-0.2, 0.2), (0.2, 0.2)] {
            let mut c = contact(0, 1, 0);
            c.ra = vec3(x, 0.25, z);
            c.rb = vec3(x, -0.25, z);
            cs.push(c);
        }
        LOCAL_KERNEL_CALLS.with(|c| c.set(0));
        let mut geometry = NormalGeometry::default();
        geometry.historical_local = true;
        geometry.force_reference = true;
        geometry.build(&cs, &vs);
        let n = LOCAL_KERNEL_CALLS.with(|c| c.get());
        println!("ROUND198 actual_matching_endpoint_kernels={n}");
        assert_eq!(n, 4 * 4 * 2);
    }

    #[test]
    fn global_normal_local_kernel_exact_bits() {
        let mut vs = vec![BodyView::frozen(); 3];
        for (i, v) in vs.iter_mut().enumerate() {
            v.inv_m = 0.5 + i as f32 * 0.3;
            let r = Mat3::from_axis_angle(vec3(0.3, 0.8, 0.2).normalize(), 0.2 + i as f32 * 0.4);
            v.inv_i_world_mat = r * Mat3::from_diagonal(vec3(0.8, 1.3, 2.2)) * r.transpose();
        }
        let mut cs = Vec::new();
        for i in 0..5 {
            let mut c = contact(i % 2, (i % 2) + 1, i);
            if i % 3 == 0 {
                std::mem::swap(&mut c.a, &mut c.b);
            }
            c.ra = vec3(-0.3 + i as f32 * 0.13, 0.2, -0.1);
            c.rb = vec3(0.4, -0.3 + i as f32 * 0.07, 0.2);
            c.normal = vec3(0.3, 0.7 + i as f32 * 0.03, -0.2).normalize();
            cs.push(c);
        }
        let points: Vec<_> = (0..cs.len()).collect();
        let kernel = LocalKernelCache::build(&points, &cs, &vs);
        let endpoints: Vec<_> = cs.iter().map(|c| NormalEndpoint::new(c, &vs)).collect();
        let sets = [points.clone(), vec![4, 2, 0], vec![1, 3], vec![3, 1, 4, 0]];
        for a in &sets {
            for b in &sets {
                assert_eq!(
                    coarse_cross(a, b, &cs, &vs).to_bits(),
                    coarse_cross_cached(a, b, &cs, &vs, &endpoints).to_bits()
                );
            }
        }
        println!("ROUND205 coarse16permutedsubsetkernelbits PASS");
        let probes = [
            [0.13, -0.27, 0.0, -0.0, 0.71],
            [0.37, 0.23, -0.19, 0.41, -0.53],
            [-0.0, 0.0, 0.61, -0.83, 0.17],
        ];
        for a in probes {
            for b in probes {
                assert_eq!(
                    kernel.weighted(&a, &b).to_bits(),
                    weighted_cross(&points, &a, &points, &b, &cs, &vs).to_bits()
                );
            }
        }
        println!("ROUND198 exactf64kernelbits nineprobes swappedendpoints/nonDiagI/unequalM/normal/offCOM/signedzero PASS");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)] // Preserve explicit independent numerical fixture indexing.
    fn global_normal_coarse_diagonal_spd_masked() {
        let mut vs = vec![BodyView::frozen(); 4];
        vs[0].is_static = true;
        for v in &mut vs[1..] {
            v.inv_m = 1.0;
            v.inv_i_world_mat = Mat3::IDENTITY;
        }
        let mut cs = Vec::new();
        for edge in 0..3 {
            let mut c = contact(edge, edge + 1, 0);
            c.k_normal = 2.0;
            cs.push(c);
        }
        let mut geometry = NormalGeometry::default();
        geometry.force_reference = true;
        geometry.build(&cs, &vs);
        assert!(geometry.locals.is_empty());
        for mask in 1..8 {
            let free: Vec<_> = (0..3).map(|i| mask & (1 << i) != 0).collect();
            for probe in 0..9 {
                let r: Vec<_> = (0..3)
                    .map(|i| {
                        if free[i] {
                            (0.17 + (i * 7 + probe * 3) as f64 * 0.37).sin()
                        } else {
                            0.0
                        }
                    })
                    .collect();
                let actual = geometry.apply(&cs, &vs, &r, &free).clone();
                let expected = chain_precondition(&cs, &vs, &r, &free);
                for (a, b) in actual.iter().zip(expected) {
                    assert!((a - b).abs() < 1e-12 * a.abs().max(b.abs()).max(1.0));
                }
                let energy = r.iter().zip(&actual).map(|(a, b)| a * b).sum::<f64>();
                assert!(energy > 0.0);
            }
        }
        println!("ROUND208 SPD maskeddiag+coarse 7masks9probes PASS");
    }

    #[test]
    fn global_normal_translation_gram_dense() {
        let mut vs = vec![BodyView::frozen(); 4];
        for (i, v) in vs.iter_mut().enumerate() {
            v.inv_m = if i == 0 { 0.0 } else { 0.5 + i as f32 * 0.3 };
            v.inv_i_world_mat = Mat3::IDENTITY;
        }
        for reversed in [false, true] {
            let mut cs = Vec::new();
            for i in 0..3 {
                let mut c = contact(i, i + 1, 0);
                if reversed {
                    std::mem::swap(&mut c.a, &mut c.b);
                }
                c.normal = vec3(0.1 + i as f32 * 0.03, 1.2, 0.2);
                cs.push(c);
            }
            for x in &cs {
                for y in &cs {
                    let mut expected = 0.0;
                    for (body, v) in vs.iter().enumerate() {
                        let sx = if x.a == body {
                            -1.0
                        } else if x.b == body {
                            1.0
                        } else {
                            0.0
                        };
                        let sy = if y.a == body {
                            -1.0
                        } else if y.b == body {
                            1.0
                        } else {
                            0.0
                        };
                        let nx = [x.normal.x, x.normal.y, x.normal.z].map(f64::from);
                        let ny = [y.normal.x, y.normal.y, y.normal.z].map(f64::from);
                        for k in 0..3 {
                            expected += (sx * nx[k]) * f64::from(v.inv_m) * (sy * ny[k]);
                        }
                    }
                    let actual = translation_cross214(x, y, &vs);
                    assert!(
                        (actual - expected).abs() < 1e-12 * expected.abs().max(1.0),
                        "actual={actual} expected={expected}"
                    );
                }
            }
        }
        println!("ROUND214 translational independent body-incidence Gram unequalmass/nonunit/reversedendpoints PASS");
    }

    #[test]
    #[allow(clippy::needless_range_loop)] // Preserve explicit independent numerical fixture indexing.
    fn global_normal_translation_masked_dense_solve() {
        fn solve(mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> [f64; 3] {
            for k in 0..3 {
                let pivot = a[k][k];
                assert!(pivot > 0.0);
                for i in k + 1..3 {
                    let factor = a[i][k] / pivot;
                    for j in k..3 {
                        a[i][j] -= factor * a[k][j];
                    }
                    b[i] -= factor * b[k];
                }
            }
            let mut x = [0.0; 3];
            for i in (0..3).rev() {
                let mut v = b[i];
                for j in i + 1..3 {
                    v -= a[i][j] * x[j];
                }
                x[i] = v / a[i][i];
            }
            x
        }
        for reversed in [false, true] {
            let mut vs = vec![BodyView::frozen(); 4];
            vs[0].is_static = true;
            for (i, v) in vs.iter_mut().enumerate().skip(1) {
                v.inv_m = 0.4 + i as f32 * 0.3;
                v.inv_i_world_mat = Mat3::from_cols(
                    vec3(1.2, 0.1, 0.0),
                    vec3(0.1, 0.8, 0.0),
                    vec3(0.0, 0.0, 1.4),
                );
            }
            let mut cs = Vec::new();
            for edge in 0..3 {
                for point in 0..4 {
                    let mut c = contact(edge, edge + 1, 0);
                    c.normal = vec3(0.01 * edge as f32, 1.1, 0.02);
                    c.ra = vec3(point as f32 * 0.12, 0.3, -0.2);
                    c.rb = vec3(-0.1, -0.2, point as f32 * 0.13);
                    c.k_normal = 1.0 + (edge * 4 + point) as f32 * 0.2;
                    if reversed {
                        std::mem::swap(&mut c.a, &mut c.b);
                        std::mem::swap(&mut c.ra, &mut c.rb);
                    }
                    cs.push(c);
                }
            }
            let mut gram = [[0.0; 3]; 3];
            for e in 0..3 {
                for f in 0..3 {
                    for (body, v) in vs.iter().enumerate() {
                        let x = &cs[e * 4];
                        let y = &cs[f * 4];
                        let sx = if x.a == body {
                            -1.0
                        } else if x.b == body {
                            1.0
                        } else {
                            0.0
                        };
                        let sy = if y.a == body {
                            -1.0
                        } else if y.b == body {
                            1.0
                        } else {
                            0.0
                        };
                        let nx = [x.normal.x, x.normal.y, x.normal.z].map(f64::from);
                        let ny = [y.normal.x, y.normal.y, y.normal.z].map(f64::from);
                        for k in 0..3 {
                            gram[e][f] += (sx * nx[k]) * f64::from(v.inv_m) * (sy * ny[k]);
                        }
                    }
                }
            }
            let mut geometry = NormalGeometry::default();
            for mask in 0..16 {
                let free: Vec<_> = (0..12).map(|i| mask & (1 << (i % 4)) != 0).collect();
                for probe in 0..9 {
                    let r: Vec<_> = (0..12)
                        .map(|i| 0.17 + (i * 3 + probe * 7) as f64 * 0.11)
                        .collect();
                    let mut rhs = [0.0; 3];
                    for e in 0..3 {
                        for j in 0..4 {
                            let i = e * 4 + j;
                            if free[i] {
                                rhs[e] += r[i] / 4.0;
                            }
                        }
                    }
                    let x = solve(gram, rhs);
                    let actual = geometry.apply(&cs, &vs, &r, &free).clone();
                    assert_eq!(geometry.chains.len(), 1);
                    assert!(geometry.endpoint.is_empty());
                    let mut energy = 0.0;
                    for i in 0..12 {
                        let expected = if free[i] {
                            r[i] / f64::from(cs[i].k_normal) + x[i / 4] / 4.0
                        } else {
                            0.0
                        };
                        assert!((actual[i]-expected).abs()<=1e-12*expected.abs().max(1.0),"reverse={reversed} mask={mask} probe={probe} i={i} actual={} expected={expected}",actual[i]);
                        energy += r[i] * actual[i];
                    }
                    if mask != 0 {
                        assert!(energy > 0.0);
                    }
                }
            }
        }
        println!("ROUND214 actualT masked denseGauss16masks9probes/reversedroot/unequalmass/nonunit/nonDiagI/offCOM SPD PASS");
    }

    #[test]
    fn global_normal_setup_once_per_substep() {
        let mut vs = vec![BodyView::frozen(); 4];
        vs[0].is_static = true;
        for v in &mut vs[1..] {
            v.inv_m = 1.0;
            v.inv_i_world_mat = Mat3::IDENTITY;
            v.lin = -Vec3::Y;
        }
        let mut cs = Vec::new();
        for edge in 0..3 {
            for (x, z) in [(-0.2, -0.2), (0.2, -0.2), (-0.2, 0.2), (0.2, 0.2)] {
                let mut c = contact(edge, edge + 1, 0);
                c.ra = vec3(x, 0.25, z);
                c.rb = vec3(x, -0.25, z);
                c.k_normal = 2.0;
                c.normal_impulse = 0.0;
                c.tangent_impulse = [0.0; 2];
                c.friction = 0.0;
                c.friction_tangent = [0.0; 2];
                cs.push(c);
            }
        }
        let mut state = NormalSolveState::default();
        NORMAL_SETUP_COUNTS.with(|c| c.set([0; 4]));
        for _ in 0..20 {
            solve_velocity_contacts_state(&mut cs, &mut vs, true, &mut state);
        }
        let counts = NORMAL_SETUP_COUNTS.with(|c| c.get());
        println!("ROUND195 actual_geometry_force_local_ap={counts:?}");
        assert_eq!(counts[0], 1);
        assert_eq!(counts[1], 1);
        assert_eq!(counts[2], 0, "208 omits local torque factors");
        assert!(counts[3] <= 20);
    }

    #[test]
    fn global_normal_rejected_overflow_clears_memory() {
        let mut vs = vec![BodyView::frozen(); 2];
        for v in &mut vs {
            v.inv_m = 1.0;
            v.inv_i_world_mat = Mat3::IDENTITY;
        }
        vs[1].lin = -Vec3::Y * 1e38;
        let before = vs.clone();
        let mut c = contact(0, 1, 0);
        c.ra = Vec3::ZERO;
        c.rb = Vec3::ZERO;
        c.normal = Vec3::Y;
        c.k_normal = 2.0;
        c.normal_impulse = f32::MAX;
        c.tangent_impulse = [0.0; 2];
        c.friction = 0.0;
        c.friction_tangent = [0.0; 2];
        c.rolling_friction = 0.0;
        let mut cs = vec![c];
        let mut memory = NormalDirectionMemory::default();
        solve_velocity_contacts_memory(&mut cs, &mut vs, true, &mut memory);
        assert_eq!(cs[0].normal_impulse.to_bits(), f32::MAX.to_bits());
        for (a, b) in vs.iter().zip(before) {
            assert_eq!(a.lin, b.lin);
            assert_eq!(a.ang, b.ang);
        }
        println!(
            "ROUND194_OVERFLOW unchanged=true retained_p={:?} retained_ap={:?}",
            memory.p, memory.ap
        );
        assert!(
            memory.p.is_empty() && memory.ap.is_empty(),
            "rejected update retained an unapplied direction"
        );
    }

    #[test]
    fn global_normal_fresh_friction_coupled_oracle() {
        let mut vs = vec![BodyView::frozen(); 2];
        for v in &mut vs {
            v.inv_m = 1.0;
            v.inv_i_world_mat = Mat3::IDENTITY;
        }
        vs[1].lin = vec3(4.0, -2.0, 0.0);
        let mut c = contact(0, 1, 0);
        c.ra = vec3(1.0, 1.0, 0.0);
        c.rb = Vec3::Y;
        c.k_normal = 3.0;
        c.k_tangent = [4.0, 5.0];
        c.friction = 1.0;
        c.friction_tangent = [1.0; 2];
        c.normal_impulse = 2.0;
        c.tangent_impulse = [0.0; 2];
        let mut cs = vec![c];
        let mut memory = NormalDirectionMemory::default();
        solve_velocity_contacts_memory(&mut cs, &mut vs, true, &mut memory);
        assert!((cs[0].normal_impulse - (2.0 + 1.0 / 3.0)).abs() < 2e-6);
        assert!((cs[0].tangent_impulse[0] + 1.0).abs() < 1e-6);
        assert!((vs[0].lin + vs[1].lin - vec3(4.0, -2.0, 0.0)).length() < 2e-6);
    }
    #[test]
    #[allow(clippy::needless_range_loop)] // Preserve explicit independent numerical fixture indexing.
    fn global_normal_dense_spatial_operator_and_momentum() {
        fn arr(v: Vec3) -> [f64; 3] {
            [v.x as f64, v.y as f64, v.z as f64]
        }
        fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        }
        fn inverse(a: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
            let mut m = [[0.0; 6]; 3];
            for i in 0..3 {
                for j in 0..3 {
                    m[i][j] = a[i][j];
                }
                m[i][3 + i] = 1.0;
            }
            for k in 0..3 {
                let d = m[k][k];
                assert!(d > 0.0);
                for j in 0..6 {
                    m[k][j] /= d;
                }
                for i in 0..3 {
                    if i != k {
                        let f = m[i][k];
                        for j in 0..6 {
                            m[i][j] -= f * m[k][j];
                        }
                    }
                }
            }
            let mut out = [[0.0; 3]; 3];
            for i in 0..3 {
                for j in 0..3 {
                    out[i][j] = m[i][3 + j];
                }
            }
            out
        }
        let centers = [
            vec3(-0.7, 0.3, 0.2),
            vec3(0.4, -0.5, 0.1),
            vec3(0.2, 0.6, -0.4),
        ];
        let masses = [0.5, 1.25, 0.8];
        let mut vs = vec![BodyView::frozen(); 3];
        let mut w = [[[0.0f64; 6]; 6]; 3];
        for b in 0..3 {
            let r = Mat3::from_axis_angle(vec3(0.3, 0.7, 0.2).normalize(), 0.4 + b as f32 * 0.3);
            let tensor =
                r * Mat3::from_diagonal(vec3(0.7 + b as f32 * 0.2, 1.3, 2.1)) * r.transpose();
            vs[b].inv_m = masses[b];
            vs[b].inv_i_world_mat = tensor;
            let a = tensor.to_cols_array();
            for i in 0..3 {
                w[b][i][i] = masses[b] as f64;
                for j in 0..3 {
                    w[b][3 + i][3 + j] = a[j * 3 + i] as f64;
                }
            }
        }
        let definitions = [
            (0, 1, vec3(0.1, 0.2, 0.3), vec3(0.3, 0.8, -0.2)),
            (1, 2, vec3(-0.2, 0.1, 0.4), vec3(-0.4, 0.2, 0.7)),
            (0, 2, vec3(0.3, -0.1, 0.2), vec3(0.6, -0.5, 0.2)),
            (0, 1, vec3(-0.4, 0.3, -0.2), vec3(-0.1, 0.4, 0.9)),
        ];
        let mut cs = Vec::new();
        let mut j = [[[0.0f64; 6]; 3]; 4];
        for (i, (a, b, q, n)) in definitions.into_iter().enumerate() {
            let mut c = contact(a, b, i);
            c.normal = n.normalize();
            c.ra = q - centers[a];
            c.rb = q - centers[b];
            for (body, sign, arm) in [(a, -1.0, c.ra), (b, 1.0, c.rb)] {
                let n = arr(c.normal);
                let torque = cross(arr(arm), n);
                for k in 0..3 {
                    j[i][body][k] = sign * n[k];
                    j[i][body][3 + k] = sign * torque[k];
                }
            }
            cs.push(c);
        }
        let mut dense = [[0.0; 4]; 4];
        let mut absolute = [[0.0; 4]; 4];
        for i in 0..4 {
            for k in 0..4 {
                for b in 0..3 {
                    for u in 0..6 {
                        for v in 0..6 {
                            let t = j[i][b][u] * w[b][u][v] * j[k][b][v];
                            dense[i][k] += t;
                            absolute[i][k] += t.abs();
                        }
                    }
                }
            }
        }
        let probes = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.7, -1.2, 0.4, -0.9],
        ];
        for p in probes {
            let got = normal_product(&cs, &vs, &p);
            for i in 0..4 {
                let expected = (0..4).map(|k| dense[i][k] * p[k]).sum::<f64>();
                let bound = 128.0
                    * f32::EPSILON as f64
                    * (0..4)
                        .map(|k| absolute[i][k] * p[k].abs())
                        .sum::<f64>()
                        .max(1.0);
                assert!(
                    (got[i] - expected).abs() <= bound,
                    "row={i} got={} expected={expected} bound={bound}",
                    got[i]
                );
            }
        }
        let impulses = [0.7f64, -1.2, 0.4, -0.9];
        for (c, p) in cs.iter().zip(impulses) {
            let (a, b) = split_body_pair(&mut vs, c.a, c.b).unwrap();
            pair_apply_impulse(a, b, c.ra, c.rb, c.normal * p as f32);
        }
        let mut momentum = [0.0; 3];
        let mut angular = [0.0; 3];
        for b in 0..3 {
            let actual = [
                vs[b].lin.x,
                vs[b].lin.y,
                vs[b].lin.z,
                vs[b].ang.x,
                vs[b].ang.y,
                vs[b].ang.z,
            ]
            .map(f64::from);
            for u in 0..6 {
                let expected = (0..4)
                    .map(|i| {
                        (0..6)
                            .map(|v| w[b][u][v] * j[i][b][v] * impulses[i])
                            .sum::<f64>()
                    })
                    .sum::<f64>();
                assert!(
                    (actual[u] - expected).abs()
                        < 128.0 * f32::EPSILON as f64 * expected.abs().max(1.0)
                );
            }
            let mut it = [[0.0; 3]; 3];
            for i in 0..3 {
                for k in 0..3 {
                    it[i][k] = w[b][3 + i][3 + k];
                }
            }
            let inertia = inverse(it);
            let linear = arr(vs[b].lin).map(|x| x / masses[b] as f64);
            let orbital = cross(arr(centers[b]), linear);
            for i in 0..3 {
                momentum[i] += linear[i];
                angular[i] +=
                    orbital[i] + (0..3).map(|k| inertia[i][k] * actual[3 + k]).sum::<f64>();
            }
        }
        for x in momentum.into_iter().chain(angular) {
            assert!(
                x.abs() < 128.0 * f32::EPSILON as f64,
                "momentum residual={x}"
            );
        }
    }

    #[test]
    fn global_normal_rolling_two_round_stall() {
        for rolling in [0.0, 0.2] {
            let mut vs = vec![BodyView::frozen(); 2];
            vs[0].is_static = true;
            vs[1].inv_m = 1.0;
            vs[1].inv_i_world_mat = Mat3::IDENTITY;
            vs[1].lin = -Vec3::Y;
            vs[1].ang = Vec3::X * 3.0;
            let mut c = contact(0, 1, 0);
            c.ra = Vec3::ZERO;
            c.rb = -Vec3::Y;
            c.friction = 0.0;
            c.friction_tangent = [0.0; 2];
            c.rolling_friction = rolling;
            c.normal_impulse = 0.0;
            c.tangent_impulse = [0.0; 2];
            c.k_normal = 1.0;
            let mut cs = vec![c];
            let mut memory = NormalDirectionMemory::default();
            for _ in 0..2 {
                solve_velocity_contacts_memory(&mut cs, &mut vs, true, &mut memory);
            }
            assert_eq!(cs[0].normal_impulse, 1.0);
            assert_eq!(vs[1].lin, Vec3::ZERO);
            let expected = if rolling == 0.0 { 3.0 } else { 2.8 };
            assert!(
                (vs[1].ang.x - expected).abs() < 1e-6,
                "rolling={rolling} spin={:?}",
                vs[1].ang
            );
        }
    }

    #[test]
    fn normal_direction_memory_changed_rhs_restart() {
        let mut memory = NormalDirectionMemory {
            #[cfg(test)]
            copy_commit: false,
            p: vec![1.0, 0.0],
            ap: vec![4.0, 1.0],
            curvature: 4.0,
            free: vec![true, true],
        };
        let (p, retain) = memory_feasible_direction(
            &[1.0, 1.0],
            &[1.0, -1.0],
            vec![-1.0, 1.0],
            &[4.0, 3.0],
            &[true, true],
            &mut memory,
        );
        assert!(p[0] - p[1] < 0.0);
        assert!(retain);
        memory = NormalDirectionMemory {
            #[cfg(test)]
            copy_commit: false,
            p: vec![0.0, 1.0],
            ap: vec![1.0, 3.0],
            curvature: 3.0,
            free: vec![true, true],
        };
        let (p, retain) = memory_feasible_direction(
            &[0.0, 1.0],
            &[-1.0, 1.0],
            vec![-1.0, -1.0],
            &[1.0, 1.0],
            &[true, true],
            &mut memory,
        );
        assert!(!retain);
        assert!(p[0] >= 0.0);
        assert!(-p[0] + p[1] < 0.0);
        assert!(memory.p.is_empty());
    }

    #[test]
    fn normal_direction_memory_two_step_spd() {
        let mut g = vec![-1.0, -2.0];
        let mut memory = NormalDirectionMemory::default();
        for _ in 0..2 {
            let z = vec![-g[0] / 4.0, -g[1] / 3.0];
            let p = remembered_direction(z, &memory);
            let ap = vec![4.0 * p[0] + p[1], p[0] + 3.0 * p[1]];
            let curvature = p.iter().zip(&ap).map(|(a, b)| a * b).sum::<f64>();
            let alpha = -g.iter().zip(&p).map(|(a, b)| a * b).sum::<f64>() / curvature;
            for i in 0..2 {
                g[i] += alpha * ap[i];
            }
            memory = NormalDirectionMemory {
                copy_commit: false,
                p,
                ap,
                curvature,
                free: vec![true, true],
            };
        }
        assert!(
            g.iter().all(|x| x.abs() < 1e-12),
            "two-step SPD residual={g:?}"
        );
    }

    #[test]
    fn global_force_local_torque_three_interface_dense_masked() {
        let mut vs = vec![BodyView::frozen(); 4];
        vs[0].is_static = true;
        for (i, v) in vs.iter_mut().enumerate().skip(1) {
            v.inv_m = 1.0 / (i as f32);
            v.inv_i_world_mat = Mat3::IDENTITY;
        }
        let patches = [
            vec![(-0.25, -0.25), (0.25, -0.25), (0.25, 0.25), (-0.25, 0.25)],
            vec![(-0.25, 0.0), (0.25, 0.0)],
            vec![(0.0, 0.0)],
        ];
        let mut cs = Vec::new();
        let mut groups = Vec::new();
        for (edge, patch) in patches.iter().enumerate() {
            let mut group = Vec::new();
            for (i, (x, z)) in patch.iter().enumerate() {
                let mut c = contact(edge, edge + 1, i);
                c.normal = Vec3::Y;
                c.ra = vec3(*x, 0.25, *z);
                c.rb = vec3(*x, -0.25, *z);
                c.k_normal = 2.0;
                group.push(cs.len());
                cs.push(c);
            }
            groups.push(group);
        }
        let r = vec![0.3, -0.7, 1.1, 0.2, 0.8, -0.4, 0.5];
        let free = vec![true, false, true, true, true, false, true];
        let got = local_torque_correction(&cs, &vs, &r, &free);
        let mut expected = vec![0.0; cs.len()];
        for points in &groups {
            let basis = wrench_basis(&cs, points);
            let rank = basis.len() - 1;
            assert!(rank <= 2);
            if rank == 0 {
                continue;
            }
            let mut a = vec![vec![0.0; points.len()]; points.len()];
            for (i, ii) in points.iter().enumerate() {
                for (j, jj) in points.iter().enumerate() {
                    let x = cs[*ii];
                    let y = cs[*jj];
                    a[i][j] = f64::from(vs[x.a].inv_m + vs[x.b].inv_m)
                        + (if x.a == 0 { 1.0 } else { 2.0 })
                            * (f64::from(x.ra.x) * f64::from(y.ra.x)
                                + f64::from(x.ra.z) * f64::from(y.ra.z));
                }
            }
            let mut t = [[0.0; 2]; 2];
            let mut b = [0.0; 2];
            for i in 0..rank {
                for (k, index) in points.iter().enumerate() {
                    if free[*index] {
                        b[i] += basis[i + 1][k] * r[*index];
                    }
                    for j in 0..rank {
                        for l in 0..points.len() {
                            t[i][j] += basis[i + 1][k] * a[k][l] * basis[j + 1][l];
                        }
                    }
                }
            }
            let x = if rank == 1 {
                [b[0] / t[0][0], 0.0]
            } else {
                let det = t[0][0] * t[1][1] - t[0][1] * t[1][0];
                [
                    (b[0] * t[1][1] - b[1] * t[0][1]) / det,
                    (b[1] * t[0][0] - b[0] * t[1][0]) / det,
                ]
            };
            for (k, index) in points.iter().enumerate() {
                if free[*index] {
                    for i in 0..rank {
                        expected[*index] += basis[i + 1][k] * x[i];
                    }
                }
            }
        }
        let error = got
            .iter()
            .zip(&expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        assert!(error < 1e-6, "local additive reference mismatch {error}");
        let force = chain_precondition(&cs, &vs, &r, &free);
        let actual: Vec<_> = force.iter().zip(&got).map(|(a, b)| a + b).collect();
        for i in 0..cs.len() {
            assert!((actual[i] - force[i] - expected[i]).abs() < 1e-6);
            if !free[i] {
                assert_eq!(actual[i], 0.0);
            }
        }
        assert!(actual.iter().zip(&r).map(|(a, b)| a * b).sum::<f64>() > 0.0);
    }

    #[test]
    fn wrench_basis_normal_offset_invariance() {
        let mut vs = vec![BodyView::frozen(); 3];
        vs[0].is_static = true;
        for v in &mut vs[1..] {
            v.inv_m = 1.0;
            v.inv_i_world_mat = Mat3::IDENTITY;
        }
        let mut cs = Vec::new();
        for edge in 0..2 {
            for (i, (x, z)) in [(-0.25, -0.25), (0.25, -0.25), (0.25, 0.25), (-0.25, 0.25)]
                .into_iter()
                .enumerate()
            {
                let mut c = contact(edge, edge + 1, i);
                c.normal = Vec3::Y;
                c.ra = vec3(x, 0.25, z);
                c.rb = vec3(x, -0.25, z);
                c.k_normal = 2.0;
                cs.push(c);
            }
        }
        let shifted: Vec<_> = cs
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut c = *c;
                let offset = Vec3::Y * (i as f32 * 0.125);
                c.ra += offset;
                c.rb += offset;
                c
            })
            .collect();
        let probe = vec![1.0, -0.5, 0.25, 0.75, -0.25, 0.5, 1.0, -0.75];
        assert_eq!(
            normal_product(&cs, &vs, &probe),
            normal_product(&shifted, &vs, &probe)
        );
        assert_eq!(
            wrench_basis(&cs, &[0, 1, 2, 3]),
            wrench_basis(&shifted, &[0, 1, 2, 3])
        );
        let columns = vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]];
        let free = vec![true; 8];
        // Independent Jacobian rows and arbitrary spin normal velocity.
        for (c, m) in cs.iter().zip(&shifted) {
            let jac = |r: Vec3| [0.0f64, 1.0, 0.0, -f64::from(r.z), 0.0, f64::from(r.x)];
            assert_eq!(jac(c.ra), jac(m.ra));
            assert_eq!(jac(c.rb), jac(m.rb));
            let wa = vec3(0.25, 0.5, -0.75);
            let wb = vec3(-0.5, 0.25, 0.125);
            assert_eq!(
                (wb.cross(c.rb) - wa.cross(c.ra)).y,
                (wb.cross(m.rb) - wa.cross(m.ra)).y
            );
        }
        let original = wrench_chain_correction(&columns, &cs, &vs, &probe, &free).unwrap();
        let moved = wrench_chain_correction(&columns, &shifted, &vs, &probe, &free)
            .expect("normal offsets must retain valid normal wrench coarse span");
        assert_eq!(original, moved);
    }

    #[test]
    #[allow(clippy::needless_range_loop)] // Preserve explicit independent numerical fixture indexing.
    fn wrench_basis_two_interface_dense_reference() {
        let mut vs = vec![BodyView::frozen(); 3];
        vs[0].is_static = true;
        for v in &mut vs[1..] {
            v.inv_m = 1.0;
            v.inv_i_world_mat = Mat3::IDENTITY;
        }
        let corners = [[-0.7f32, -0.2f32], [0.5, -0.2], [0.5, 0.6], [-0.7, 0.6]];
        let mut cs = Vec::new();
        for edge in 0..2 {
            for (i, [x, z]) in corners.iter().enumerate() {
                let mut c = contact(edge, edge + 1, i);
                c.normal = Vec3::Y;
                c.ra = vec3(*x, 0.25, *z);
                c.rb = vec3(*x, -0.25, *z);
                c.k_normal = 2.0;
                cs.push(c);
            }
        }
        let columns0 = wrench_basis(&cs, &[0, 1, 2, 3]);
        let columns1 = wrench_basis(&cs, &[4, 5, 6, 7]);
        assert_eq!(columns0.len(), 3);
        assert_eq!(columns1.len(), 3);
        for q in &columns0 {
            assert!((q.iter().map(|x| x * x).sum::<f64>() - 1.0).abs() < 1e-10);
        }
        for i in 0..3 {
            for j in 0..i {
                assert!(
                    columns0[i]
                        .iter()
                        .zip(&columns0[j])
                        .map(|(a, b)| a * b)
                        .sum::<f64>()
                        .abs()
                        < 1e-10
                );
            }
        }
        let residual = vec![1.0, -0.3, 0.7, -0.2, 0.2, 0.8, -0.6, 0.4];
        let free = vec![true; 8];
        let correction = wrench_chain_correction(
            &[vec![0, 1, 2, 3], vec![4, 5, 6, 7]],
            &cs,
            &vs,
            &residual,
            &free,
        )
        .expect("full wrench reference must qualify");
        let z: Vec<_> = correction
            .iter()
            .zip(&residual)
            .zip(&cs)
            .map(|((c, r), contact)| c + r / f64::from(contact.k_normal))
            .collect();
        // Independent dense normal Jacobian rows: y translation, x/z torque.
        let mut a = vec![vec![0.0f64; 8]; 8];
        for i in 0..8 {
            for j in 0..8 {
                for body in 1..3 {
                    let jac = |c: &SolverContact| {
                        let sign = if c.a == body {
                            -1.0
                        } else if c.b == body {
                            1.0
                        } else {
                            0.0
                        };
                        let r = if c.a == body { c.ra } else { c.rb };
                        [sign, sign * (-r.z as f64), sign * (r.x as f64)]
                    };
                    let x = jac(&cs[i]);
                    let y = jac(&cs[j]);
                    a[i][j] += x.iter().zip(y).map(|(a, b)| a * b).sum::<f64>();
                }
            }
        }
        let mut p = vec![vec![0.0; 6]; 8];
        for i in 0..4 {
            for k in 0..3 {
                p[i][k] = columns0[k][i];
                p[4 + i][3 + k] = columns1[k][i];
            }
        }
        let correction: Vec<_> = z.iter().zip(&residual).map(|(z, r)| z - r / 2.0).collect();
        let mut worst = 0.0f64;
        for k in 0..6 {
            let expected = (0..8).map(|i| p[i][k] * residual[i]).sum::<f64>();
            let got = (0..8)
                .map(|i| p[i][k] * (0..8).map(|j| a[i][j] * correction[j]).sum::<f64>())
                .sum::<f64>();
            worst = worst.max((expected - got).abs());
        }
        assert!(worst < 1e-5, "projected dense residual={worst}");
        assert!(z.iter().zip(residual).map(|(a, b)| a * b).sum::<f64>() > 0.0);
        assert_eq!(wrench_basis(&cs, &[0, 1]).len(), 2);
    }

    #[test]
    fn global_projection_spd_boundary_and_release() {
        let lambda = [0.0, 1.0];
        let g = [-1.0, 1.0];
        let p = feasible_direction(&lambda, &g, vec![-1.0, -3.0], &[1.0, 1.0]);
        assert_eq!(p, vec![0.0, -3.0]);
        let alpha: f64 = 1.0 / 3.0;
        let next = [lambda[0] + alpha * p[0], lambda[1] + alpha * p[1]];
        assert_eq!(next, [0.0, 0.0]);
        assert!(0.5 * (next[0] * next[0] + next[1] * next[1]) - next[0] < 0.5);
        assert_eq!(
            feasible_direction(&[0.0, 0.0], &[-1.0, 1.0], vec![1.0, -1.0], &[1.0, 1.0]),
            vec![1.0, 0.0]
        );
        assert_eq!(
            feasible_direction(&[1.0], &[1.0], vec![-1.0], &[1.0]),
            vec![-1.0]
        );
    }

    #[test]
    fn global_projection_product_dense_and_true_mass() {
        let mut bodies = vec![BodyView::frozen(); 2];
        bodies[0].inv_m = 0.5;
        bodies[1].inv_m = 1.0;
        bodies[0].inv_i_world_mat = Mat3::IDENTITY;
        bodies[1].inv_i_world_mat = Mat3::IDENTITY;
        let mut c = contact(0, 1, 0);
        c.normal = Vec3::Y;
        c.ra = vec3(0.2, 0.0, 0.3);
        c.rb = vec3(-0.1, 0.0, 0.4);
        let got = normal_product(&[c], &bodies, &[2.0])[0];
        let expected = 2.0 * (1.5 + 0.2 * 0.2 + 0.3 * 0.3 + 0.1 * 0.1 + 0.4 * 0.4);
        assert!((got - expected).abs() < 1e-6);
        let (a, b) = split_body_pair(&mut bodies, 0, 1).unwrap();
        pair_apply_impulse(a, b, c.ra, c.rb, Vec3::Y * 2.0);
        assert!((a.lin / a.inv_m + b.lin / b.inv_m).length() < 1e-6);
        assert!(
            (f64::from(pair_relative_velocity(a, b, c.ra, c.rb).dot(c.normal)) - got).abs() < 1e-6
        );
    }

    #[test]
    fn cache_rebuild_and_lookup_match_reference_multiframe() {
        CACHE_REFERENCE.set(0);
        CACHE_FAST_REBUILDS.set(0);
        let mut actual = ContactCache::default();
        let mut reference = ContactCache::default();
        for frame in 0..40 {
            let mut contacts = Vec::new();
            for a in 0..32 {
                for i in 0..16 {
                    contacts.push(contact(a, a + 1, i + frame));
                }
            }
            // Duplicate feature IDs, equal-distance anchors and opposite pair order.
            contacts.push(contact(1, 0, frame));
            if frame % 2 == 0 {
                contacts.sort_by_key(|c| (c.a, c.b));
            } else {
                contacts.rotate_left(17);
                contacts.reverse();
            }
            // Retain unusual bits: selection must not silently normalize them.
            contacts[7].local_a.x = f32::NAN;
            contacts[9].normal_impulse = -0.0;
            actual.rebuild(&contacts);
            reference.rebuild_unsorted(&contacts);
            assert_eq!(actual.keys, reference.keys);
            assert_eq!(actual.ranges, reference.ranges);
            assert_eq!(
                actual
                    .impulses
                    .iter()
                    .copied()
                    .map(bits)
                    .collect::<Vec<_>>(),
                reference
                    .impulses
                    .iter()
                    .copied()
                    .map(bits)
                    .collect::<Vec<_>>()
            );
            for a in 0..34 {
                for id in [0, 1, 2, 99] {
                    for x in [0.0, 0.0025, 0.0075, 0.02, 0.5, f32::NAN] {
                        let anchor = vec3(x, 0.0, 0.0);
                        let got = ContactCache::lookup_in_list(
                            actual.list(a, a + 1),
                            anchor,
                            Vec3::ZERO,
                            id,
                            0.02,
                        );
                        let expected = reference.lookup(a, a + 1, anchor, Vec3::ZERO, id, 0.02);
                        assert_eq!(
                            got.map(bits),
                            expected.map(bits),
                            "frame={frame} a={a} id={id} x={x}"
                        );
                    }
                }
            }
        }
        assert_eq!(
            CACHE_FAST_REBUILDS.get(),
            20,
            "both sorted fast path and unsorted fallback exercised"
        );
        actual.clear();
        assert!(actual.impulses.is_empty());
        actual.rebuild(&[]);
        assert!(actual.keys.is_empty());
    }

    #[test]
    fn cache_lookup_retains_feature_priority_and_first_ties() {
        let make = |x, normal, feature_id| CachedImpulse {
            local_a: vec3(x, 0.0, 0.0),
            local_b: Vec3::ZERO,
            normal,
            tangent: [-0.0, 0.0],
            feature_id,
        };
        let list = [make(-0.01, 1.0, 5), make(0.01, 2.0, 5), make(1.0, 3.0, 8)];
        // Exact feature matching wins even outside the geometric tolerance.
        assert_eq!(
            ContactCache::lookup_in_list(&list, Vec3::ZERO, Vec3::ZERO, 8, 0.02)
                .unwrap()
                .normal,
            3.0
        );
        // Duplicate features and equally near fallbacks retain the first entry.
        for id in [0, 5, 99] {
            assert_eq!(
                ContactCache::lookup_in_list(&list, Vec3::ZERO, Vec3::ZERO, id, 0.02)
                    .unwrap()
                    .normal,
                1.0
            );
        }
        let boundary = [make(0.02, 4.0, 0)];
        assert!(ContactCache::lookup_in_list(&boundary, Vec3::ZERO, Vec3::ZERO, 0, 0.02).is_some());
        assert!(ContactCache::lookup_in_list(
            &boundary,
            vec3(-0.001, 0.0, 0.0),
            Vec3::ZERO,
            0,
            0.02
        )
        .is_none());
    }

    #[test]
    fn cache_prepare_reuses_pair_search_and_preserves_warm_start_bits() {
        CACHE_REFERENCE.set(0);
        let mut cache = ContactCache::default();
        let contacts: Vec<_> = (0..16).map(|i| contact(0, 1, i)).collect();
        cache.rebuild(&contacts);
        let body = crate::BodyBuilder::dynamic(crate::Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .build();
        let views = [BodyView::from_body(&body), BodyView::from_body(&body)];
        let material = ContactMaterial {
            dynamic_friction: 0.8,
            static_friction: 0.9,
            rolling_friction: 0.0,
            restitution: 0.0,
            friction_combine: None,
            restitution_combine: None,
        };
        let mut m = ManifoldEntry {
            a: 0,
            b: 1,
            points: SmallVec::new(),
        };
        for i in 0..4 {
            m.points.push(ContactPoint {
                position: vec3(i as f32 * 0.005, 0.0, 0.0),
                normal: Vec3::Y,
                penetration: 0.01,
                feature_id: 0,
            });
        }
        let mut opt = ContactSolver::default();
        let mut old = ContactSolver::default();
        let mut cfg = SolverConfig::default();
        CACHE_PAIR_SEARCHES.set(0);
        for frame in 0..40 {
            for p in &mut m.points {
                p.position.x += 0.00001;
            }
            opt.prepare(
                &views,
                &[material; 2],
                &[],
                std::slice::from_ref(&m),
                &cache,
                &cfg,
                1.0 / 60.0,
                frame % 2 == 0,
            );
            old.prepare_reference(
                &views,
                &[material; 2],
                &[],
                std::slice::from_ref(&m),
                &cache,
                &cfg,
                1.0 / 60.0,
                frame % 2 == 0,
            );
            assert_eq!(opt.contacts.len(), old.contacts.len());
            for (a, b) in opt.contacts.iter().zip(&old.contacts) {
                assert_eq!(a.normal_impulse.to_bits(), b.normal_impulse.to_bits());
                assert_eq!(
                    a.tangent_impulse.map(f32::to_bits),
                    b.tangent_impulse.map(f32::to_bits)
                );
            }
            let mut v1 = views;
            let mut v2 = views;
            for _ in 0..10 {
                opt.solve_velocity(&mut v1, false);
                old.solve_velocity(&mut v2, false);
            }
            for (a, b) in v1.iter().zip(v2) {
                for (va, vb) in [(a.lin, b.lin), (a.ang, b.ang)] {
                    assert_eq!(
                        [va.x.to_bits(), va.y.to_bits(), va.z.to_bits()],
                        [vb.x.to_bits(), vb.y.to_bits(), vb.z.to_bits()]
                    );
                }
            }
            cache.rebuild(&opt.contacts);
        }
        assert_eq!(
            CACHE_PAIR_SEARCHES.get(),
            40,
            "four contacts require only one pair lookup per manifold"
        );
        cfg.warm_starting = false;
        opt.prepare(
            &views,
            &[material; 2],
            &[],
            std::slice::from_ref(&m),
            &cache,
            &cfg,
            1.0 / 60.0,
            false,
        );
        assert_eq!(CACHE_PAIR_SEARCHES.get(), 40);
        cfg.warm_starting = true;
        for p in &mut m.points {
            p.normal = Vec3::ZERO;
        }
        opt.prepare(
            &views,
            &[material; 2],
            &[],
            std::slice::from_ref(&m),
            &cache,
            &cfg,
            1.0 / 60.0,
            false,
        );
        assert!(opt.contacts.is_empty());
        assert_eq!(CACHE_PAIR_SEARCHES.get(), 40);
        m.points.clear();
        opt.prepare(
            &views,
            &[material; 2],
            &[],
            &[m],
            &cache,
            &cfg,
            1.0 / 60.0,
            false,
        );
        assert_eq!(CACHE_PAIR_SEARCHES.get(), 40);
    }
}

fn global_friction(c: &mut SolverContact, va: &mut BodyView, vb: &mut BodyView) {
    let mut vrel = pair_relative_velocity(va, vb, c.ra, c.rb);
    // 摩擦（用上一轮的法向冲量作为上界）
    let max_friction = c.friction * c.normal_impulse;
    // 各向异性时两个切向的上界不同；ratio=0 时两值逐位等于 friction，
    // 下面仍走原来的标量路径，旧场景/旧测试逐位不变。
    let anisotropic = c.friction_tangent[0].to_bits() != c.friction.to_bits()
        || c.friction_tangent[1].to_bits() != c.friction.to_bits();
    if c.friction_cone {
        // 圆形摩擦锥：两个切向方向一起投影，再按总量缩放。旧路径的
        // 逐轴 clamp 在对角线方向允许 √2·μ·N。
        if c.k_tangent[0] > 1e-12 && c.k_tangent[1] > 1e-12 {
            let d0 = -vrel.dot(c.tangent[0]) / c.k_tangent[0];
            let d1 = -vrel.dot(c.tangent[1]) / c.k_tangent[1];
            let mut next = [c.tangent_impulse[0] + d0, c.tangent_impulse[1] + d1];
            let len = (next[0] * next[0] + next[1] * next[1]).sqrt();
            // 各向异性 + 圆形锥没有单一 μ：取两个方向上限的平均值作为
            // 保守上界。真正沿椭圆锥的投影留给后续版本。
            let cone_limit = if anisotropic {
                (c.friction_tangent[0] + c.friction_tangent[1]) * 0.5 * c.normal_impulse
            } else {
                max_friction
            };
            if len > cone_limit {
                let scale = cone_limit / len;
                next[0] *= scale;
                next[1] *= scale;
            }
            for (i, n) in next.iter().enumerate() {
                let d = *n - c.tangent_impulse[i];
                c.tangent_impulse[i] = *n;
                if d != 0.0 {
                    if c.precomputed {
                        apply_precomputed(va, vb, c.tangent[i], c.tangent_arm[i], d);
                    } else {
                        pair_apply_impulse(va, vb, c.ra, c.rb, c.tangent[i] * d);
                    }
                }
            }
        }
    } else {
        for i in 0..2 {
            let k = c.k_tangent[i];
            if k <= 1e-12 {
                continue;
            }
            let vt = vrel.dot(c.tangent[i]);
            let mut d = -vt / k;
            let limit = if anisotropic {
                c.friction_tangent[i] * c.normal_impulse
            } else {
                max_friction
            };
            let new = clamp(c.tangent_impulse[i] + d, -limit, limit);
            d = new - c.tangent_impulse[i];
            c.tangent_impulse[i] = new;
            // 冲量没变时不需要再乘一遍/重算相对速度；结果逐位相同。
            if d != 0.0 {
                if c.precomputed {
                    apply_precomputed(va, vb, c.tangent[i], c.tangent_arm[i], d);
                } else {
                    pair_apply_impulse(va, vb, c.ra, c.rb, c.tangent[i] * d);
                }
                vrel = pair_relative_velocity(va, vb, c.ra, c.rb);
            }
        }
    }
}

fn normal_product(contacts: &[SolverContact], views: &[BodyView], p: &[f64]) -> Vec<f64> {
    #[cfg(test)]
    NORMAL_SETUP_COUNTS.with(|c| {
        let mut n = c.get();
        n[3] += 1;
        c.set(n);
    });
    let mut force = vec![(Vec3::ZERO, Vec3::ZERO); views.len()];
    for (c, value) in contacts.iter().zip(p) {
        let impulse = c.normal * (*value as f32);
        force[c.a].0 -= impulse;
        force[c.a].1 -= c.ra.cross(impulse);
        force[c.b].0 += impulse;
        force[c.b].1 += c.rb.cross(impulse);
    }
    for (v, (lin, ang)) in views.iter().zip(&mut force) {
        *lin *= v.inv_m;
        *ang = v.inv_i_world(*ang);
    }
    contacts
        .iter()
        .map(|c| {
            let (la, wa) = force[c.a];
            let (lb, wb) = force[c.b];
            f64::from(((lb + wb.cross(c.rb)) - (la + wa.cross(c.ra))).dot(c.normal))
        })
        .collect()
}

// Coarse aggregate columns are evaluated with exactly the same true-body wrench.
fn coarse_cross(a: &[usize], b: &[usize], cs: &[SolverContact], vs: &[BodyView]) -> f64 {
    let mut result = 0.0;
    for &i in a {
        for &j in b {
            let x = &cs[i];
            let y = &cs[j];
            for (body_x, sign_x, arm_x) in [(x.a, -1.0, x.ra), (x.b, 1.0, x.rb)] {
                for (body_y, sign_y, arm_y) in [(y.a, -1.0, y.ra), (y.b, 1.0, y.rb)] {
                    if body_x == body_y {
                        let v = vs[body_x];
                        result += sign_x
                            * sign_y
                            * (f64::from(v.inv_m * x.normal.dot(y.normal))
                                + f64::from(
                                    arm_x
                                        .cross(x.normal)
                                        .dot(v.inv_i_world(arm_y.cross(y.normal))),
                                ));
                    }
                }
            }
        }
    }
    result / (a.len() * b.len()) as f64
}

fn chain_precondition(
    cs: &[SolverContact],
    vs: &[BodyView],
    residual: &[f64],
    free: &[bool],
) -> Vec<f64> {
    #[cfg(test)]
    NORMAL_SETUP_COUNTS.with(|c| {
        let mut n = c.get();
        n[0] += 1;
        c.set(n);
    });
    let mut z: Vec<_> = cs
        .iter()
        .zip(residual)
        .zip(free)
        .map(|((c, r), f)| if *f { r / f64::from(c.k_normal) } else { 0.0 })
        .collect();
    let mut groups: std::collections::BTreeMap<(usize, usize), Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, c) in cs.iter().enumerate() {
        groups.entry((c.a, c.b)).or_default().push(i);
    }
    let mut adjacency = vec![Vec::new(); vs.len()];
    let edges: Vec<_> = groups.into_iter().collect();
    for (i, ((a, b), _)) in edges.iter().enumerate() {
        adjacency[*a].push(i);
        adjacency[*b].push(i);
    }
    let roots: Vec<_> = vs
        .iter()
        .enumerate()
        .filter(|(i, v)| v.is_static && adjacency[*i].len() == 1)
        .map(|(i, _)| i)
        .collect();
    #[cfg(test)]
    global_note(format!(
        "roots={} freecount={}",
        roots.len(),
        free.iter().filter(|f| **f).count()
    ));
    for root in roots {
        let mut node = root;
        let mut previous = usize::MAX;
        let mut columns = Vec::new();
        let mut seen = vec![false; vs.len()];
        let mut direction = None;
        let mut valid = true;
        loop {
            if seen[node] {
                #[cfg(test)]
                global_note("coarse_status=cycle");
                valid = false;
                break;
            }
            seen[node] = true;
            let next: Vec<_> = adjacency[node]
                .iter()
                .copied()
                .filter(|e| *e != previous)
                .collect();
            if next.is_empty() {
                break;
            }
            if next.len() != 1 {
                #[cfg(test)]
                global_note("coarse_status=branch");
                valid = false;
                break;
            }
            let edge = next[0];
            let ((a, b), points) = &edges[edge];
            let other = if *a == node { *b } else { *a };
            if vs[other].inv_m <= 0.0 {
                #[cfg(test)]
                global_note("coarse_status=frozenother");
                valid = false;
                break;
            }
            let n = cs[points[0]].normal * if *a == node { 1.0 } else { -1.0 };
            if direction.is_some_and(|d: Vec3| d.dot(n) < 0.999)
                || points.iter().any(|i| cs[*i].normal != cs[points[0]].normal)
            {
                #[cfg(test)]
                global_note(format!(
                    "coarse_status=normal_mismatch adjacent_dot={} exactnormalneq={}",
                    direction.map_or(1.0, |d: Vec3| d.dot(n)),
                    points.iter().any(|i| cs[*i].normal != cs[points[0]].normal)
                ));
                valid = false;
                break;
            }
            direction = Some(n);
            columns.push(points.clone());
            previous = edge;
            node = other;
        }
        if !valid || columns.is_empty() {
            continue;
        }
        let n = columns.len();
        let mut diagonal = vec![0.0; n];
        let mut lower = vec![0.0; n];
        let mut rhs = vec![0.0; n];
        for i in 0..n {
            diagonal[i] = coarse_cross(&columns[i], &columns[i], cs, vs);
            rhs[i] = columns[i]
                .iter()
                .filter(|j| free[**j])
                .map(|j| residual[*j])
                .sum::<f64>()
                / columns[i].len() as f64;
            if i > 0 {
                lower[i] = coarse_cross(&columns[i], &columns[i - 1], cs, vs);
            }
        }
        #[cfg(test)]
        NORMAL_SETUP_COUNTS.with(|c| {
            let mut n = c.get();
            n[1] += 1;
            c.set(n);
        });
        let scale = diagonal.iter().copied().fold(0.0, f64::max);
        for i in 0..n {
            if i > 0 {
                let off = lower[i];
                lower[i] /= diagonal[i - 1];
                diagonal[i] -= lower[i] * off;
                rhs[i] -= lower[i] * rhs[i - 1];
            }
            if !diagonal[i].is_finite() || diagonal[i] <= 1e-9 * scale {
                #[cfg(test)]
                global_note(format!("coarse_status=pivot ratio={}", diagonal[i] / scale));
                valid = false;
                break;
            }
        }
        if !valid {
            continue;
        }
        #[cfg(test)]
        global_note(format!(
            "coarse_status=accepted chainlen={n} minpivotratio={}",
            diagonal.iter().copied().fold(f64::INFINITY, f64::min) / scale
        ));
        for i in 0..n {
            rhs[i] /= diagonal[i];
        }
        for i in (0..n.saturating_sub(1)).rev() {
            rhs[i] -= lower[i + 1] * rhs[i + 1];
        }
        for i in 0..n {
            for j in &columns[i] {
                if free[*j] {
                    z[*j] += rhs[i] / columns[i].len() as f64;
                }
            }
        }
    }
    z
}

fn feasible_direction(
    lambda: &[f64],
    gradient: &[f64],
    mut p: Vec<f64>,
    diagonal: &[f64],
) -> Vec<f64> {
    #[cfg(test)]
    global_note(format!(
        "zero_clips={}",
        (0..p.len())
            .filter(|i| lambda[*i] == 0.0 && p[*i] < 0.0)
            .count()
    ));
    for i in 0..p.len() {
        if lambda[i] == 0.0 && p[i] < 0.0 {
            p[i] = 0.0;
        }
    }
    if p.iter().zip(gradient).map(|(p, g)| p * g).sum::<f64>() >= 0.0
        || p.iter().any(|x| !x.is_finite())
    {
        #[cfg(test)]
        global_note("diag_descent_fallback=true");
        for i in 0..p.len() {
            p[i] = if lambda[i] > 0.0 || gradient[i] < 0.0 {
                -gradient[i] / diagonal[i]
            } else {
                0.0
            };
        }
    }
    p
}

fn global_normal_round(
    cs: &mut [SolverContact],
    vs: &mut [BodyView],
    memory: &mut NormalDirectionMemory,
) {
    global_normal_attempt(cs, vs, memory);
    for c in cs.iter() {
        if let Some((va, vb)) = split_body_pair(vs, c.a, c.b) {
            global_rolling(c, va, vb);
        }
    }
}

fn global_normal_attempt(
    cs: &mut [SolverContact],
    vs: &mut [BodyView],
    memory: &mut NormalDirectionMemory,
) {
    global_normal_attempt_cached(cs, vs, memory, None);
}
fn global_normal_attempt_cached(
    cs: &mut [SolverContact],
    vs: &mut [BodyView],
    memory: &mut NormalDirectionMemory,
    cache: Option<&mut NormalGeometry>,
) {
    #[cfg(test)]
    {
        GLOBAL_TRACE.with(|t| {
            let (step, r) = t.get();
            t.set((step, r + 1));
        });
        global_observe(cs, vs, "before_friction");
    }
    for c in cs.iter_mut() {
        if let Some((a, b)) = split_body_pair(vs, c.a, c.b) {
            global_friction(c, a, b);
        }
    }
    #[cfg(test)]
    global_observe(cs, vs, "fresh_gradient");
    let lambda: Vec<_> = cs.iter().map(|c| f64::from(c.normal_impulse)).collect();
    let gradient: Vec<_> = cs
        .iter()
        .map(|c| {
            f64::from(
                pair_relative_velocity(&vs[c.a], &vs[c.b], c.ra, c.rb).dot(c.normal)
                    - c.restitution_bias
                    - c.speculative_bias,
            )
        })
        .collect();
    let free: Vec<_> = lambda
        .iter()
        .zip(&gradient)
        .map(|(l, g)| *l > 0.0 || *g < 0.0)
        .collect();
    let residual: Vec<_> = gradient.iter().map(|x| -x).collect();
    let diagonal: Vec<_> = cs.iter().map(|c| f64::from(c.k_normal)).collect();
    let preconditioned = if let Some(cache) = cache {
        cache.apply(cs, vs, &residual, &free).clone()
    } else {
        let mut preconditioned = chain_precondition(cs, vs, &residual, &free);
        for (z, local) in preconditioned
            .iter_mut()
            .zip(local_torque_correction(cs, vs, &residual, &free))
        {
            *z += local;
        }
        preconditioned
    };
    let (p, retain) =
        memory_feasible_direction(&lambda, &gradient, preconditioned, &diagonal, &free, memory);
    let descent = p.iter().zip(&gradient).map(|(p, g)| p * g).sum::<f64>();
    if !descent.is_finite() || descent >= 0.0 {
        memory.clear();
        return;
    }
    let ap = normal_product(cs, vs, &p);
    let curvature = p.iter().zip(&ap).map(|(p, a)| p * a).sum::<f64>();
    if !curvature.is_finite() || curvature <= 0.0 {
        memory.clear();
        return;
    }
    #[cfg(test)]
    global_note(format!(
        "descent={descent} curvature={curvature} alphaopt={} apcount=1",
        -descent / curvature
    ));
    let mut alpha = -descent / curvature;
    for i in 0..p.len() {
        if p[i] < 0.0 {
            alpha = alpha.min(-lambda[i] / p[i]);
        }
    }
    #[cfg(test)]
    global_note(format!(
        "alpha={alpha} bound_limited={}",
        alpha < -descent / curvature
    ));
    #[cfg(test)]
    {
        let limiting = (0..p.len())
            .filter(|i| p[*i] < 0.0)
            .min_by(|i, j| (-lambda[*i] / p[*i]).total_cmp(&(-lambda[*j] / p[*j])));
        if let Some(i) = limiting {
            global_note(format!(
                "limiter={i} pair={},{} lambda={} p={} g={} ratio={}",
                cs[i].a,
                cs[i].b,
                lambda[i],
                p[i],
                gradient[i],
                alpha / (-descent / curvature)
            ));
        }
    }
    if !alpha.is_finite() || alpha <= 0.0 {
        memory.clear();
        return;
    }
    let next = projected_line_update(&lambda, &p, alpha);
    if next.iter().any(|x| !x.is_finite()) {
        memory.clear();
        return;
    }
    let hitbound = (0..p.len()).any(|i| p[i] < 0.0 && alpha == -lambda[i] / p[i]);
    if retain && !hitbound {
        *memory = NormalDirectionMemory {
            #[cfg(test)]
            copy_commit: false,
            p: p.clone(),
            ap,
            curvature,
            free: free.clone(),
        };
    } else {
        memory.clear();
    }
    for (c, new) in cs.iter_mut().zip(next) {
        let delta = new - c.normal_impulse;
        c.normal_impulse = new;
        if delta != 0.0 {
            if let Some((a, b)) = split_body_pair(vs, c.a, c.b) {
                pair_apply_impulse(a, b, c.ra, c.rb, c.normal * delta);
            }
        }
    }
    #[cfg(test)]
    global_observe(cs, vs, "after_normal");
}

#[cfg(test)]
thread_local! { pub(crate) static GLOBAL_TRACE: std::cell::Cell<(usize,usize)> = const { std::cell::Cell::new((0,0)) }; }
#[cfg(test)]
fn global_note(message: impl std::fmt::Display) {
    GLOBAL_TRACE.with(|t| {
        let (step, round) = t.get();
        if (225..=245).contains(&step)
            || ((225..=245).contains(&step) && message.to_string().contains("coarse_status="))
        {
            println!("ROUND182 step={step} round={round} {message}");
        }
    });
}
#[cfg(test)]
fn global_observe(cs: &[SolverContact], vs: &[BodyView], phase: &str) {
    let kkt = cs
        .iter()
        .map(|c| {
            let g = pair_relative_velocity(&vs[c.a], &vs[c.b], c.ra, c.rb).dot(c.normal)
                - c.restitution_bias
                - c.speculative_bias;
            if c.normal_impulse > 0.0 {
                g.abs()
            } else {
                (-g).max(0.0)
            }
        })
        .fold(0.0, f32::max);
    let pair: Vec<_> = cs
        .iter()
        .filter(|c| c.a == 4 && c.b == 5)
        .map(|c| {
            (
                c.penetration,
                pair_relative_velocity(&vs[c.a], &vs[c.b], c.ra, c.rb).dot(c.normal),
                c.speculative_bias + c.restitution_bias,
                c.normal_impulse,
            )
        })
        .collect();
    global_note(format!("phase={phase} kkt={kkt} pair12={pair:?}"));
}

#[cfg(test)]
pub(crate) fn global_phase_observe(cs: &[SolverContact], vs: &[BodyView], phase: &str) {
    global_observe(cs, vs, phase);
}

fn wrench_basis(cs: &[SolverContact], points: &[usize]) -> Vec<Vec<f64>> {
    let n = cs[points[0]].normal;
    let (t1, t2) = n.any_orthonormal_pair();
    let raw: Vec<Vec<f64>> = vec![
        vec![1.0; points.len()],
        points
            .iter()
            .map(|i| f64::from(cs[*i].ra.cross(n).dot(t1)))
            .collect(),
        points
            .iter()
            .map(|i| f64::from(cs[*i].ra.cross(n).dot(t2)))
            .collect(),
    ];
    let mut result: Vec<Vec<f64>> = Vec::new();
    for mut v in raw {
        let original = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        if !original.is_finite() || original <= 1e-30 {
            continue;
        }
        for _ in 0..2 {
            for q in &result {
                let d = v.iter().zip(q).map(|(a, b)| a * b).sum::<f64>();
                for (i, x) in v.iter_mut().enumerate() {
                    *x -= d * q[i];
                }
            }
        }
        let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm.is_finite() && norm > original * 1e-9 {
            for x in &mut v {
                *x /= norm;
            }
            result.push(v);
        }
    }
    result
}

fn weighted_cross(
    a: &[usize],
    qa: &[f64],
    b: &[usize],
    qb: &[f64],
    cs: &[SolverContact],
    vs: &[BodyView],
) -> f64 {
    let mut value = 0.0;
    for (ii, &i) in a.iter().enumerate() {
        for (jj, &j) in b.iter().enumerate() {
            let x = cs[i];
            let y = cs[j];
            for (bx, sx, rx) in [(x.a, -1.0, x.ra), (x.b, 1.0, x.rb)] {
                for (by, sy, ry) in [(y.a, -1.0, y.ra), (y.b, 1.0, y.rb)] {
                    if bx == by {
                        #[cfg(test)]
                        LOCAL_KERNEL_CALLS.with(|c| c.set(c.get() + 1));
                        let v = vs[bx];
                        value += qa[ii]
                            * qb[jj]
                            * sx
                            * sy
                            * (f64::from(v.inv_m * x.normal.dot(y.normal))
                                + f64::from(
                                    rx.cross(x.normal).dot(v.inv_i_world(ry.cross(y.normal))),
                                ));
                    }
                }
            }
        }
    }
    value
}

// Fixed triangular index order is part of the numerical reference.
#[allow(clippy::needless_range_loop)]
fn small_spd_inverse(a: [[f64; 3]; 3], n: usize, scale: f64) -> Option<[[f64; 3]; 3]> {
    let mut l = [[0.0; 3]; 3];
    for i in 0..n {
        for j in 0..=i {
            let mut x = a[i][j];
            for k in 0..j {
                x -= l[i][k] * l[j][k];
            }
            if i == j {
                #[cfg(test)]
                global_note(format!(
                    "wrench_pivot row={i} ratio={} scale={scale}",
                    x / scale
                ));
                if !x.is_finite() || x <= scale * 1e-9 {
                    return None;
                }
                l[i][j] = x.sqrt();
            } else {
                l[i][j] = x / l[j][j];
            }
        }
    }
    let mut inv = [[0.0; 3]; 3];
    for col in 0..n {
        let mut x = [0.0; 3];
        for i in 0..n {
            let mut rhs = if i == col { 1.0 } else { 0.0 };
            for j in 0..i {
                rhs -= l[i][j] * x[j];
            }
            x[i] = rhs / l[i][i];
        }
        for i in (0..n).rev() {
            let mut rhs = x[i];
            for j in i + 1..n {
                rhs -= l[j][i] * inv[j][col];
            }
            inv[i][col] = rhs / l[i][i];
        }
    }
    Some(inv)
}

#[cfg(test)]
fn wrench_chain_correction(
    columns: &[Vec<usize>],
    cs: &[SolverContact],
    vs: &[BodyView],
    residual: &[f64],
    free: &[bool],
) -> Option<Vec<f64>> {
    let mut bases = Vec::new();
    for points in columns {
        let q = wrench_basis(cs, points);
        if q.is_empty() {
            return None;
        }
        bases.push(q);
    }
    let n = columns.len();
    let mut d = vec![[[0.0; 3]; 3]; n];
    let mut e = d.clone();
    let mut rhs = vec![[0.0; 3]; n];
    for i in 0..n {
        for a in 0..bases[i].len() {
            rhs[i][a] = columns[i]
                .iter()
                .enumerate()
                .filter(|(_, j)| free[**j])
                .map(|(j, index)| bases[i][a][j] * residual[*index])
                .sum();
            for b in 0..bases[i].len() {
                let ab =
                    weighted_cross(&columns[i], &bases[i][a], &columns[i], &bases[i][b], cs, vs);
                let ba =
                    weighted_cross(&columns[i], &bases[i][b], &columns[i], &bases[i][a], cs, vs);
                d[i][a][b] = 0.5 * (ab + ba);
            }
            if i > 0 {
                for b in 0..bases[i - 1].len() {
                    let ab = weighted_cross(
                        &columns[i],
                        &bases[i][a],
                        &columns[i - 1],
                        &bases[i - 1][b],
                        cs,
                        vs,
                    );
                    let ba = weighted_cross(
                        &columns[i - 1],
                        &bases[i - 1][b],
                        &columns[i],
                        &bases[i][a],
                        cs,
                        vs,
                    );
                    e[i][a][b] = 0.5 * (ab + ba);
                }
            }
        }
    }
    let scale = d
        .iter()
        .flat_map(|m| (0..3).map(move |i| m[i][i]))
        .fold(0.0, f64::max);
    let mut inverse = vec![[[0.0; 3]; 3]; n];
    for i in 0..n {
        if i > 0 {
            for a in 0..bases[i].len() {
                for b in 0..bases[i].len() {
                    let mut delta = 0.0;
                    for j in 0..bases[i - 1].len() {
                        for k in 0..bases[i - 1].len() {
                            delta += e[i][a][j] * inverse[i - 1][j][k] * e[i][b][k];
                        }
                    }
                    d[i][a][b] -= delta;
                }
                let mut delta = 0.0;
                for j in 0..bases[i - 1].len() {
                    for k in 0..bases[i - 1].len() {
                        delta += e[i][a][j] * inverse[i - 1][j][k] * rhs[i - 1][k];
                    }
                }
                rhs[i][a] -= delta;
            }
        }
        inverse[i] = small_spd_inverse(d[i], bases[i].len(), scale)?;
    }
    let mut x = vec![[0.0; 3]; n];
    for i in (0..n).rev() {
        let mut b = rhs[i];
        if i + 1 < n {
            for a in 0..bases[i].len() {
                for j in 0..bases[i + 1].len() {
                    b[a] -= e[i + 1][j][a] * x[i + 1][j];
                }
            }
        }
        for a in 0..bases[i].len() {
            for j in 0..bases[i].len() {
                x[i][a] += inverse[i][a][j] * b[j];
            }
        }
    }
    let mut out = vec![0.0; cs.len()];
    for i in 0..n {
        for (j, index) in columns[i].iter().enumerate() {
            if free[*index] {
                for a in 0..bases[i].len() {
                    out[*index] += bases[i][a][j] * x[i][a];
                }
            }
        }
    }
    Some(out)
}

fn projected_line_update(lambda: &[f64], p: &[f64], alpha: f64) -> Vec<f32> {
    lambda
        .iter()
        .zip(p)
        .map(|(l, p)| {
            if *p < 0.0 && alpha == -*l / *p {
                0.0
            } else {
                (l + alpha * p).max(0.0) as f32
            }
        })
        .collect()
}
fn local_torque_correction(
    cs: &[SolverContact],
    vs: &[BodyView],
    r: &[f64],
    free: &[bool],
) -> Vec<f64> {
    let mut groups: std::collections::BTreeMap<(usize, usize), Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, c) in cs.iter().enumerate() {
        groups.entry((c.a, c.b)).or_default().push(i);
    }
    let mut out = vec![0.0; cs.len()];
    for points in groups.values() {
        if points.iter().any(|i| cs[*i].normal != cs[points[0]].normal) {
            continue;
        }
        let basis = wrench_basis(cs, points);
        let rank = basis.len().saturating_sub(1);
        #[cfg(test)]
        global_note(format!(
            "local_pair={},{} local_rank={rank}",
            cs[points[0]].a, cs[points[0]].b
        ));
        if rank == 0 {
            continue;
        }
        let mut matrix = [[0.0; 3]; 3];
        let mut rhs = [0.0; 3];
        for a in 0..rank {
            rhs[a] = points
                .iter()
                .enumerate()
                .filter(|(_, i)| free[**i])
                .map(|(j, i)| basis[a + 1][j] * r[*i])
                .sum();
            for b in 0..rank {
                matrix[a][b] = 0.5
                    * (weighted_cross(points, &basis[a + 1], points, &basis[b + 1], cs, vs)
                        + weighted_cross(points, &basis[b + 1], points, &basis[a + 1], cs, vs));
            }
        }
        let scale = (0..rank).map(|i| matrix[i][i]).fold(0.0, f64::max);
        #[cfg(test)]
        NORMAL_SETUP_COUNTS.with(|c| {
            let mut n = c.get();
            n[2] += 1;
            c.set(n);
        });
        let Some(inverse) = small_spd_inverse(matrix, rank, scale) else {
            #[cfg(test)]
            global_note("local_status=badpivot");
            continue;
        };
        #[cfg(test)]
        global_note("local_status=accepted");
        let mut x = [0.0; 3];
        for a in 0..rank {
            for b in 0..rank {
                x[a] += inverse[a][b] * rhs[b];
            }
        }
        for (j, i) in points.iter().enumerate() {
            if free[*i] {
                for a in 0..rank {
                    out[*i] += basis[a + 1][j] * x[a];
                }
            }
        }
    }
    out
}

#[derive(Default, Clone, Debug)]
pub(crate) struct NormalDirectionMemory {
    #[cfg(test)]
    copy_commit: bool,
    p: Vec<f64>,
    ap: Vec<f64>,
    curvature: f64,
    free: Vec<bool>,
}
fn remembered_direction(mut z: Vec<f64>, memory: &NormalDirectionMemory) -> Vec<f64> {
    if memory.p.len() == z.len() && memory.curvature.is_finite() && memory.curvature > 0.0 {
        let beta = z.iter().zip(&memory.ap).map(|(a, b)| a * b).sum::<f64>() / memory.curvature;
        for (i, x) in z.iter_mut().enumerate() {
            *x -= beta * memory.p[i];
        }
    }
    z
}
fn memory_feasible_direction(
    lambda: &[f64],
    g: &[f64],
    z: Vec<f64>,
    diagonal: &[f64],
    free: &[bool],
    memory: &mut NormalDirectionMemory,
) -> (Vec<f64>, bool) {
    if memory.free != free {
        memory.clear();
    }
    let candidate = remembered_direction(z.clone(), memory);
    let clipped = (0..candidate.len()).any(|i| lambda[i] == 0.0 && candidate[i] < 0.0);
    let descent = candidate.iter().zip(g).map(|(a, b)| a * b).sum::<f64>();
    if clipped || candidate.iter().any(|x| !x.is_finite()) || !descent.is_finite() || descent >= 0.0
    {
        memory.clear();
        return (feasible_direction(lambda, g, z, diagonal), false);
    }
    (candidate, true)
}

fn global_rolling(c: &SolverContact, va: &mut BodyView, vb: &mut BodyView) {
    if c.rolling_friction > 0.0 && c.normal_impulse > 0.0 {
        let wrel = vb.ang - va.ang;
        // 标量近似 1/trace(invI_a + invI_b)，避免为每个接触求 3x3 逆。
        let k = va.inv_i_world_mat.cols[0].x
            + va.inv_i_world_mat.cols[1].y
            + va.inv_i_world_mat.cols[2].z
            + vb.inv_i_world_mat.cols[0].x
            + vb.inv_i_world_mat.cols[1].y
            + vb.inv_i_world_mat.cols[2].z;
        if k.is_finite() && k > 1e-9 {
            let limit = c.rolling_friction * c.normal_impulse * 0.5;
            let dl = (wrel * (-1.0 / k)).clamp_length(limit);
            pair_apply_angular_impulse(va, vb, dl);
        }
    }
}

#[cfg(test)]
thread_local! {static NORMAL_SETUP_COUNTS:std::cell::Cell<[usize;4]>=const {std::cell::Cell::new([0;4])};}

#[derive(Default, Clone, Debug)]
struct CachedForceChain {
    columns: Vec<Vec<usize>>,
    diagonal: Vec<f64>,
    lower: Vec<f64>,
    rhs: Vec<f64>,
}
#[derive(Clone, Debug)]
struct CachedLocal {
    points: Vec<usize>,
    basis: Vec<Vec<f64>>,
    inverse: [[f64; 3]; 3],
    rank: usize,
}
#[derive(Default, Clone, Debug)]
struct NormalGeometry {
    #[cfg(test)]
    force_reference: bool,
    #[cfg(test)]
    historical_local: bool,
    ready: bool,
    endpoint: Vec<NormalEndpoint>,
    chains: Vec<CachedForceChain>,
    locals: Vec<CachedLocal>,
    z: Vec<f64>,
    out: Vec<f64>,
}
impl NormalGeometry {
    fn build(&mut self, cs: &[SolverContact], vs: &[BodyView]) {
        #[cfg(test)]
        NORMAL_SETUP_COUNTS.with(|c| {
            let mut n = c.get();
            n[0] += 1;
            c.set(n);
        });
        #[cfg(test)]
        let force_reference = self.force_reference;
        #[cfg(not(test))]
        let force_reference = false;
        if force_reference {
            self.endpoint.clear();
            self.endpoint
                .extend(cs.iter().map(|c| NormalEndpoint::new(c, vs)));
        }
        self.chains.clear();
        self.locals.clear();
        let mut groups: std::collections::BTreeMap<(usize, usize), Vec<usize>> =
            std::collections::BTreeMap::new();
        for (i, c) in cs.iter().enumerate() {
            groups.entry((c.a, c.b)).or_default().push(i);
        }
        let mut adjacency = vec![Vec::new(); vs.len()];
        let edges: Vec<_> = groups.into_iter().collect();
        for (i, ((a, b), _)) in edges.iter().enumerate() {
            adjacency[*a].push(i);
            adjacency[*b].push(i);
        }
        let roots: Vec<_> = vs
            .iter()
            .enumerate()
            .filter(|(i, v)| v.is_static && adjacency[*i].len() == 1)
            .map(|(i, _)| i)
            .collect();
        #[cfg(test)]
        global_note(format!("roots={} freecount={}", roots.len(), 0));
        for root in roots {
            let mut node = root;
            let mut previous = usize::MAX;
            let mut columns = Vec::new();
            let mut seen = vec![false; vs.len()];
            let mut direction = None;
            let mut valid = true;
            loop {
                if seen[node] {
                    #[cfg(test)]
                    global_note("coarse_status=cycle");
                    valid = false;
                    break;
                }
                seen[node] = true;
                let next: Vec<_> = adjacency[node]
                    .iter()
                    .copied()
                    .filter(|e| *e != previous)
                    .collect();
                if next.is_empty() {
                    break;
                }
                if next.len() != 1 {
                    #[cfg(test)]
                    global_note("coarse_status=branch");
                    valid = false;
                    break;
                }
                let edge = next[0];
                let ((a, b), points) = &edges[edge];
                let other = if *a == node { *b } else { *a };
                if vs[other].inv_m <= 0.0 {
                    #[cfg(test)]
                    global_note("coarse_status=frozenother");
                    valid = false;
                    break;
                }
                let n = cs[points[0]].normal * if *a == node { 1.0 } else { -1.0 };
                if direction.is_some_and(|d: Vec3| d.dot(n) < 0.999)
                    || points.iter().any(|i| cs[*i].normal != cs[points[0]].normal)
                {
                    #[cfg(test)]
                    global_note(format!(
                        "coarse_status=normal_mismatch adjacent_dot={} exactnormalneq={}",
                        direction.map_or(1.0, |d: Vec3| d.dot(n)),
                        points.iter().any(|i| cs[*i].normal != cs[points[0]].normal)
                    ));
                    valid = false;
                    break;
                }
                direction = Some(n);
                columns.push(points.clone());
                previous = edge;
                node = other;
            }
            if !valid || columns.is_empty() {
                continue;
            }
            let n = columns.len();
            let mut diagonal = vec![0.0; n];
            let mut lower = vec![0.0; n];
            for i in 0..n {
                diagonal[i] = if force_reference {
                    coarse_cross_cached(&columns[i], &columns[i], cs, vs, &self.endpoint)
                } else {
                    translation_cross214(&cs[columns[i][0]], &cs[columns[i][0]], vs)
                };
                if i > 0 {
                    lower[i] = if force_reference {
                        coarse_cross_cached(&columns[i], &columns[i - 1], cs, vs, &self.endpoint)
                    } else {
                        translation_cross214(&cs[columns[i][0]], &cs[columns[i - 1][0]], vs)
                    };
                }
            }
            #[cfg(test)]
            NORMAL_SETUP_COUNTS.with(|c| {
                let mut n = c.get();
                n[1] += 1;
                c.set(n);
            });
            let scale = diagonal.iter().copied().fold(0.0, f64::max);
            for i in 0..n {
                if i > 0 {
                    let off = lower[i];
                    lower[i] /= diagonal[i - 1];
                    diagonal[i] -= lower[i] * off;
                }
                if !diagonal[i].is_finite() || diagonal[i] <= 1e-9 * scale {
                    #[cfg(test)]
                    global_note(format!("coarse_status=pivot ratio={}", diagonal[i] / scale));
                    valid = false;
                    break;
                }
            }
            if !valid {
                continue;
            }
            #[cfg(test)]
            global_note(format!(
                "coarse_status=accepted chainlen={n} minpivotratio={}",
                diagonal.iter().copied().fold(f64::INFINITY, f64::min) / scale
            ));
            self.chains.push(CachedForceChain {
                columns,
                diagonal,
                lower,
                rhs: vec![0.0; n],
            });
        }
        #[cfg(test)]
        let build_local = self.historical_local;
        #[cfg(not(test))]
        let build_local = false;
        if build_local {
            for (_, points) in &edges {
                if points.iter().any(|i| cs[*i].normal != cs[points[0]].normal) {
                    continue;
                }
                let basis = wrench_basis(cs, points);
                let rank = basis.len().saturating_sub(1);
                #[cfg(test)]
                global_note(format!(
                    "local_pair={},{} local_rank={rank}",
                    cs[points[0]].a, cs[points[0]].b
                ));
                if rank == 0 {
                    continue;
                }
                let kernel = LocalKernelCache::build_shared(points, cs, vs, &self.endpoint);
                let mut ordered = [[0.0; 3]; 3];
                for a in 0..rank {
                    for b in 0..rank {
                        ordered[a][b] = kernel.weighted(&basis[a + 1], &basis[b + 1]);
                    }
                }
                let mut matrix = [[0.0; 3]; 3];
                for a in 0..rank {
                    for b in 0..rank {
                        matrix[a][b] = 0.5 * (ordered[a][b] + ordered[b][a]);
                    }
                }
                let scale = (0..rank).map(|i| matrix[i][i]).fold(0.0, f64::max);
                #[cfg(test)]
                NORMAL_SETUP_COUNTS.with(|c| {
                    let mut n = c.get();
                    n[2] += 1;
                    c.set(n);
                });
                let Some(inverse) = small_spd_inverse(matrix, rank, scale) else {
                    #[cfg(test)]
                    global_note("local_status=badpivot");
                    continue;
                };
                #[cfg(test)]
                global_note("local_status=accepted");
                self.locals.push(CachedLocal {
                    points: points.clone(),
                    basis,
                    inverse,
                    rank,
                });
            }
        }
        self.ready = true;
    }
    fn apply<'a>(
        &'a mut self,
        cs: &[SolverContact],
        vs: &[BodyView],
        r: &[f64],
        free: &[bool],
    ) -> &'a Vec<f64> {
        if !self.ready {
            self.build(cs, vs);
        }
        self.z.clear();
        self.z.extend(cs.iter().zip(r).zip(free).map(|((c, r), f)| {
            if *f {
                r / f64::from(c.k_normal)
            } else {
                0.0
            }
        }));
        for chain in &mut self.chains {
            let CachedForceChain {
                columns,
                diagonal,
                lower,
                rhs,
            } = chain;
            let n = columns.len();
            for i in 0..n {
                rhs[i] = columns[i]
                    .iter()
                    .filter(|j| free[**j])
                    .map(|j| r[*j])
                    .sum::<f64>()
                    / columns[i].len() as f64;
                if i > 0 {
                    rhs[i] -= lower[i] * rhs[i - 1];
                }
            }
            for i in 0..n {
                rhs[i] /= diagonal[i];
            }
            for i in (0..n.saturating_sub(1)).rev() {
                rhs[i] -= lower[i + 1] * rhs[i + 1];
            }
            for i in 0..n {
                for j in &columns[i] {
                    if free[*j] {
                        self.z[*j] += rhs[i] / columns[i].len() as f64;
                    }
                }
            }
        }
        #[cfg(test)]
        let apply_local = self.historical_local;
        #[cfg(not(test))]
        let apply_local = false;
        if apply_local {
            self.out.clear();
            self.out.resize(cs.len(), 0.0);
            for local in &self.locals {
                let CachedLocal {
                    points,
                    basis,
                    inverse,
                    rank,
                } = local;
                let mut rhs = [0.0; 3];
                for a in 0..*rank {
                    rhs[a] = points
                        .iter()
                        .enumerate()
                        .filter(|(_, i)| free[**i])
                        .map(|(j, i)| basis[a + 1][j] * r[*i])
                        .sum();
                }
                let mut x = [0.0; 3];
                for a in 0..*rank {
                    for b in 0..*rank {
                        x[a] += inverse[a][b] * rhs[b];
                    }
                }
                for (j, i) in points.iter().enumerate() {
                    if free[*i] {
                        for a in 0..*rank {
                            self.out[*i] += basis[a + 1][j] * x[a];
                        }
                    }
                }
            }
            for (z, local) in self.z.iter_mut().zip(self.out.iter()) {
                *z += local;
            }
        }
        &self.z
    }
}
#[derive(Default, Clone, Debug)]
pub(crate) struct NormalSolveState {
    memory: NormalDirectionMemory,
    geometry: NormalGeometry,
    work: NormalWork,
}
impl NormalSolveState {
    pub(crate) fn reset(&mut self) {
        self.memory.clear();
        self.geometry.ready = false;
    }
}
impl NormalDirectionMemory {
    fn clear(&mut self) {
        self.p.clear();
        self.ap.clear();
        self.free.clear();
        self.curvature = 0.0;
    }
}
pub(crate) fn solve_velocity_contacts_state(
    cs: &mut [SolverContact],
    vs: &mut [BodyView],
    global: bool,
    state: &mut NormalSolveState,
) {
    if global {
        global_normal_attempt_work(
            cs,
            vs,
            &mut state.memory,
            &mut state.geometry,
            &mut state.work,
        );
        for c in cs {
            if let Some((a, b)) = split_body_pair(vs, c.a, c.b) {
                global_rolling(c, a, b);
            }
        }
    } else {
        solve_velocity_contacts_memory(cs, vs, false, &mut state.memory);
    }
}

#[derive(Default, Clone, Debug)]
struct NormalWork {
    lambda: Vec<f64>,
    gradient: Vec<f64>,
    free: Vec<bool>,
    residual: Vec<f64>,
    diagonal: Vec<f64>,
    z: Vec<f64>,
    p: Vec<f64>,
    ap: Vec<f64>,
    next: Vec<f32>,
    force: Vec<(Vec3, Vec3)>,
}
fn normal_product_into(
    contacts: &[SolverContact],
    views: &[BodyView],
    p: &[f64],
    force: &mut Vec<(Vec3, Vec3)>,
    out: &mut Vec<f64>,
) {
    #[cfg(test)]
    NORMAL_SETUP_COUNTS.with(|c| {
        let mut n = c.get();
        n[3] += 1;
        c.set(n);
    });
    force.clear();
    force.resize(views.len(), (Vec3::ZERO, Vec3::ZERO));
    for (c, value) in contacts.iter().zip(p) {
        let impulse = c.normal * (*value as f32);
        force[c.a].0 -= impulse;
        force[c.a].1 -= c.ra.cross(impulse);
        force[c.b].0 += impulse;
        force[c.b].1 += c.rb.cross(impulse);
    }
    for (v, (lin, ang)) in views.iter().zip(force.iter_mut()) {
        *lin *= v.inv_m;
        *ang = v.inv_i_world(*ang);
    }
    out.clear();
    out.extend(contacts.iter().map(|c| {
        let (la, wa) = force[c.a];
        let (lb, wb) = force[c.b];
        f64::from(((lb + wb.cross(c.rb)) - (la + wa.cross(c.ra))).dot(c.normal))
    }));
}

fn global_normal_attempt_work(
    cs: &mut [SolverContact],
    vs: &mut [BodyView],
    memory: &mut NormalDirectionMemory,
    cache: &mut NormalGeometry,
    work: &mut NormalWork,
) {
    #[cfg(test)]
    {
        GLOBAL_TRACE.with(|t| {
            let (step, r) = t.get();
            t.set((step, r + 1));
        });
        global_observe(cs, vs, "before_friction");
    }
    for c in cs.iter_mut() {
        if let Some((a, b)) = split_body_pair(vs, c.a, c.b) {
            global_friction(c, a, b);
        }
    }
    #[cfg(test)]
    global_observe(cs, vs, "fresh_gradient");
    let NormalWork {
        lambda,
        gradient,
        free,
        residual,
        diagonal,
        z,
        p,
        ap,
        next,
        force,
        ..
    } = work;
    lambda.clear();
    lambda.extend(cs.iter().map(|c| f64::from(c.normal_impulse)));
    gradient.clear();
    gradient.extend(cs.iter().map(|c| {
        f64::from(
            pair_relative_velocity(&vs[c.a], &vs[c.b], c.ra, c.rb).dot(c.normal)
                - c.restitution_bias
                - c.speculative_bias,
        )
    }));
    free.clear();
    free.extend(
        lambda
            .iter()
            .zip(gradient.iter())
            .map(|(l, g)| *l > 0.0 || *g < 0.0),
    );
    residual.clear();
    residual.extend(gradient.iter().map(|x| -x));
    diagonal.clear();
    diagonal.extend(cs.iter().map(|c| f64::from(c.k_normal)));
    z.clone_from(cache.apply(cs, vs, residual, free));
    if memory.free != *free {
        memory.clear();
    }
    p.clone_from(z);
    if memory.p.len() == p.len() && memory.curvature.is_finite() && memory.curvature > 0.0 {
        let beta = p.iter().zip(&memory.ap).map(|(a, b)| a * b).sum::<f64>() / memory.curvature;
        for (i, x) in p.iter_mut().enumerate() {
            *x -= beta * memory.p[i];
        }
    }
    let clipped = (0..p.len()).any(|i| lambda[i] == 0.0 && p[i] < 0.0);
    let d = p
        .iter()
        .zip(gradient.iter())
        .map(|(a, b)| a * b)
        .sum::<f64>();
    let retain = !(clipped || p.iter().any(|x| !x.is_finite()) || !d.is_finite() || d >= 0.0);
    if !retain {
        memory.clear();
        p.clone_from(z);
        for i in 0..p.len() {
            if lambda[i] == 0.0 && p[i] < 0.0 {
                p[i] = 0.0;
            }
        }
        if p.iter()
            .zip(gradient.iter())
            .map(|(p, g)| p * g)
            .sum::<f64>()
            >= 0.0
            || p.iter().any(|x| !x.is_finite())
        {
            for i in 0..p.len() {
                p[i] = if lambda[i] > 0.0 || gradient[i] < 0.0 {
                    -gradient[i] / diagonal[i]
                } else {
                    0.0
                };
            }
        }
    }
    let descent = if retain {
        d
    } else {
        p.iter()
            .zip(gradient.iter())
            .map(|(p, g)| p * g)
            .sum::<f64>()
    };
    if !descent.is_finite() || descent >= 0.0 {
        memory.clear();
        return;
    }
    normal_product_into(cs, vs, p, force, ap);
    let curvature = p.iter().zip(ap.iter()).map(|(p, a)| p * a).sum::<f64>();
    if !curvature.is_finite() || curvature <= 0.0 {
        memory.clear();
        return;
    }
    #[cfg(test)]
    global_note(format!(
        "descent={descent} curvature={curvature} alphaopt={} apcount=1",
        -descent / curvature
    ));
    let mut alpha = -descent / curvature;
    for i in 0..p.len() {
        if p[i] < 0.0 {
            alpha = alpha.min(-lambda[i] / p[i]);
        }
    }
    #[cfg(test)]
    global_note(format!(
        "alpha={alpha} bound_limited={}",
        alpha < -descent / curvature
    ));
    #[cfg(test)]
    {
        let limiting = (0..p.len())
            .filter(|i| p[*i] < 0.0)
            .min_by(|i, j| (-lambda[*i] / p[*i]).total_cmp(&(-lambda[*j] / p[*j])));
        if let Some(i) = limiting {
            global_note(format!(
                "limiter={i} pair={},{} lambda={} p={} g={} ratio={}",
                cs[i].a,
                cs[i].b,
                lambda[i],
                p[i],
                gradient[i],
                alpha / (-descent / curvature)
            ));
        }
    }
    if !alpha.is_finite() || alpha <= 0.0 {
        memory.clear();
        return;
    }
    let mut hitbound = false;
    next.clear();
    next.extend(lambda.iter().zip(p.iter()).map(|(l, p)| {
        if *p < 0.0 && alpha == -*l / *p {
            hitbound = true;
            0.0
        } else {
            (l + alpha * p).max(0.0) as f32
        }
    }));
    if next.iter().any(|x| !x.is_finite()) {
        memory.clear();
        return;
    }
    if retain && !hitbound {
        #[cfg(test)]
        let copy_commit = memory.copy_commit;
        #[cfg(not(test))]
        let copy_commit = false;
        if copy_commit {
            memory.p.clone_from(p);
            memory.ap.clone_from(ap);
        } else {
            std::mem::swap(&mut memory.p, p);
            std::mem::swap(&mut memory.ap, ap);
        }
        memory.curvature = curvature;
        memory.free.clone_from(free);
    } else {
        memory.clear();
    }
    for (c, new) in cs.iter_mut().zip(next.iter().copied()) {
        let delta = new - c.normal_impulse;
        c.normal_impulse = new;
        if delta != 0.0 {
            if let Some((a, b)) = split_body_pair(vs, c.a, c.b) {
                pair_apply_impulse(a, b, c.ra, c.rb, c.normal * delta);
            }
        }
    }
    #[cfg(test)]
    global_observe(cs, vs, "after_normal");
}

#[cfg(test)]
impl NormalWork {
    fn capacities(&self) -> [usize; 10] {
        [
            self.lambda.capacity(),
            self.gradient.capacity(),
            self.free.capacity(),
            self.residual.capacity(),
            self.diagonal.capacity(),
            self.z.capacity(),
            self.p.capacity(),
            self.ap.capacity(),
            self.next.capacity(),
            self.force.capacity(),
        ]
    }
}

#[cfg(test)]
thread_local! {static LOCAL_KERNEL_CALLS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};}

struct LocalKernelCache {
    n: usize,
    terms: Vec<[f64; 4]>,
    matches: Vec<u8>,
}
impl LocalKernelCache {
    #[cfg(test)]
    fn build(points: &[usize], cs: &[SolverContact], vs: &[BodyView]) -> Self {
        let endpoint: Vec<_> = cs.iter().map(|c| NormalEndpoint::new(c, vs)).collect();
        Self::build_shared(points, cs, vs, &endpoint)
    }
    fn build_shared(
        points: &[usize],
        cs: &[SolverContact],
        vs: &[BodyView],
        endpoint: &[NormalEndpoint],
    ) -> Self {
        let n = points.len();
        let mut terms = vec![[0.0; 4]; n * n];
        let mut matches = vec![0; n * n];
        for (ii, &i) in points.iter().enumerate() {
            for (jj, &j) in points.iter().enumerate() {
                let x = cs[i];
                let y = cs[j];
                for (ax, (bx, _)) in [(x.a, ()), (x.b, ())].into_iter().enumerate() {
                    for (ay, (by, _)) in [(y.a, ()), (y.b, ())].into_iter().enumerate() {
                        if bx == by {
                            #[cfg(test)]
                            LOCAL_KERNEL_CALLS.with(|c| c.set(c.get() + 1));
                            let v = vs[bx];
                            let slot = ax * 2 + ay;
                            matches[ii * n + jj] |= 1 << slot;
                            terms[ii * n + jj][slot] = f64::from(v.inv_m * x.normal.dot(y.normal))
                                + f64::from(endpoint[i].torque[ax].dot(endpoint[j].response[ay]));
                        }
                    }
                }
            }
        }
        Self { n, terms, matches }
    }
    fn weighted(&self, qa: &[f64], qb: &[f64]) -> f64 {
        let mut value = 0.0;
        for (ii, &qa) in qa.iter().enumerate().take(self.n) {
            for (jj, &qb) in qb.iter().enumerate().take(self.n) {
                for (ax, sx) in [-1.0, 1.0].into_iter().enumerate() {
                    for (ay, sy) in [-1.0, 1.0].into_iter().enumerate() {
                        let slot = ax * 2 + ay;
                        if self.matches[ii * self.n + jj] & (1 << slot) != 0 {
                            value += qa * qb * sx * sy * self.terms[ii * self.n + jj][slot];
                        }
                    }
                }
            }
        }
        value
    }
}

#[derive(Clone, Debug)]
struct NormalEndpoint {
    torque: [Vec3; 2],
    response: [Vec3; 2],
}
impl NormalEndpoint {
    fn new(c: &SolverContact, vs: &[BodyView]) -> Self {
        let torque = [c.ra.cross(c.normal), c.rb.cross(c.normal)];
        let response = [
            vs[c.a].inv_i_world(torque[0]),
            vs[c.b].inv_i_world(torque[1]),
        ];
        Self { torque, response }
    }
}
fn coarse_cross_cached(
    a: &[usize],
    b: &[usize],
    cs: &[SolverContact],
    vs: &[BodyView],
    endpoint: &[NormalEndpoint],
) -> f64 {
    let mut result = 0.0;
    for &i in a {
        for &j in b {
            let x = &cs[i];
            let y = &cs[j];
            for (ax, (body_x, sign_x)) in [(x.a, -1.0), (x.b, 1.0)].into_iter().enumerate() {
                for (ay, (body_y, sign_y)) in [(y.a, -1.0), (y.b, 1.0)].into_iter().enumerate() {
                    if body_x == body_y {
                        let v = vs[body_x];
                        result += sign_x
                            * sign_y
                            * (f64::from(v.inv_m * x.normal.dot(y.normal))
                                + f64::from(endpoint[i].torque[ax].dot(endpoint[j].response[ay])));
                    }
                }
            }
        }
    }
    result / (a.len() * b.len()) as f64
}

fn translation_cross214(x: &SolverContact, y: &SolverContact, vs: &[BodyView]) -> f64 {
    let n = x.normal;
    let m = y.normal;
    let dot = (f64::from(n.x) * f64::from(m.x) + f64::from(n.y) * f64::from(m.y))
        + f64::from(n.z) * f64::from(m.z);
    let mut t = 0.0;
    for (a, sa) in [(x.a, -1.0), (x.b, 1.0)] {
        for (b, sb) in [(y.a, -1.0), (y.b, 1.0)] {
            if a == b {
                t += f64::from(vs[a].inv_m) * dot * sa * sb;
            }
        }
    }
    t
}
